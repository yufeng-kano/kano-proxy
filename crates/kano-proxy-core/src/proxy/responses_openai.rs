//! OpenAI Responses API ↔ Chat Completions (apps/api/src/proxy/responses_openai.ts,
//! docs/api.md § `POST /openai/v1/responses`). The conversion path for every non-codex
//! target: a Responses request becomes the internal Chat shape, dispatches like
//! `/openai/v1/chat/completions`, and the Chat result becomes Responses events / a
//! Response object. Also the small in-stream error rewrite the native codex path needs so
//! dispatch's OpenAI-shaped error frames reach a Responses client as `response.failed`.
//!
//! Streaming conversion is incremental: the input byte stream is split into SSE lines as it
//! arrives and each line's events are emitted before the next line is pulled, so nothing is
//! buffered beyond the item currently open (docs/api.md § Streaming). Rust streams are
//! pull-based, so the TypeScript `backpressuredStream` wrapper has no counterpart here.

use std::collections::{HashMap, VecDeque};
use std::io;

use bytes::Bytes;
use futures::stream::{Stream, StreamExt};
use serde_json::{json, Map, Value};

use crate::app::now_ms;
use crate::ids::new_id;

/// A request field the proxy cannot honour (needs server-side state, or an unconvertible
/// part). Route → `400 unsupported_field`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct UnsupportedResponsesField {
    pub field: String,
    pub message: String,
}

impl UnsupportedResponsesField {
    fn new(field: &str, message: &str) -> Self {
        Self { field: field.to_string(), message: message.to_string() }
    }
}

/// How a flattened Chat function name maps back to the Responses tool the client declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponsesToolKind {
    Function,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponsesToolRef {
    pub kind: ResponsesToolKind,
    /// Present for a tool that came out of a `namespace` group — echoed on the call item.
    pub namespace: Option<String>,
    /// The client's own (unflattened) tool name.
    pub name: String,
}

impl ResponsesToolRef {
    pub fn function(name: &str) -> Self {
        Self { kind: ResponsesToolKind::Function, namespace: None, name: name.to_string() }
    }
    pub fn custom(name: &str) -> Self {
        Self { kind: ResponsesToolKind::Custom, namespace: None, name: name.to_string() }
    }
    pub fn namespaced(self, namespace: &str) -> Self {
        Self { namespace: Some(namespace.to_string()), ..self }
    }
    fn is_custom(&self) -> bool {
        self.kind == ResponsesToolKind::Custom
    }
}

pub type ResponsesToolNames = HashMap<String, ResponsesToolRef>;

#[derive(Debug, Clone)]
pub struct ResponsesToChatResult {
    /// Chat Completions-shaped body — the named fields dispatch reads, and the `rawBody` a
    /// custom-openai adapter forwards.
    pub chat: Map<String, Value>,
    pub tool_names: ResponsesToolNames,
}

pub const WEB_SEARCH_STUB_NAME: &str = "web_search";

/// Stub that replaces a hosted `web_search` tool on the conversion path (docs/api.md "Web
/// search on the conversion path").
pub fn web_search_stub_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": WEB_SEARCH_STUB_NAME,
            "description": "Web search is NOT available for this model through this proxy. Do not call this tool; it always fails. Answer from your own knowledge and the tools that do work.",
            "parameters": {
                "type": "object",
                "properties": { "query": { "type": "string", "description": "Unused." } },
                "additionalProperties": false
            }
        }
    })
}

const NAMESPACE_SEPARATOR: &str = "__";
const GEMINI_THOUGHT_SIGNATURE_MARKER_PREFIX: &str = "kano-proxy:gemini-thought-signature:v1:";

fn as_object(v: Option<&Value>) -> Option<&Map<String, Value>> {
    v.and_then(|v| v.as_object())
}

fn str_of(v: Option<&Value>) -> Option<&str> {
    v.and_then(|v| v.as_str())
}

/// JavaScript `String(x)` for the values a request body can carry.
fn js_string(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

fn text_of_parts(content: Option<&Value>, joiner: &str) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => {
            let mut out: Vec<String> = Vec::new();
            for part in parts {
                if let Value::String(s) = part {
                    out.push(s.clone());
                    continue;
                }
                let Some(part) = part.as_object() else { continue };
                let ty = str_of(part.get("type")).unwrap_or("");
                if matches!(ty, "input_text" | "output_text" | "text") {
                    if let Some(text) = str_of(part.get("text")) {
                        out.push(text.to_string());
                    }
                } else if ty == "refusal" {
                    if let Some(refusal) = str_of(part.get("refusal")) {
                        out.push(refusal.to_string());
                    }
                }
            }
            out.join(joiner)
        }
        _ => String::new(),
    }
}

/// User-role content: text and images become Chat parts; anything else is a hard reject
/// rather than a silent drop.
fn user_content_parts(content: Option<&Value>) -> Result<Value, UnsupportedResponsesField> {
    match content {
        Some(Value::String(s)) => return Ok(Value::String(s.clone())),
        Some(Value::Array(_)) => {}
        _ => return Ok(Value::String(String::new())),
    }
    let raw_parts = content.and_then(|c| c.as_array()).expect("array checked above");
    let mut parts: Vec<Value> = Vec::new();
    for part in raw_parts {
        if let Value::String(s) = part {
            parts.push(json!({ "type": "text", "text": s }));
            continue;
        }
        let Some(part) = part.as_object() else { continue };
        let ty = str_of(part.get("type")).unwrap_or("");
        if matches!(ty, "input_text" | "text") {
            if let Some(text) = str_of(part.get("text")) {
                parts.push(json!({ "type": "text", "text": text }));
            }
        } else if ty == "input_image" {
            let url = match part.get("image_url") {
                Some(Value::String(s)) => Some(s.clone()),
                Some(Value::Object(o)) => str_of(o.get("url")).map(|s| s.to_string()),
                _ => None,
            };
            let Some(url) = url.filter(|u| !u.is_empty()) else {
                return Err(UnsupportedResponsesField::new(
                    "input.content.input_image",
                    "input_image needs an image_url (data: URL or https URL); file_id references are not supported",
                ));
            };
            let mut image = Map::new();
            image.insert("url".into(), Value::String(url));
            if let Some(detail) = str_of(part.get("detail")) {
                if detail != "auto" {
                    image.insert("detail".into(), Value::String(detail.to_string()));
                }
            }
            parts.push(json!({ "type": "image_url", "image_url": Value::Object(image) }));
        } else if ty == "input_file" || ty == "input_audio" {
            return Err(UnsupportedResponsesField::new(
                &format!("input.content.{ty}"),
                &format!("{ty} content parts are not supported on this endpoint"),
            ));
        }
    }
    if parts.len() == 1 {
        if let Some(only) = parts[0].as_object() {
            if str_of(only.get("type")) == Some("text") {
                if let Some(text) = str_of(only.get("text")) {
                    return Ok(Value::String(text.to_string()));
                }
            }
        }
    }
    Ok(Value::Array(parts))
}

fn custom_call_arguments(input: Option<&Value>) -> String {
    let text = match input {
        Some(Value::String(s)) => s.clone(),
        other => js_string(other),
    };
    json!({ "input": text }).to_string()
}

fn encode_gemini_thought_signature(call_id: &str, signature: &str) -> String {
    format!(
        "{GEMINI_THOUGHT_SIGNATURE_MARKER_PREFIX}{}",
        json!({ "call_id": call_id, "signature": signature })
    )
}

fn decode_gemini_thought_signature(value: Option<&Value>) -> Option<(String, String)> {
    let text = value?.as_str()?;
    let payload = text.strip_prefix(GEMINI_THOUGHT_SIGNATURE_MARKER_PREFIX)?;
    let parsed: Value = serde_json::from_str(payload).ok()?;
    let obj = parsed.as_object()?;
    let call_id = str_of(obj.get("call_id"))?;
    let signature = str_of(obj.get("signature"))?;
    if call_id.is_empty() || signature.is_empty() {
        return None;
    }
    Some((call_id.to_string(), signature.to_string()))
}

fn gemini_thought_signature_item(call_id: &str, signature: &str) -> Value {
    json!({
        "id": new_id("rs"),
        "type": "reasoning",
        "summary": [],
        "encrypted_content": encode_gemini_thought_signature(call_id, signature),
    })
}

/// Responses request → Chat Completions request. Returns `UnsupportedResponsesField` for
/// the fields the proxy cannot honour.
pub fn responses_to_chat_request(
    body: &Map<String, Value>,
) -> Result<ResponsesToChatResult, UnsupportedResponsesField> {
    if let Some(prev) = str_of(body.get("previous_response_id")) {
        if !prev.is_empty() {
            return Err(UnsupportedResponsesField::new(
                "previous_response_id",
                "previous_response_id is not supported: this proxy stores no responses (store is always false); send the full input instead",
            ));
        }
    }
    if !matches!(body.get("conversation"), None | Some(Value::Null)) {
        return Err(UnsupportedResponsesField::new(
            "conversation",
            "conversation is not supported: this proxy keeps no server-side conversation state",
        ));
    }
    if body.get("background") == Some(&Value::Bool(true)) {
        return Err(UnsupportedResponsesField::new(
            "background",
            "background responses are not supported: nothing is stored to poll later",
        ));
    }

    let mut messages: Vec<Value> = Vec::new();
    if let Some(instructions) = str_of(body.get("instructions")) {
        if !instructions.is_empty() {
            messages.push(json!({ "role": "system", "content": instructions }));
        }
    }

    // Consecutive `function_call` items — and an assistant text immediately before them —
    // fold into one assistant message, which is the only shape Chat Completions has for
    // "the assistant said this and called these".
    struct Pending {
        content: Value,
        tool_calls: Vec<Value>,
    }
    let mut pending: Option<Pending> = None;
    let mut thought_signatures: HashMap<String, String> = HashMap::new();

    fn flush(pending: &mut Option<Pending>, messages: &mut Vec<Value>) {
        let Some(cur) = pending.take() else { return };
        let mut msg = Map::new();
        msg.insert("role".into(), json!("assistant"));
        msg.insert("content".into(), cur.content);
        if !cur.tool_calls.is_empty() {
            msg.insert("tool_calls".into(), Value::Array(cur.tool_calls));
        }
        messages.push(Value::Object(msg));
    }

    let owned_items: Vec<Value> = match body.get("input") {
        Some(Value::String(s)) => vec![json!({ "type": "message", "role": "user", "content": s })],
        Some(Value::Array(items)) => items.clone(),
        _ => Vec::new(),
    };

    for raw in &owned_items {
        let Some(raw) = raw.as_object() else { continue };
        let ty = match str_of(raw.get("type")) {
            Some(t) => t.to_string(),
            None => {
                if raw.contains_key("role") {
                    "message".to_string()
                } else {
                    String::new()
                }
            }
        };
        match ty.as_str() {
            "message" => {
                let role = match raw.get("role") {
                    None | Some(Value::Null) => "user".to_string(),
                    other => js_string(other),
                };
                if role == "assistant" {
                    flush(&mut pending, &mut messages);
                    pending = Some(Pending {
                        content: Value::String(text_of_parts(raw.get("content"), "")),
                        tool_calls: Vec::new(),
                    });
                    continue;
                }
                flush(&mut pending, &mut messages);
                if role == "system" || role == "developer" {
                    messages.push(json!({
                        "role": "system",
                        "content": text_of_parts(raw.get("content"), "\n\n"),
                    }));
                } else {
                    let content = user_content_parts(raw.get("content"))?;
                    let mut msg = Map::new();
                    msg.insert("role".into(), json!("user"));
                    msg.insert("content".into(), content);
                    messages.push(Value::Object(msg));
                }
            }
            "reasoning" => {
                if let Some((call_id, signature)) =
                    decode_gemini_thought_signature(raw.get("encrypted_content"))
                {
                    thought_signatures.insert(call_id, signature);
                }
            }
            "function_call" | "custom_tool_call" => {
                let call_id = match raw.get("call_id") {
                    None | Some(Value::Null) => js_string(raw.get("id")),
                    other => js_string(other),
                };
                let mut function = Map::new();
                if ty == "function_call" {
                    let name = str_of(raw.get("name")).unwrap_or("");
                    let flat = match str_of(raw.get("namespace")) {
                        Some(ns) if !ns.is_empty() => format!("{ns}{NAMESPACE_SEPARATOR}{name}"),
                        _ => name.to_string(),
                    };
                    function.insert("name".into(), Value::String(flat));
                    let args = match raw.get("arguments") {
                        Some(Value::String(s)) => s.clone(),
                        None | Some(Value::Null) => "{}".to_string(),
                        Some(other) => other.to_string(),
                    };
                    function.insert("arguments".into(), Value::String(args));
                } else {
                    function.insert(
                        "name".into(),
                        Value::String(str_of(raw.get("name")).unwrap_or("").to_string()),
                    );
                    function
                        .insert("arguments".into(), Value::String(custom_call_arguments(raw.get("input"))));
                }
                let mut call = Map::new();
                call.insert("id".into(), Value::String(call_id.clone()));
                call.insert("type".into(), json!("function"));
                call.insert("function".into(), Value::Object(function));
                if let Some(signature) = thought_signatures.remove(&call_id) {
                    call.insert("thought_signature".into(), Value::String(signature));
                }
                if pending.is_none() {
                    pending = Some(Pending { content: Value::Null, tool_calls: Vec::new() });
                }
                pending.as_mut().expect("just created").tool_calls.push(Value::Object(call));
            }
            "function_call_output" | "custom_tool_call_output" => {
                flush(&mut pending, &mut messages);
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": js_string(raw.get("call_id")),
                    "content": text_of_parts(raw.get("output"), "\n"),
                }));
            }
            "item_reference" => {
                return Err(UnsupportedResponsesField::new(
                    "input.item_reference",
                    "item_reference input items need stored responses, which this proxy does not keep",
                ))
            }
            // Hosted tool calls (web_search_call, …) and unknown future item types:
            // nothing on the Chat wire can carry them.
            _ => {}
        }
    }
    flush(&mut pending, &mut messages);

    let mut tool_names: ResponsesToolNames = HashMap::new();
    let mut tools: Vec<Value> = Vec::new();
    let mut web_search_stubbed = false;

    fn add_function(
        tools: &mut Vec<Value>,
        tool_names: &mut ResponsesToolNames,
        flat: &str,
        tool_ref: ResponsesToolRef,
        description: Option<&Value>,
        parameters: Option<Value>,
        strict: Option<&Value>,
    ) {
        let mut function = Map::new();
        function.insert("name".into(), Value::String(flat.to_string()));
        if let Some(Value::String(d)) = description {
            function.insert("description".into(), Value::String(d.clone()));
        }
        match parameters {
            Some(Value::Null) | None => {}
            Some(p) => {
                function.insert("parameters".into(), p);
            }
        }
        if let Some(Value::Bool(s)) = strict {
            function.insert("strict".into(), Value::Bool(*s));
        }
        tools.push(json!({ "type": "function", "function": Value::Object(function) }));
        tool_names.insert(flat.to_string(), tool_ref);
    }

    fn custom_parameters() -> Value {
        json!({
            "type": "object",
            "properties": { "input": { "type": "string", "description": "The raw text input for this tool." } },
            "required": ["input"],
            "additionalProperties": false
        })
    }

    if let Some(Value::Array(declared)) = body.get("tools") {
        for tool in declared {
            let Some(tool) = tool.as_object() else { continue };
            match str_of(tool.get("type")).unwrap_or("") {
                "function" => {
                    let name = str_of(tool.get("name")).unwrap_or("");
                    if name.is_empty() {
                        continue;
                    }
                    add_function(
                        &mut tools,
                        &mut tool_names,
                        name,
                        ResponsesToolRef::function(name),
                        tool.get("description"),
                        tool.get("parameters").cloned(),
                        tool.get("strict"),
                    );
                }
                "custom" => {
                    let name = str_of(tool.get("name")).unwrap_or("");
                    if name.is_empty() {
                        continue;
                    }
                    add_function(
                        &mut tools,
                        &mut tool_names,
                        name,
                        ResponsesToolRef::custom(name),
                        tool.get("description"),
                        Some(custom_parameters()),
                        None,
                    );
                }
                "namespace" => {
                    let ns = str_of(tool.get("name")).unwrap_or("").to_string();
                    let Some(Value::Array(inner_tools)) = tool.get("tools") else { continue };
                    if ns.is_empty() {
                        continue;
                    }
                    for inner in inner_tools {
                        let Some(inner) = inner.as_object() else { continue };
                        let name = str_of(inner.get("name")).unwrap_or("");
                        if name.is_empty() {
                            continue;
                        }
                        let flat = format!("{ns}{NAMESPACE_SEPARATOR}{name}");
                        if str_of(inner.get("type")) == Some("custom") {
                            add_function(
                                &mut tools,
                                &mut tool_names,
                                &flat,
                                ResponsesToolRef::custom(name).namespaced(&ns),
                                inner.get("description"),
                                Some(custom_parameters()),
                                None,
                            );
                        } else {
                            add_function(
                                &mut tools,
                                &mut tool_names,
                                &flat,
                                ResponsesToolRef::function(name).namespaced(&ns),
                                inner.get("description"),
                                inner.get("parameters").cloned(),
                                inner.get("strict"),
                            );
                        }
                    }
                }
                "web_search" | "web_search_preview" | "web_search_preview_2025_03_11" => {
                    web_search_stubbed = true;
                }
                // image_generation, file_search, code_interpreter, computer_use_preview,
                // mcp, local_shell, shell: hosted tools no Chat upstream can execute.
                _ => {}
            }
        }
    }
    if web_search_stubbed && !tool_names.contains_key(WEB_SEARCH_STUB_NAME) {
        tools.push(web_search_stub_tool());
        tool_names.insert(WEB_SEARCH_STUB_NAME.to_string(), ResponsesToolRef::function(WEB_SEARCH_STUB_NAME));
    }

    let mut chat = Map::new();
    if let Some(model) = body.get("model") {
        chat.insert("model".into(), model.clone());
    }
    chat.insert("messages".into(), Value::Array(messages));
    chat.insert("stream".into(), Value::Bool(body.get("stream") == Some(&Value::Bool(true))));
    let has_tools = !tools.is_empty();
    if has_tools {
        chat.insert("tools".into(), Value::Array(tools));
    }

    if has_tools {
        match body.get("tool_choice") {
            Some(Value::String(s)) if s == "auto" || s == "none" || s == "required" => {
                chat.insert("tool_choice".into(), Value::String(s.clone()));
            }
            Some(Value::Object(choice))
                if str_of(choice.get("type")) == Some("function")
                    && choice.get("name").map(|n| n.is_string()).unwrap_or(false) =>
            {
                let name = str_of(choice.get("name")).unwrap_or("");
                let flat = match str_of(choice.get("namespace")) {
                    Some(ns) if !ns.is_empty() => format!("{ns}{NAMESPACE_SEPARATOR}{name}"),
                    _ => name.to_string(),
                };
                chat.insert(
                    "tool_choice".into(),
                    json!({ "type": "function", "function": { "name": flat } }),
                );
            }
            None | Some(Value::Null) => {}
            // allowed_tools / hosted-tool choices: the closest Chat has is auto.
            Some(_) => {
                chat.insert("tool_choice".into(), json!("auto"));
            }
        }
    }

    let format = as_object(body.get("text")).and_then(|t| as_object(t.get("format")));
    match format.and_then(|f| str_of(f.get("type"))) {
        Some("json_schema") => {
            let format = format.expect("checked");
            let mut schema = Map::new();
            let name = str_of(format.get("name")).filter(|n| !n.is_empty()).unwrap_or("response");
            schema.insert("name".into(), Value::String(name.to_string()));
            if let Some(s) = format.get("schema") {
                schema.insert("schema".into(), s.clone());
            }
            if let Some(Value::Bool(strict)) = format.get("strict") {
                schema.insert("strict".into(), Value::Bool(*strict));
            }
            if let Some(Value::String(description)) = format.get("description") {
                schema.insert("description".into(), Value::String(description.clone()));
            }
            chat.insert(
                "response_format".into(),
                json!({ "type": "json_schema", "json_schema": Value::Object(schema) }),
            );
        }
        Some("json_object") => {
            chat.insert("response_format".into(), json!({ "type": "json_object" }));
        }
        _ => {}
    }

    if let Some(reasoning) = as_object(body.get("reasoning")) {
        match reasoning.get("effort") {
            None | Some(Value::Null) => {}
            Some(effort) => {
                chat.insert("reasoning_effort".into(), effort.clone());
            }
        }
    }
    if let Some(Value::Number(n)) = body.get("max_output_tokens") {
        chat.insert("max_tokens".into(), Value::Number(n.clone()));
    }
    if let Some(Value::Number(n)) = body.get("temperature") {
        chat.insert("temperature".into(), Value::Number(n.clone()));
    }
    if let Some(Value::Number(n)) = body.get("top_p") {
        chat.insert("top_p".into(), Value::Number(n.clone()));
    }
    if let Some(key) = str_of(body.get("prompt_cache_key")) {
        if !key.is_empty() {
            chat.insert("prompt_cache_key".into(), Value::String(key.to_string()));
        }
    }
    if let Some(Value::Bool(parallel)) = body.get("parallel_tool_calls") {
        chat.insert("parallel_tool_calls".into(), Value::Bool(*parallel));
    }

    Ok(ResponsesToChatResult { chat, tool_names })
}

// ---------------------------------------------------------------------------
// Chat → Responses output
// ---------------------------------------------------------------------------

fn now_seconds() -> i64 {
    now_ms().div_euclid(1000)
}

/// Chat `usage` → Responses `usage`; detail fields only when the upstream reported them
/// (absent means unreported, never 0).
pub fn chat_usage_to_responses(usage: Option<&Value>) -> Option<Value> {
    let u = usage?.as_object()?;
    let input = u.get("prompt_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
    let output = u.get("completion_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
    let mut out = Map::new();
    out.insert("input_tokens".into(), json!(input));
    out.insert("output_tokens".into(), json!(output));
    out.insert("total_tokens".into(), json!(input + output));
    if let Some(cached) =
        as_object(u.get("prompt_tokens_details")).and_then(|d| d.get("cached_tokens")).and_then(|v| v.as_i64())
    {
        out.insert("input_tokens_details".into(), json!({ "cached_tokens": cached }));
    }
    if let Some(reasoning) = as_object(u.get("completion_tokens_details"))
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(|v| v.as_i64())
    {
        out.insert("output_tokens_details".into(), json!({ "reasoning_tokens": reasoning }));
    }
    Some(Value::Object(out))
}

fn parse_custom_input(args: &str) -> String {
    if let Ok(Value::Object(parsed)) = serde_json::from_str::<Value>(args) {
        if let Some(input) = str_of(parsed.get("input")) {
            return input.to_string();
        }
    }
    args.to_string()
}

/// The completed call item for a Chat tool call, mapped back to the tool the client declared.
fn tool_call_item(
    id: &str,
    call_id: &str,
    flat_name: &str,
    args: &str,
    tool_names: &ResponsesToolNames,
) -> Value {
    let tool_ref = tool_names.get(flat_name);
    if let Some(tool_ref) = tool_ref.filter(|r| r.is_custom()) {
        return json!({
            "id": id,
            "type": "custom_tool_call",
            "call_id": call_id,
            "name": tool_ref.name,
            "input": parse_custom_input(args),
            "status": "completed",
        });
    }
    let mut item = Map::new();
    item.insert("id".into(), json!(id));
    item.insert("type".into(), json!("function_call"));
    item.insert("call_id".into(), json!(call_id));
    item.insert(
        "name".into(),
        json!(tool_ref.map(|r| r.name.as_str()).unwrap_or(flat_name)),
    );
    item.insert("arguments".into(), json!(args));
    item.insert("status".into(), json!("completed"));
    if let Some(namespace) = tool_ref.and_then(|r| r.namespace.as_deref()) {
        item.insert("namespace".into(), json!(namespace));
    }
    Value::Object(item)
}

fn thought_signature_of(tool_call: &Map<String, Value>) -> Option<String> {
    str_of(tool_call.get("thought_signature")).filter(|s| !s.is_empty()).map(|s| s.to_string())
}

#[derive(Debug, Clone)]
pub struct ResponsesOutputOptions {
    /// `response.model` — the client-facing id, same as the Chat path's chunks.
    pub model: String,
    pub tool_names: ResponsesToolNames,
}

fn response_status_from_finish(finish_reason: Option<&str>) -> (&'static str, Option<Value>) {
    match finish_reason {
        Some("length") => ("incomplete", Some(json!({ "reason": "max_output_tokens" }))),
        Some("content_filter") => ("incomplete", Some(json!({ "reason": "content_filter" }))),
        _ => ("completed", None),
    }
}

#[derive(Debug)]
enum Open {
    Reasoning { id: String, index: usize, text: String },
    Message { id: String, index: usize, text: String },
    Tool { index: usize, tool_index: i64 },
}

#[derive(Debug)]
struct ToolEntry {
    id: String,
    call_id: String,
    name: String,
    args: String,
    custom: bool,
    closed: bool,
    thought_signature: Option<String>,
}

struct ResponsesConverter {
    opts: ResponsesOutputOptions,
    seq: u64,
    response_id: String,
    created_at: i64,
    output: Vec<Value>,
    finished: bool,
    finish_reason: Option<String>,
    usage: Option<Value>,
    open: Option<Open>,
    tools: HashMap<i64, ToolEntry>,
    out: VecDeque<Bytes>,
}

impl ResponsesConverter {
    fn new(opts: ResponsesOutputOptions) -> Self {
        Self {
            opts,
            seq: 0,
            response_id: new_id("resp"),
            created_at: now_seconds(),
            output: Vec::new(),
            finished: false,
            finish_reason: None,
            usage: None,
            open: None,
            tools: HashMap::new(),
            out: VecDeque::new(),
        }
    }

    fn emit(&mut self, event_type: &str, payload: Map<String, Value>) {
        let mut body = Map::new();
        body.insert("type".into(), json!(event_type));
        body.insert("sequence_number".into(), json!(self.seq));
        self.seq += 1;
        for (k, v) in payload {
            body.insert(k, v);
        }
        let text = format!("event: {event_type}\ndata: {}\n\n", Value::Object(body));
        self.out.push_back(Bytes::from(text));
    }

    fn snapshot(&self, extra: Map<String, Value>) -> Value {
        let mut snap = Map::new();
        snap.insert("id".into(), json!(self.response_id));
        snap.insert("object".into(), json!("response"));
        snap.insert("created_at".into(), json!(self.created_at));
        snap.insert("model".into(), json!(self.opts.model));
        snap.insert("output".into(), Value::Array(self.output.clone()));
        for (k, v) in extra {
            snap.insert(k, v);
        }
        Value::Object(snap)
    }

    fn close_open(&mut self) {
        let Some(cur) = self.open.take() else { return };
        match cur {
            Open::Reasoning { id, index, text } => {
                let item = json!({
                    "id": id,
                    "type": "reasoning",
                    "summary": [{ "type": "summary_text", "text": text }],
                });
                self.emit(
                    "response.reasoning_summary_text.done",
                    json!({ "item_id": id, "output_index": index, "summary_index": 0, "text": text })
                        .as_object()
                        .cloned()
                        .expect("object"),
                );
                self.emit(
                    "response.reasoning_summary_part.done",
                    json!({
                        "item_id": id,
                        "output_index": index,
                        "summary_index": 0,
                        "part": { "type": "summary_text", "text": text },
                    })
                    .as_object()
                    .cloned()
                    .expect("object"),
                );
                self.output[index] = item.clone();
                self.emit(
                    "response.output_item.done",
                    json!({ "output_index": index, "item": item }).as_object().cloned().expect("object"),
                );
            }
            Open::Message { id, index, text } => {
                let part = json!({ "type": "output_text", "text": text, "annotations": [] });
                let item = json!({
                    "id": id,
                    "type": "message",
                    "role": "assistant",
                    "status": "completed",
                    "content": [part.clone()],
                });
                self.emit(
                    "response.output_text.done",
                    json!({ "item_id": id, "output_index": index, "content_index": 0, "text": text })
                        .as_object()
                        .cloned()
                        .expect("object"),
                );
                self.emit(
                    "response.content_part.done",
                    json!({ "item_id": id, "output_index": index, "content_index": 0, "part": part })
                        .as_object()
                        .cloned()
                        .expect("object"),
                );
                self.output[index] = item.clone();
                self.emit(
                    "response.output_item.done",
                    json!({ "output_index": index, "item": item }).as_object().cloned().expect("object"),
                );
            }
            Open::Tool { index, tool_index } => {
                let Some(tool) = self.tools.get_mut(&tool_index) else { return };
                tool.closed = true;
                let (id, call_id, name, args, custom) =
                    (tool.id.clone(), tool.call_id.clone(), tool.name.clone(), tool.args.clone(), tool.custom);
                let item = tool_call_item(&id, &call_id, &name, &args, &self.opts.tool_names);
                if custom {
                    // Custom tools carry a raw string the model produced as JSON fragments —
                    // only whole at the end, so the item goes out whole.
                    let mut in_progress = item.as_object().cloned().expect("object");
                    in_progress.insert("status".into(), json!("in_progress"));
                    self.emit(
                        "response.output_item.added",
                        json!({ "output_index": index, "item": Value::Object(in_progress) })
                            .as_object()
                            .cloned()
                            .expect("object"),
                    );
                } else {
                    self.emit(
                        "response.function_call_arguments.done",
                        json!({ "item_id": id, "output_index": index, "arguments": args })
                            .as_object()
                            .cloned()
                            .expect("object"),
                    );
                }
                self.output[index] = item.clone();
                self.emit(
                    "response.output_item.done",
                    json!({ "output_index": index, "item": item }).as_object().cloned().expect("object"),
                );
            }
        }
    }

    /// Opens (or keeps) the reasoning item; returns its id and output index.
    fn open_reasoning(&mut self) -> (String, usize) {
        if let Some(Open::Reasoning { id, index, .. }) = &self.open {
            return (id.clone(), *index);
        }
        self.close_open();
        let id = new_id("rs");
        let index = self.output.len();
        self.output.push(json!({ "id": id, "type": "reasoning", "summary": [] }));
        self.open = Some(Open::Reasoning { id: id.clone(), index, text: String::new() });
        self.emit(
            "response.output_item.added",
            json!({ "output_index": index, "item": { "id": id, "type": "reasoning", "summary": [] } })
                .as_object()
                .cloned()
                .expect("object"),
        );
        self.emit(
            "response.reasoning_summary_part.added",
            json!({
                "item_id": id,
                "output_index": index,
                "summary_index": 0,
                "part": { "type": "summary_text", "text": "" },
            })
            .as_object()
            .cloned()
            .expect("object"),
        );
        (id, index)
    }

    fn open_message(&mut self) -> (String, usize) {
        if let Some(Open::Message { id, index, .. }) = &self.open {
            return (id.clone(), *index);
        }
        self.close_open();
        let id = new_id("msg");
        let index = self.output.len();
        let item = json!({
            "id": id,
            "type": "message",
            "role": "assistant",
            "status": "in_progress",
            "content": [],
        });
        self.output.push(item.clone());
        self.open = Some(Open::Message { id: id.clone(), index, text: String::new() });
        self.emit(
            "response.output_item.added",
            json!({ "output_index": index, "item": item }).as_object().cloned().expect("object"),
        );
        self.emit(
            "response.content_part.added",
            json!({
                "item_id": id,
                "output_index": index,
                "content_index": 0,
                "part": { "type": "output_text", "text": "", "annotations": [] },
            })
            .as_object()
            .cloned()
            .expect("object"),
        );
        (id, index)
    }

    fn emit_thought_signature(&mut self, call_id: &str, signature: &str) {
        let item = gemini_thought_signature_item(call_id, signature);
        let index = self.output.len();
        self.output.push(item.clone());
        self.emit(
            "response.output_item.added",
            json!({ "output_index": index, "item": item }).as_object().cloned().expect("object"),
        );
        self.emit(
            "response.output_item.done",
            json!({ "output_index": index, "item": item }).as_object().cloned().expect("object"),
        );
    }

    fn open_tool(&mut self, tool_index: i64, call_id: &str, name: &str, thought_signature: Option<String>) {
        self.close_open();
        if let Some(signature) = thought_signature.as_deref() {
            self.emit_thought_signature(call_id, signature);
        }
        let custom = self.opts.tool_names.get(name).map(|r| r.is_custom()).unwrap_or(false);
        let id = new_id(if custom { "ctc" } else { "fc" });
        let index = self.output.len();
        self.tools.insert(
            tool_index,
            ToolEntry {
                id: id.clone(),
                call_id: call_id.to_string(),
                name: name.to_string(),
                args: String::new(),
                custom,
                closed: false,
                thought_signature,
            },
        );
        self.output.push(json!({}));
        self.open = Some(Open::Tool { index, tool_index });
        if !custom {
            let tool_ref = self.opts.tool_names.get(name).cloned();
            let mut item = Map::new();
            item.insert("id".into(), json!(id));
            item.insert("type".into(), json!("function_call"));
            item.insert("call_id".into(), json!(call_id));
            item.insert(
                "name".into(),
                json!(tool_ref.as_ref().map(|r| r.name.as_str()).unwrap_or(name)),
            );
            item.insert("arguments".into(), json!(""));
            item.insert("status".into(), json!("in_progress"));
            if let Some(namespace) = tool_ref.as_ref().and_then(|r| r.namespace.as_deref()) {
                item.insert("namespace".into(), json!(namespace));
            }
            self.emit(
                "response.output_item.added",
                json!({ "output_index": index, "item": Value::Object(item) })
                    .as_object()
                    .cloned()
                    .expect("object"),
            );
        }
    }

    fn fail(&mut self, message: &str, code: Option<&str>) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.close_open();
        let mut extra = Map::new();
        extra.insert("status".into(), json!("failed"));
        extra.insert(
            "error".into(),
            json!({ "code": code.unwrap_or("upstream_error"), "message": message }),
        );
        let response = self.snapshot(extra);
        self.emit(
            "response.failed",
            json!({ "response": response }).as_object().cloned().expect("object"),
        );
    }

    fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.close_open();
        let (status, incomplete_details) = response_status_from_finish(self.finish_reason.as_deref());
        let mut extra = Map::new();
        extra.insert("status".into(), json!(status));
        if let Some(details) = incomplete_details {
            extra.insert("incomplete_details".into(), details);
        }
        if let Some(usage) = chat_usage_to_responses(self.usage.as_ref()) {
            extra.insert("usage".into(), usage);
        }
        let response = self.snapshot(extra);
        let event = if status == "incomplete" { "response.incomplete" } else { "response.completed" };
        self.emit(event, json!({ "response": response }).as_object().cloned().expect("object"));
    }

    fn start(&mut self) {
        let mut in_progress = Map::new();
        in_progress.insert("status".into(), json!("in_progress"));
        let snapshot = self.snapshot(in_progress.clone());
        self.emit(
            "response.created",
            json!({ "response": snapshot }).as_object().cloned().expect("object"),
        );
        let snapshot = self.snapshot(in_progress);
        self.emit(
            "response.in_progress",
            json!({ "response": snapshot }).as_object().cloned().expect("object"),
        );
    }

    /// Handles one upstream SSE line. Returns `false` when the turn is over.
    fn on_line(&mut self, line: &str) -> bool {
        if self.finished {
            return false;
        }
        let Some(rest) = line.strip_prefix("data:") else { return true };
        let data = rest.trim();
        if data.is_empty() {
            return true;
        }
        if data == "[DONE]" {
            self.finish();
            return false;
        }
        let Ok(json) = serde_json::from_str::<Value>(data) else { return true };
        let Some(json) = json.as_object() else { return true };
        if let Some(err) = as_object(json.get("error")) {
            let message = str_of(err.get("message")).filter(|m| !m.is_empty()).unwrap_or("upstream error").to_string();
            let code = str_of(err.get("code")).map(|c| c.to_string());
            self.fail(&message, code.as_deref());
            return false;
        }
        if let Some(usage) = json.get("usage").filter(|u| u.is_object()) {
            self.usage = Some(usage.clone());
        }
        let Some(choice) =
            json.get("choices").and_then(|c| c.as_array()).and_then(|c| c.first()).and_then(|c| c.as_object())
        else {
            return true;
        };
        if let Some(delta) = as_object(choice.get("delta")).cloned() {
            if let Some(reasoning) = str_of(delta.get("reasoning_content")).filter(|s| !s.is_empty()) {
                let reasoning = reasoning.to_string();
                let (id, index) = self.open_reasoning();
                if let Some(Open::Reasoning { text, .. }) = &mut self.open {
                    text.push_str(&reasoning);
                }
                self.emit(
                    "response.reasoning_summary_text.delta",
                    json!({
                        "item_id": id,
                        "output_index": index,
                        "summary_index": 0,
                        "delta": reasoning,
                    })
                    .as_object()
                    .cloned()
                    .expect("object"),
                );
            }
            if let Some(content) = str_of(delta.get("content")).filter(|s| !s.is_empty()) {
                let content = content.to_string();
                let (id, index) = self.open_message();
                if let Some(Open::Message { text, .. }) = &mut self.open {
                    text.push_str(&content);
                }
                self.emit(
                    "response.output_text.delta",
                    json!({
                        "item_id": id,
                        "output_index": index,
                        "content_index": 0,
                        "delta": content,
                    })
                    .as_object()
                    .cloned()
                    .expect("object"),
                );
            }
            if let Some(Value::Array(tool_calls)) = delta.get("tool_calls") {
                for tc in tool_calls {
                    let Some(tc) = tc.as_object() else { continue };
                    let tool_index = tc.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
                    let function = as_object(tc.get("function")).cloned();
                    let thought_signature = thought_signature_of(tc);
                    if !self.tools.contains_key(&tool_index) {
                        let name = function
                            .as_ref()
                            .and_then(|f| str_of(f.get("name")))
                            .unwrap_or("")
                            .to_string();
                        let call_id = match str_of(tc.get("id")).filter(|s| !s.is_empty()) {
                            Some(id) => id.to_string(),
                            None => format!("call_{tool_index}_{}", self.seq),
                        };
                        self.open_tool(tool_index, &call_id, &name, thought_signature.clone());
                    } else if let Some(entry) = self.tools.get_mut(&tool_index) {
                        if entry.thought_signature.is_none() && thought_signature.is_some() {
                            entry.thought_signature = thought_signature.clone();
                        }
                    }
                    let args = function.as_ref().and_then(|f| str_of(f.get("arguments"))).unwrap_or("");
                    if !args.is_empty() {
                        let args = args.to_string();
                        let Some(entry) = self.tools.get_mut(&tool_index) else { continue };
                        if entry.closed {
                            continue;
                        }
                        entry.args.push_str(&args);
                        let entry_id = entry.id.clone();
                        let custom = entry.custom;
                        if !custom {
                            if let Some(Open::Tool { index, tool_index: open_index }) = &self.open {
                                if *open_index == tool_index {
                                    let index = *index;
                                    self.emit(
                                        "response.function_call_arguments.delta",
                                        json!({ "item_id": entry_id, "output_index": index, "delta": args })
                                            .as_object()
                                            .cloned()
                                            .expect("object"),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some(finish_reason) = str_of(choice.get("finish_reason")).filter(|s| !s.is_empty()) {
            self.finish_reason = Some(finish_reason.to_string());
        }
        true
    }
}

/// Chat Completions SSE → Responses SSE. Streams as it goes: one Chat chunk in, its
/// Responses events out. Never buffers the turn — only the text of the item currently open
/// is kept, so its `*.done` events can carry it.
pub fn openai_sse_to_responses_stream<S>(
    body: S,
    opts: ResponsesOutputOptions,
) -> impl Stream<Item = Result<Bytes, io::Error>> + Send
where
    S: Stream<Item = Result<Bytes, io::Error>> + Send + 'static,
{
    struct State<L> {
        lines: L,
        conv: ResponsesConverter,
        started: bool,
        done: bool,
    }
    let state = State {
        lines: Box::pin(sse_line_stream(body)),
        conv: ResponsesConverter::new(opts),
        started: false,
        done: false,
    };
    futures::stream::unfold(state, |mut st| async move {
        loop {
            if let Some(chunk) = st.conv.out.pop_front() {
                return Some((Ok(chunk), st));
            }
            if st.done {
                return None;
            }
            if !st.started {
                st.started = true;
                st.conv.start();
                continue;
            }
            match st.lines.next().await {
                Some(Ok(line)) => {
                    if !st.conv.on_line(&line) {
                        // Clean end of turn: the trailing finish() is a no-op.
                        st.conv.finish();
                        st.done = true;
                    }
                }
                Some(Err(e)) => {
                    st.done = true;
                    return Some((Err(e), st));
                }
                None => {
                    // Clean EOF without [DONE]: end the turn properly rather than leave
                    // the client waiting for a completion event that never comes.
                    st.conv.finish();
                    st.done = true;
                }
            }
        }
    })
}

/// Non-stream Chat completion → one Response object (same item shapes as the stream).
pub fn openai_to_responses_object(json: &Value, opts: &ResponsesOutputOptions) -> Value {
    let json = json.as_object().cloned().unwrap_or_default();
    let mut output: Vec<Value> = Vec::new();
    let choice = json
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .and_then(|c| c.as_object());
    let message = choice.and_then(|c| as_object(c.get("message")));
    if let Some(message) = message {
        if let Some(reasoning) = str_of(message.get("reasoning_content")).filter(|s| !s.is_empty()) {
            output.push(json!({
                "id": new_id("rs"),
                "type": "reasoning",
                "summary": [{ "type": "summary_text", "text": reasoning }],
            }));
        }
        let text = match message.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(_)) => text_of_parts(message.get("content"), ""),
            _ => String::new(),
        };
        if !text.is_empty() {
            output.push(json!({
                "id": new_id("msg"),
                "type": "message",
                "role": "assistant",
                "status": "completed",
                "content": [{ "type": "output_text", "text": text, "annotations": [] }],
            }));
        }
        if let Some(Value::Array(tool_calls)) = message.get("tool_calls") {
            for tc in tool_calls {
                let Some(tc) = tc.as_object() else { continue };
                let function = as_object(tc.get("function"));
                let name = function.and_then(|f| str_of(f.get("name"))).unwrap_or("");
                let args = function.and_then(|f| str_of(f.get("arguments"))).unwrap_or("{}");
                let call_id = match str_of(tc.get("id")) {
                    Some(id) => id.to_string(),
                    None => new_id("call"),
                };
                let thought_signature = thought_signature_of(tc);
                let custom = opts.tool_names.get(name).map(|r| r.is_custom()).unwrap_or(false);
                if let Some(signature) = thought_signature {
                    output.push(gemini_thought_signature_item(&call_id, &signature));
                }
                output.push(tool_call_item(
                    &new_id(if custom { "ctc" } else { "fc" }),
                    &call_id,
                    name,
                    args,
                    &opts.tool_names,
                ));
            }
        }
    }
    let finish_reason = choice.and_then(|c| str_of(c.get("finish_reason")));
    let (status, incomplete_details) = response_status_from_finish(finish_reason);
    let mut response = Map::new();
    response.insert("id".into(), json!(new_id("resp")));
    response.insert("object".into(), json!("response"));
    response.insert(
        "created_at".into(),
        match json.get("created") {
            Some(Value::Number(n)) => Value::Number(n.clone()),
            _ => json!(now_seconds()),
        },
    );
    response.insert("status".into(), json!(status));
    response.insert("model".into(), json!(opts.model));
    response.insert("output".into(), Value::Array(output));
    if let Some(details) = incomplete_details {
        response.insert("incomplete_details".into(), details);
    }
    if let Some(usage) = chat_usage_to_responses(json.get("usage")) {
        response.insert("usage".into(), usage);
    }
    Value::Object(response)
}

// ---------------------------------------------------------------------------
// Native codex path helpers
// ---------------------------------------------------------------------------

/// Parsed SSE lines have a finite byte budget; native byte passthrough does not use this
/// reader (apps/api/src/proxy/sse_lines.ts).
pub(crate) const MAX_SSE_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Split an upstream byte stream into SSE lines as the bytes arrive, including an
/// unterminated line at EOF. A private port of `readSseLines` so this module does not wait
/// on `proxy::sse_lines`.
pub(crate) fn sse_line_stream<S>(body: S) -> impl Stream<Item = Result<String, io::Error>> + Send
where
    S: Stream<Item = Result<Bytes, io::Error>> + Send + 'static,
{
    struct State<S> {
        body: S,
        buf: Vec<u8>,
        pending: VecDeque<String>,
        eof: bool,
    }
    let state = State { body: Box::pin(body), buf: Vec::new(), pending: VecDeque::new(), eof: false };
    futures::stream::unfold(state, |mut st| async move {
        loop {
            if let Some(line) = st.pending.pop_front() {
                return Some((Ok(line), st));
            }
            if st.eof {
                return None;
            }
            match st.body.next().await {
                Some(Ok(chunk)) => {
                    st.buf.extend_from_slice(&chunk);
                    let mut start = 0;
                    while let Some(pos) = st.buf[start..].iter().position(|b| *b == b'\n') {
                        let end = start + pos;
                        st.pending.push_back(String::from_utf8_lossy(&st.buf[start..end]).into_owned());
                        start = end + 1;
                    }
                    st.buf.drain(..start);
                    if st.buf.len() > MAX_SSE_LINE_BYTES {
                        st.eof = true;
                        st.pending.clear();
                        return Some((
                            Err(io::Error::other("Upstream SSE event exceeds the 16 MiB parsing limit")),
                            st,
                        ));
                    }
                }
                Some(Err(e)) => {
                    st.eof = true;
                    return Some((Err(e), st));
                }
                None => {
                    st.eof = true;
                    if !st.buf.is_empty() {
                        st.pending.push_back(String::from_utf8_lossy(&st.buf).into_owned());
                        st.buf.clear();
                    }
                }
            }
        }
    })
}

const ERROR_PREFIXES: [&[u8]; 2] = [b"data: {\"error\"", b"data:{\"error\""];
pub(crate) const MAX_PROXY_ERROR_BYTES: usize = 64 * 1024;

fn responses_failed_line(line: &str, model: &str) -> String {
    let Some(rest) = line.strip_prefix("data:") else { return line.to_string() };
    let Ok(json) = serde_json::from_str::<Value>(rest.trim()) else { return line.to_string() };
    let Some(err) = json.as_object().and_then(|o| as_object(o.get("error"))) else {
        return line.to_string();
    };
    let payload = json!({
        "type": "response.failed",
        "response": {
            "id": new_id("resp"),
            "object": "response",
            "created_at": now_seconds(),
            "status": "failed",
            "model": model,
            "output": [],
            "error": {
                "code": str_of(err.get("code")).unwrap_or("upstream_error"),
                "message": str_of(err.get("message")).unwrap_or("upstream error"),
            },
        },
    });
    format!("event: response.failed\ndata: {payload}")
}

/// Rewrites dispatch's OpenAI-shaped in-stream error lines (`data: {"error":…}` — pool
/// exhaustion, upstream non-2xx, stall) into a `response.failed` event, leaving every other
/// byte of the relayed Responses SSE untouched. Only error candidates are buffered, up to
/// 64 KiB.
pub fn rewrite_openai_error_frames_to_responses<S>(
    body: S,
    model: String,
) -> impl Stream<Item = Result<Bytes, io::Error>> + Send
where
    S: Stream<Item = Result<Bytes, io::Error>> + Send + 'static,
{
    #[derive(PartialEq, Eq)]
    enum LineState {
        Prefix,
        Pass,
        Error,
        Skip,
    }
    struct State<S> {
        body: S,
        model: String,
        state: LineState,
        prefix: Vec<u8>,
        error: Vec<u8>,
        out: VecDeque<Bytes>,
        eof: bool,
        flushed: bool,
    }

    fn reset<S>(st: &mut State<S>) {
        st.state = LineState::Prefix;
        st.prefix.clear();
        st.error.clear();
    }

    fn end_line<S>(st: &mut State<S>, newline: bool) {
        match st.state {
            LineState::Prefix => {
                if !st.prefix.is_empty() {
                    st.out.push_back(Bytes::from(st.prefix.clone()));
                }
            }
            LineState::Error => {
                let original = String::from_utf8_lossy(&st.error).into_owned();
                let rewritten = responses_failed_line(&original, &st.model);
                if rewritten == original {
                    st.out.push_back(Bytes::from(st.error.clone()));
                    if newline {
                        st.out.push_back(Bytes::from_static(b"\n"));
                    }
                } else {
                    let mut text = rewritten;
                    if newline {
                        text.push('\n');
                    }
                    st.out.push_back(Bytes::from(text));
                }
            }
            _ => {}
        }
        reset(st);
    }

    fn feed<S>(st: &mut State<S>, chunk: &[u8]) {
        let mut offset = 0;
        while offset < chunk.len() {
            let newline = chunk[offset..].iter().position(|b| *b == b'\n').map(|p| offset + p);
            let end = newline.unwrap_or(chunk.len());
            while offset < end && st.state == LineState::Prefix {
                st.prefix.push(chunk[offset]);
                offset += 1;
                let size = st.prefix.len();
                let candidates: Vec<&&[u8]> = ERROR_PREFIXES
                    .iter()
                    .filter(|p| size <= p.len() && p[..size] == st.prefix[..])
                    .collect();
                if candidates.is_empty() {
                    st.state = LineState::Pass;
                    st.out.push_back(Bytes::from(st.prefix.clone()));
                } else if candidates.iter().any(|p| p.len() == size) {
                    st.state = LineState::Error;
                    st.error.clear();
                    st.error.extend_from_slice(&st.prefix);
                }
            }
            match st.state {
                LineState::Pass => {
                    let stop = end + usize::from(newline.is_some());
                    if end > offset || newline.is_some() {
                        st.out.push_back(Bytes::copy_from_slice(&chunk[offset..stop]));
                    }
                }
                LineState::Error => {
                    if st.error.len() + end - offset > MAX_PROXY_ERROR_BYTES {
                        let line = responses_failed_line(
                            "data: {\"error\":{\"code\":\"upstream_error\",\"message\":\"Proxy error exceeds the 64 KiB rewrite limit\"}}",
                            &st.model,
                        );
                        st.out.push_back(Bytes::from(format!("{line}\n")));
                        st.error.clear();
                        st.state = LineState::Skip;
                    } else {
                        st.error.extend_from_slice(&chunk[offset..end]);
                    }
                }
                _ => {}
            }
            if let Some(newline) = newline {
                if st.state == LineState::Prefix {
                    if !st.prefix.is_empty() {
                        st.out.push_back(Bytes::from(st.prefix.clone()));
                    }
                    st.out.push_back(Bytes::from_static(b"\n"));
                    reset(st);
                } else {
                    end_line(st, true);
                }
                offset = newline + 1;
            } else {
                offset = end;
            }
        }
    }

    let state = State {
        body: Box::pin(body),
        model,
        state: LineState::Prefix,
        prefix: Vec::new(),
        error: Vec::new(),
        out: VecDeque::new(),
        eof: false,
        flushed: false,
    };
    futures::stream::unfold(state, |mut st| async move {
        loop {
            if let Some(chunk) = st.out.pop_front() {
                return Some((Ok(chunk), st));
            }
            if st.eof {
                if st.flushed {
                    return None;
                }
                st.flushed = true;
                end_line(&mut st, false);
                continue;
            }
            match st.body.next().await {
                Some(Ok(chunk)) => feed(&mut st, &chunk),
                Some(Err(e)) => {
                    st.eof = true;
                    st.flushed = true;
                    return Some((Err(e), st));
                }
                None => st.eof = true,
            }
        }
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum CollectedResponses {
    Response(Value),
    /// `{"error":{"message":…,"type":"upstream_error"}}` for the caller's envelope.
    Error { message: String },
}

impl CollectedResponses {
    pub fn error_value(&self) -> Option<Value> {
        match self {
            CollectedResponses::Error { message } => {
                Some(json!({ "error": { "message": message, "type": "upstream_error" } }))
            }
            CollectedResponses::Response(_) => None,
        }
    }
}

/// Non-stream native path: drain a Responses SSE and return its terminal `response` object,
/// or the failure.
pub async fn collect_responses_sse<S>(body: S) -> CollectedResponses
where
    S: Stream<Item = Result<Bytes, io::Error>> + Send + 'static,
{
    let mut lines = Box::pin(sse_line_stream(body));
    let mut response: Option<Value> = None;
    let mut error: Option<String> = None;
    while let Some(line) = lines.next().await {
        let Ok(line) = line else { break };
        let Some(rest) = line.strip_prefix("data:") else { continue };
        let data = rest.trim();
        if data.is_empty() || data == "[DONE]" || error.is_some() {
            continue;
        }
        let Ok(ev) = serde_json::from_str::<Value>(data) else { continue };
        let Some(ev) = ev.as_object() else { continue };
        let ty = str_of(ev.get("type")).unwrap_or("");
        if ty == "response.failed" || ty == "error" {
            let message = as_object(ev.get("response"))
                .and_then(|r| as_object(r.get("error")))
                .and_then(|e| str_of(e.get("message")))
                .filter(|m| !m.is_empty())
                .or_else(|| as_object(ev.get("error")).and_then(|e| str_of(e.get("message"))).filter(|m| !m.is_empty()))
                .or_else(|| str_of(ev.get("message")).filter(|m| !m.is_empty()))
                .unwrap_or("codex upstream failure")
                .to_string();
            error = Some(message);
        } else if matches!(ty, "response.completed" | "response.incomplete" | "response.done") {
            if let Some(r) = ev.get("response").filter(|r| r.is_object()) {
                response = Some(r.clone());
            }
        }
    }
    if let Some(message) = error {
        return CollectedResponses::Error { message };
    }
    match response {
        Some(response) => CollectedResponses::Response(response),
        None => CollectedResponses::Error {
            message: "upstream ended without response.completed".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_names(entries: &[(&str, ResponsesToolRef)]) -> ResponsesToolNames {
        entries.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect()
    }

    /// Bytes split every 7 bytes, mid-line, to prove the converters carry partial lines.
    fn sse_body(lines: &[&str]) -> impl Stream<Item = Result<Bytes, io::Error>> + Send + 'static {
        let text: String = lines.iter().map(|l| format!("{l}\n\n")).collect();
        chunked(&text, 7)
    }

    fn chunked(text: &str, size: usize) -> impl Stream<Item = Result<Bytes, io::Error>> + Send + 'static {
        let bytes = Bytes::from(text.to_string());
        let chunks: Vec<Result<Bytes, io::Error>> =
            (0..bytes.len()).step_by(size).map(|i| Ok(bytes.slice(i..(i + size).min(bytes.len())))).collect();
        futures::stream::iter(chunks)
    }

    async fn drain<S>(stream: S) -> String
    where
        S: Stream<Item = Result<Bytes, io::Error>>,
    {
        let mut out = Vec::new();
        let mut stream = Box::pin(stream);
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.expect("no stream error"));
        }
        String::from_utf8(out).expect("utf-8")
    }

    /// Parse a Responses SSE text into its event payloads (in order).
    fn events(text: &str) -> Vec<Map<String, Value>> {
        text.split('\n')
            .filter_map(|l| l.strip_prefix("data:"))
            .map(|d| serde_json::from_str::<Value>(d.trim()).expect("event JSON"))
            .map(|v| v.as_object().cloned().expect("object"))
            .collect()
    }

    fn event_types(evs: &[Map<String, Value>]) -> Vec<String> {
        evs.iter().map(|e| e["type"].as_str().expect("type").to_string()).collect()
    }

    fn chat_chunk(delta: Value) -> String {
        format!(
            "data: {}",
            json!({
                "id": "chatcmpl_x",
                "object": "chat.completion.chunk",
                "choices": [{ "index": 0, "delta": delta, "finish_reason": null }],
            })
        )
    }

    fn body_of(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("object body")
    }

    /// The shape the Codex CLI 0.150.1 actually sent (captured 2026-09-04).
    fn codex_cli_request() -> Map<String, Value> {
        body_of(json!({
            "model": "claude-code/claude-opus-5",
            "instructions": "You are Codex.",
            "input": [
                {
                    "type": "message",
                    "role": "developer",
                    "content": [
                        { "type": "input_text", "text": "<environment_context>cwd=/tmp</environment_context>" },
                        { "type": "input_text", "text": "AGENTS.md says hi" }
                    ]
                },
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "say hi" }] }
            ],
            "tools": [
                {
                    "type": "function",
                    "name": "exec_command",
                    "description": "Run a command",
                    "strict": false,
                    "parameters": { "type": "object", "properties": { "cmd": { "type": "string" } }, "required": ["cmd"] }
                },
                {
                    "type": "namespace",
                    "name": "multi_agent_v1",
                    "description": "Tools in the multi_agent_v1 namespace.",
                    "tools": [
                        {
                            "type": "function",
                            "name": "spawn_agent",
                            "description": "Spawn a sub-agent",
                            "strict": false,
                            "parameters": { "type": "object", "properties": { "message": { "type": "string" } } }
                        }
                    ]
                },
                { "type": "web_search", "external_web_access": false }
            ],
            "tool_choice": "auto",
            "parallel_tool_calls": true,
            "reasoning": { "summary": "auto" },
            "store": false,
            "stream": true,
            "include": ["reasoning.encrypted_content"],
            "prompt_cache_key": "01a06ce4-68a8-7892-bcf5-b64013c7f279",
            "client_metadata": { "session_id": "01a06ce4-68a8-7892-bcf5-b64013c7f279" }
        }))
    }

    #[test]
    fn converts_the_codex_cli_request() {
        let ResponsesToChatResult { chat, tool_names } =
            responses_to_chat_request(&codex_cli_request()).expect("converts");
        assert_eq!(chat["model"], json!("claude-code/claude-opus-5"));
        assert_eq!(chat["stream"], json!(true));
        assert_eq!(
            chat["messages"],
            json!([
                { "role": "system", "content": "You are Codex." },
                { "role": "system", "content": "<environment_context>cwd=/tmp</environment_context>\n\nAGENTS.md says hi" },
                { "role": "user", "content": "say hi" }
            ])
        );
        let tools = chat["tools"].as_array().expect("tools");
        let names: Vec<&str> =
            tools.iter().map(|t| t["function"]["name"].as_str().expect("name")).collect();
        assert_eq!(names, ["exec_command", "multi_agent_v1__spawn_agent", "web_search"]);
        assert_eq!(tools[2], web_search_stub_tool());
        assert_eq!(
            tool_names.get("multi_agent_v1__spawn_agent"),
            Some(&ResponsesToolRef::function("spawn_agent").namespaced("multi_agent_v1"))
        );
        assert_eq!(chat["tool_choice"], json!("auto"));
        assert_eq!(chat["parallel_tool_calls"], json!(true));
        assert_eq!(chat["prompt_cache_key"], json!("01a06ce4-68a8-7892-bcf5-b64013c7f279"));
        // Nothing the Chat wire has no field for leaks into the passthrough body.
        for absent in ["client_metadata", "include", "store", "reasoning_effort"] {
            assert!(!chat.contains_key(absent), "{absent} must not leak");
        }
    }

    #[test]
    fn keeps_a_client_declared_web_search_function() {
        let ResponsesToChatResult { chat, .. } = responses_to_chat_request(&body_of(json!({
            "model": "grok/grok-4.5",
            "input": "hi",
            "tools": [
                { "type": "function", "name": "web_search", "parameters": { "type": "object" } },
                { "type": "web_search" }
            ]
        })))
        .expect("converts");
        let tools = chat["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["function"]["name"], json!("web_search"));
        assert_ne!(tools[0], web_search_stub_tool());
    }

    #[test]
    fn folds_a_multi_turn_tool_round_into_chat_messages() {
        let ResponsesToChatResult { chat, .. } = responses_to_chat_request(&body_of(json!({
            "model": "claude-code/claude-opus-5",
            "input": [
                { "type": "message", "role": "user", "content": "list files" },
                { "type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "gAAAA…" },
                { "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "Sure." }] },
                { "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "exec_command", "arguments": "{\"cmd\":\"ls\"}" },
                { "type": "function_call", "id": "fc_2", "call_id": "call_2", "name": "spawn_agent", "namespace": "multi_agent_v1", "arguments": "{\"message\":\"go\"}" },
                { "type": "function_call_output", "call_id": "call_1", "output": "a.txt\nb.txt" },
                { "type": "function_call_output", "call_id": "call_2", "output": [{ "type": "input_text", "text": "spawned" }] },
                { "type": "custom_tool_call", "call_id": "call_3", "name": "apply_patch", "input": "*** Begin Patch" },
                { "type": "custom_tool_call_output", "call_id": "call_3", "output": "ok" },
                { "role": "user", "content": [{ "type": "input_text", "text": "thanks" }] }
            ]
        })))
        .expect("converts");
        assert_eq!(
            chat["messages"],
            json!([
                { "role": "user", "content": "list files" },
                {
                    "role": "assistant",
                    "content": "Sure.",
                    "tool_calls": [
                        { "id": "call_1", "type": "function", "function": { "name": "exec_command", "arguments": "{\"cmd\":\"ls\"}" } },
                        { "id": "call_2", "type": "function", "function": { "name": "multi_agent_v1__spawn_agent", "arguments": "{\"message\":\"go\"}" } }
                    ]
                },
                { "role": "tool", "tool_call_id": "call_1", "content": "a.txt\nb.txt" },
                { "role": "tool", "tool_call_id": "call_2", "content": "spawned" },
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        { "id": "call_3", "type": "function", "function": { "name": "apply_patch", "arguments": "{\"input\":\"*** Begin Patch\"}" } }
                    ]
                },
                { "role": "tool", "tool_call_id": "call_3", "content": "ok" },
                { "role": "user", "content": "thanks" }
            ])
        );
    }

    #[test]
    fn restores_only_proxy_owned_gemini_signatures() {
        let marker = |call_id: &str, signature: &str| {
            json!({
                "type": "reasoning",
                "summary": [],
                "encrypted_content": format!(
                    "kano-proxy:gemini-thought-signature:v1:{}",
                    json!({ "call_id": call_id, "signature": signature })
                ),
            })
        };
        let ResponsesToChatResult { chat, .. } = responses_to_chat_request(&body_of(json!({
            "model": "antigravity/gemini-3-flash",
            "input": [
                marker("call_2", "sig-2"),
                { "type": "reasoning", "summary": [], "encrypted_content": "foreign-encrypted-reasoning" },
                marker("call_1", "sig-1"),
                { "type": "function_call", "call_id": "call_1", "name": "first", "arguments": "{}" },
                { "type": "function_call", "call_id": "call_2", "name": "second", "arguments": "{}" },
                { "type": "function_call", "call_id": "call_3", "name": "unsigned", "arguments": "{}" }
            ]
        })))
        .expect("converts");
        let calls = chat["messages"][0]["tool_calls"].as_array().expect("tool calls").clone();
        assert_eq!(calls[0]["id"], json!("call_1"));
        assert_eq!(calls[0]["thought_signature"], json!("sig-1"));
        assert_eq!(calls[1]["id"], json!("call_2"));
        assert_eq!(calls[1]["thought_signature"], json!("sig-2"));
        assert!(calls[2].get("thought_signature").is_none());
    }

    #[test]
    fn maps_images_custom_tools_format_effort_and_tool_choice() {
        let ResponsesToChatResult { chat, tool_names } = responses_to_chat_request(&body_of(json!({
            "model": "grok/grok-4.5",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [
                    { "type": "input_text", "text": "what is this" },
                    { "type": "input_image", "image_url": "data:image/png;base64,AAAA", "detail": "high" }
                ]
            }],
            "tools": [{ "type": "custom", "name": "apply_patch", "description": "Patch files" }],
            "tool_choice": { "type": "function", "name": "apply_patch" },
            "text": { "format": { "type": "json_schema", "name": "out", "schema": { "type": "object" }, "strict": true }, "verbosity": "low" },
            "reasoning": { "effort": "high", "summary": "detailed" },
            "max_output_tokens": 123,
            "temperature": 0.2,
            "top_p": 0.9
        })))
        .expect("converts");
        assert_eq!(
            chat["messages"],
            json!([{
                "role": "user",
                "content": [
                    { "type": "text", "text": "what is this" },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA", "detail": "high" } }
                ]
            }])
        );
        let tools = chat["tools"].as_array().expect("tools");
        assert_eq!(tools[0]["function"]["name"], json!("apply_patch"));
        assert_eq!(tools[0]["function"]["parameters"]["required"], json!(["input"]));
        assert_eq!(tool_names.get("apply_patch"), Some(&ResponsesToolRef::custom("apply_patch")));
        assert_eq!(chat["tool_choice"], json!({ "type": "function", "function": { "name": "apply_patch" } }));
        assert_eq!(
            chat["response_format"],
            json!({ "type": "json_schema", "json_schema": { "name": "out", "schema": { "type": "object" }, "strict": true } })
        );
        assert_eq!(chat["reasoning_effort"], json!("high"));
        assert_eq!(chat["max_tokens"], json!(123));
        assert_eq!(chat["temperature"], json!(0.2));
        assert_eq!(chat["top_p"], json!(0.9));
    }

    #[test]
    fn drops_tool_choice_when_every_tool_was_hosted() {
        let ResponsesToChatResult { chat, .. } = responses_to_chat_request(&body_of(json!({
            "model": "grok/grok-4.5",
            "input": "hi",
            "tools": [{ "type": "image_generation" }],
            "tool_choice": "required"
        })))
        .expect("converts");
        assert!(!chat.contains_key("tools"));
        assert!(!chat.contains_key("tool_choice"));
    }

    #[test]
    fn rejects_unsupported_fields() {
        let cases: Vec<(Value, &str)> = vec![
            (json!({ "previous_response_id": "resp_1" }), "previous_response_id"),
            (json!({ "conversation": "conv_1" }), "conversation"),
            (json!({ "background": true }), "background"),
            (json!({ "input": [{ "type": "item_reference", "id": "msg_1" }] }), "input.item_reference"),
            (
                json!({ "input": [{ "type": "message", "role": "user", "content": [{ "type": "input_file", "file_id": "f" }] }] }),
                "input.content.input_file",
            ),
        ];
        for (extra, field) in cases {
            let mut body = body_of(json!({ "model": "grok/grok-4.5", "input": "hi" }));
            for (k, v) in extra.as_object().expect("object") {
                body.insert(k.clone(), v.clone());
            }
            let err = responses_to_chat_request(&body).expect_err("rejects");
            assert_eq!(err.field, field);
        }
    }

    fn stream_tool_names() -> ResponsesToolNames {
        tool_names(&[
            ("exec_command", ResponsesToolRef::function("exec_command")),
            (
                "multi_agent_v1__spawn_agent",
                ResponsesToolRef::function("spawn_agent").namespaced("multi_agent_v1"),
            ),
            ("apply_patch", ResponsesToolRef::custom("apply_patch")),
        ])
    }

    #[tokio::test]
    async fn emits_the_responses_event_sequence_with_usage() {
        let usage_chunk = format!(
            "data: {}",
            json!({
                "id": "chatcmpl_x",
                "object": "chat.completion.chunk",
                "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
                "usage": {
                    "prompt_tokens": 100,
                    "completion_tokens": 20,
                    "prompt_tokens_details": { "cached_tokens": 60 },
                    "completion_tokens_details": { "reasoning_tokens": 5 }
                }
            })
        );
        let lines = vec![
            chat_chunk(json!({ "role": "assistant", "content": "" })),
            chat_chunk(json!({ "reasoning_content": "thinking " })),
            chat_chunk(json!({ "reasoning_content": "hard" })),
            chat_chunk(json!({ "content": "Hel" })),
            chat_chunk(json!({ "content": "lo" })),
            chat_chunk(json!({
                "tool_calls": [{ "index": 0, "id": "call_1", "type": "function", "function": { "name": "exec_command", "arguments": "" } }]
            })),
            chat_chunk(json!({ "tool_calls": [{ "index": 0, "function": { "arguments": "{\"cmd\":" } }] })),
            chat_chunk(json!({ "tool_calls": [{ "index": 0, "function": { "arguments": "\"ls\"}" } }] })),
            usage_chunk,
            "data: [DONE]".to_string(),
        ];
        let refs: Vec<&str> = lines.iter().map(|l| l.as_str()).collect();
        let text = drain(openai_sse_to_responses_stream(
            sse_body(&refs),
            ResponsesOutputOptions {
                model: "claude-code/claude-opus-5".into(),
                tool_names: stream_tool_names(),
            },
        ))
        .await;
        assert!(text.starts_with("event: response.created\ndata: "));
        let evs = events(&text);
        assert_eq!(
            event_types(&evs),
            [
                "response.created",
                "response.in_progress",
                "response.output_item.added",
                "response.reasoning_summary_part.added",
                "response.reasoning_summary_text.delta",
                "response.reasoning_summary_text.delta",
                "response.reasoning_summary_text.done",
                "response.reasoning_summary_part.done",
                "response.output_item.done",
                "response.output_item.added",
                "response.content_part.added",
                "response.output_text.delta",
                "response.output_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.output_item.added",
                "response.function_call_arguments.delta",
                "response.function_call_arguments.delta",
                "response.function_call_arguments.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
        // Monotonic sequence numbers.
        let seqs: Vec<u64> = evs.iter().map(|e| e["sequence_number"].as_u64().expect("seq")).collect();
        assert_eq!(seqs, (0..evs.len() as u64).collect::<Vec<_>>());

        let completed = evs.last().expect("completed")["response"].as_object().expect("response").clone();
        assert_eq!(completed["status"], json!("completed"));
        assert_eq!(completed["model"], json!("claude-code/claude-opus-5"));
        let output = completed["output"].as_array().expect("output");
        assert_eq!(output.len(), 3);
        assert_eq!(output[0]["type"], json!("reasoning"));
        assert_eq!(output[0]["summary"], json!([{ "type": "summary_text", "text": "thinking hard" }]));
        assert_eq!(output[1]["content"], json!([{ "type": "output_text", "text": "Hello", "annotations": [] }]));
        assert_eq!(output[1]["status"], json!("completed"));
        assert_eq!(output[2]["type"], json!("function_call"));
        assert_eq!(output[2]["call_id"], json!("call_1"));
        assert_eq!(output[2]["name"], json!("exec_command"));
        assert_eq!(output[2]["arguments"], json!("{\"cmd\":\"ls\"}"));
        assert_eq!(output[2]["status"], json!("completed"));
        assert!(output[2].get("namespace").is_none());
        assert_eq!(
            completed["usage"],
            json!({
                "input_tokens": 100,
                "output_tokens": 20,
                "total_tokens": 120,
                "input_tokens_details": { "cached_tokens": 60 },
                "output_tokens_details": { "reasoning_tokens": 5 }
            })
        );
        assert!(output[0]["id"].as_str().expect("id").starts_with("rs_"));
        assert!(output[1]["id"].as_str().expect("id").starts_with("msg_"));
        assert!(output[2]["id"].as_str().expect("id").starts_with("fc_"));
    }

    #[tokio::test]
    async fn emits_and_replays_a_gemini_signature_before_the_call() {
        let finish = format!(
            "data: {}",
            json!({ "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }] })
        );
        let lines = [
            chat_chunk(json!({
                "tool_calls": [{
                    "index": 0,
                    "id": "call_sig",
                    "type": "function",
                    "function": { "name": "exec_command", "arguments": "{\"cmd\":\"pwd\"}" },
                    "thought_signature": "sig-stream"
                }]
            })),
            finish,
            "data: [DONE]".to_string(),
        ];
        let refs: Vec<&str> = lines.iter().map(|l| l.as_str()).collect();
        let text = drain(openai_sse_to_responses_stream(
            sse_body(&refs),
            ResponsesOutputOptions {
                model: "antigravity/gemini-3-flash".into(),
                tool_names: stream_tool_names(),
            },
        ))
        .await;
        let evs = events(&text);
        assert_eq!(
            event_types(&evs),
            [
                "response.created",
                "response.in_progress",
                "response.output_item.added",
                "response.output_item.done",
                "response.output_item.added",
                "response.function_call_arguments.delta",
                "response.function_call_arguments.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
        let completed = evs.last().expect("completed")["response"].clone();
        let output = completed["output"].as_array().expect("output").clone();
        assert_eq!(output.len(), 2);
        assert_eq!(output[0]["type"], json!("reasoning"));
        assert_eq!(output[0]["summary"], json!([]));
        let encrypted = output[0]["encrypted_content"].as_str().expect("marker");
        assert!(encrypted.contains("\"call_id\":\"call_sig\""));
        assert!(encrypted.contains("\"signature\":\"sig-stream\""));
        assert_eq!(output[1]["type"], json!("function_call"));
        assert_eq!(output[1]["call_id"], json!("call_sig"));

        let replay = responses_to_chat_request(&body_of(json!({
            "model": "antigravity/gemini-3-flash",
            "input": output
        })))
        .expect("replays");
        let call = replay.chat["messages"][0]["tool_calls"][0].clone();
        assert_eq!(call["id"], json!("call_sig"));
        assert_eq!(call["thought_signature"], json!("sig-stream"));
    }

    #[tokio::test]
    async fn restores_namespace_and_emits_a_custom_call_whole() {
        let finish = format!(
            "data: {}",
            json!({ "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }] })
        );
        let lines = [
            chat_chunk(json!({
                "tool_calls": [{ "index": 0, "id": "call_a", "type": "function", "function": { "name": "multi_agent_v1__spawn_agent", "arguments": "{\"message\":\"go\"}" } }]
            })),
            chat_chunk(json!({
                "tool_calls": [{ "index": 1, "id": "call_b", "type": "function", "function": { "name": "apply_patch", "arguments": "" } }]
            })),
            chat_chunk(json!({ "tool_calls": [{ "index": 1, "function": { "arguments": "{\"input\":\"*** Begin" } }] })),
            chat_chunk(json!({ "tool_calls": [{ "index": 1, "function": { "arguments": " Patch\"}" } }] })),
            finish,
            "data: [DONE]".to_string(),
        ];
        let refs: Vec<&str> = lines.iter().map(|l| l.as_str()).collect();
        let text = drain(openai_sse_to_responses_stream(
            sse_body(&refs),
            ResponsesOutputOptions { model: "m".into(), tool_names: stream_tool_names() },
        ))
        .await;
        let evs = events(&text);
        let added: Vec<Value> = evs
            .iter()
            .filter(|e| e["type"] == json!("response.output_item.added"))
            .map(|e| e["item"].clone())
            .collect();
        assert_eq!(added[0]["type"], json!("function_call"));
        assert_eq!(added[0]["call_id"], json!("call_a"));
        assert_eq!(added[0]["name"], json!("spawn_agent"));
        assert_eq!(added[0]["namespace"], json!("multi_agent_v1"));
        assert_eq!(added[0]["status"], json!("in_progress"));
        // The custom tool item is announced only once its input is complete.
        assert_eq!(added[1]["type"], json!("custom_tool_call"));
        assert_eq!(added[1]["call_id"], json!("call_b"));
        assert_eq!(added[1]["name"], json!("apply_patch"));
        assert_eq!(added[1]["input"], json!("*** Begin Patch"));
        assert!(!evs.iter().any(|e| {
            e["type"] == json!("response.function_call_arguments.delta")
                && e["item_id"].as_str().map(|s| s.starts_with("ctc_")).unwrap_or(false)
        }));
        let done: Vec<Value> = evs
            .iter()
            .filter(|e| e["type"] == json!("response.output_item.done"))
            .map(|e| e["item"].clone())
            .collect();
        assert_eq!(done.len(), 2);
        assert_eq!(done[0]["name"], json!("spawn_agent"));
        assert_eq!(done[0]["namespace"], json!("multi_agent_v1"));
        assert_eq!(done[0]["arguments"], json!("{\"message\":\"go\"}"));
        assert_eq!(done[1]["type"], json!("custom_tool_call"));
        assert_eq!(done[1]["input"], json!("*** Begin Patch"));
    }

    #[tokio::test]
    async fn turns_an_in_stream_error_line_into_response_failed() {
        let lines = [
            chat_chunk(json!({ "content": "partial" })),
            "data: {\"error\":{\"message\":\"All upstream accounts unavailable\",\"type\":\"api_error\",\"code\":\"upstream_unavailable\"}}".to_string(),
            chat_chunk(json!({ "content": "never" })),
            "data: [DONE]".to_string(),
        ];
        let refs: Vec<&str> = lines.iter().map(|l| l.as_str()).collect();
        let text = drain(openai_sse_to_responses_stream(
            sse_body(&refs),
            ResponsesOutputOptions { model: "m".into(), tool_names: ResponsesToolNames::new() },
        ))
        .await;
        let evs = events(&text);
        let last = evs.last().expect("last");
        assert_eq!(last["type"], json!("response.failed"));
        assert_eq!(last["response"]["status"], json!("failed"));
        assert_eq!(
            last["response"]["error"],
            json!({ "code": "upstream_unavailable", "message": "All upstream accounts unavailable" })
        );
        // The message item that was open is closed before failing, and nothing follows.
        assert_eq!(evs.iter().filter(|e| e["type"] == json!("response.output_text.delta")).count(), 1);
        assert!(!evs.iter().any(|e| e["type"] == json!("response.completed")));
    }

    #[tokio::test]
    async fn ends_a_truncated_turn_as_incomplete() {
        let finish = format!(
            "data: {}",
            json!({ "choices": [{ "index": 0, "delta": {}, "finish_reason": "length" }] })
        );
        let lines = [chat_chunk(json!({ "content": "x" })), finish, "data: [DONE]".to_string()];
        let refs: Vec<&str> = lines.iter().map(|l| l.as_str()).collect();
        let text = drain(openai_sse_to_responses_stream(
            sse_body(&refs),
            ResponsesOutputOptions { model: "m".into(), tool_names: ResponsesToolNames::new() },
        ))
        .await;
        let evs = events(&text);
        let last = evs.last().expect("last");
        assert_eq!(last["type"], json!("response.incomplete"));
        assert_eq!(last["response"]["incomplete_details"], json!({ "reason": "max_output_tokens" }));
    }

    #[tokio::test]
    async fn completes_on_a_clean_eof_without_done() {
        let line = chat_chunk(json!({ "content": "x" }));
        let text = drain(openai_sse_to_responses_stream(
            sse_body(&[line.as_str()]),
            ResponsesOutputOptions { model: "m".into(), tool_names: ResponsesToolNames::new() },
        ))
        .await;
        assert_eq!(events(&text).last().expect("last")["type"], json!("response.completed"));
    }

    #[test]
    fn builds_the_same_items_from_a_non_stream_completion() {
        let out = openai_to_responses_object(
            &json!({
                "id": "chatcmpl_1",
                "created": 1700000000,
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Hi",
                        "reasoning_content": "r",
                        "tool_calls": [
                            { "id": "call_1", "type": "function", "function": { "name": "ns__t", "arguments": "{}" } },
                            { "id": "call_2", "type": "function", "function": { "name": "apply_patch", "arguments": "{\"input\":\"p\"}" } }
                        ]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": { "prompt_tokens": 3, "completion_tokens": 4 }
            }),
            &ResponsesOutputOptions {
                model: "raw/model".into(),
                tool_names: tool_names(&[
                    ("ns__t", ResponsesToolRef::function("t").namespaced("ns")),
                    ("apply_patch", ResponsesToolRef::custom("apply_patch")),
                ]),
            },
        );
        assert_eq!(out["object"], json!("response"));
        assert_eq!(out["status"], json!("completed"));
        assert_eq!(out["model"], json!("raw/model"));
        assert_eq!(out["created_at"], json!(1700000000));
        let output = out["output"].as_array().expect("output");
        assert_eq!(output.len(), 4);
        assert_eq!(output[0]["type"], json!("reasoning"));
        assert_eq!(output[0]["summary"], json!([{ "type": "summary_text", "text": "r" }]));
        assert_eq!(output[1]["type"], json!("message"));
        assert_eq!(output[1]["content"], json!([{ "type": "output_text", "text": "Hi", "annotations": [] }]));
        assert_eq!(output[2]["type"], json!("function_call"));
        assert_eq!(output[2]["call_id"], json!("call_1"));
        assert_eq!(output[2]["name"], json!("t"));
        assert_eq!(output[2]["namespace"], json!("ns"));
        assert_eq!(output[2]["arguments"], json!("{}"));
        assert_eq!(output[3]["type"], json!("custom_tool_call"));
        assert_eq!(output[3]["call_id"], json!("call_2"));
        assert_eq!(output[3]["name"], json!("apply_patch"));
        assert_eq!(output[3]["input"], json!("p"));
        assert_eq!(out["usage"], json!({ "input_tokens": 3, "output_tokens": 4, "total_tokens": 7 }));
    }

    #[test]
    fn emits_one_signature_marker_per_signed_non_stream_call() {
        let out = openai_to_responses_object(
            &json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [
                            { "id": "call_a", "type": "function", "function": { "name": "exec_command", "arguments": "{}" }, "thought_signature": "sig-a" },
                            { "id": "call_b", "type": "function", "function": { "name": "apply_patch", "arguments": "{\"input\":\"p\"}" }, "thought_signature": "sig-b" }
                        ]
                    },
                    "finish_reason": "tool_calls"
                }]
            }),
            &ResponsesOutputOptions {
                model: "antigravity/gemini-3-flash".into(),
                tool_names: tool_names(&[
                    ("exec_command", ResponsesToolRef::function("exec_command")),
                    ("apply_patch", ResponsesToolRef::custom("apply_patch")),
                ]),
            },
        );
        let output = out["output"].as_array().expect("output").clone();
        let types: Vec<&str> = output.iter().map(|i| i["type"].as_str().expect("type")).collect();
        assert_eq!(types, ["reasoning", "function_call", "reasoning", "custom_tool_call"]);
        let replay = responses_to_chat_request(&body_of(json!({
            "model": "antigravity/gemini-3-flash",
            "input": output
        })))
        .expect("replays");
        let calls = replay.chat["messages"][0]["tool_calls"].as_array().expect("calls").clone();
        assert_eq!(calls[0]["id"], json!("call_a"));
        assert_eq!(calls[0]["thought_signature"], json!("sig-a"));
        assert_eq!(calls[1]["id"], json!("call_b"));
        assert_eq!(calls[1]["thought_signature"], json!("sig-b"));
    }

    #[tokio::test]
    async fn rewrites_only_openai_error_lines() {
        let upstream = [
            ": keepalive",
            "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}",
            "data: {\"error\":{\"message\":\"upstream stalled: no data received for 120s\",\"type\":\"api_error\",\"code\":\"upstream_stall\"}}",
        ];
        let text =
            drain(rewrite_openai_error_frames_to_responses(sse_body(&upstream), "codex/gpt-5.4".into())).await;
        assert!(text.starts_with(": keepalive\n\nevent: response.created\ndata: {\"type\":\"response.created\""));
        let failed = events(&text)
            .into_iter()
            .find(|e| e["type"] == json!("response.failed"))
            .expect("response.failed");
        assert!(text.contains("event: response.failed\ndata: "));
        assert_eq!(
            failed["response"]["error"],
            json!({ "code": "upstream_stall", "message": "upstream stalled: no data received for 120s" })
        );
        assert_eq!(failed["response"]["model"], json!("codex/gpt-5.4"));
    }

    #[tokio::test]
    async fn collects_the_completed_response_object() {
        let out = collect_responses_sse(sse_body(&[
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"status\":\"in_progress\"}}",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}}",
        ]))
        .await;
        assert_eq!(
            out,
            CollectedResponses::Response(json!({
                "id": "resp_1",
                "status": "completed",
                "output": [],
                "usage": { "input_tokens": 1, "output_tokens": 2 }
            }))
        );
    }

    #[tokio::test]
    async fn reports_a_failed_turn_and_an_eof_without_completion() {
        let failed = collect_responses_sse(sse_body(&[
            "data: {\"type\":\"response.failed\",\"response\":{\"id\":\"resp_1\",\"status\":\"failed\",\"error\":{\"message\":\"rate limited\"}}}",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"output\":[]}}",
        ]))
        .await;
        assert_eq!(failed, CollectedResponses::Error { message: "rate limited".into() });
        assert_eq!(
            failed.error_value(),
            Some(json!({ "error": { "message": "rate limited", "type": "upstream_error" } }))
        );
        let eof =
            collect_responses_sse(sse_body(&["data: {\"type\":\"response.created\",\"response\":{}}"])).await;
        assert_eq!(
            eof,
            CollectedResponses::Error { message: "upstream ended without response.completed".into() }
        );
    }
}
