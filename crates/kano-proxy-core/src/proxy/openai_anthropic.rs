//! Port of apps/api/src/proxy/openai_anthropic.ts: OpenAI Chat Completions ↔ Anthropic
//! Messages conversion, both directions, streaming and non-streaming
//! (docs/api.md § Chat Completions ↔ Messages conversion, § Prompt cache).
//!
//! OpenAI→Claude: the converter itself adds no `cache_control`; the proxy-placed breakpoints
//! live in [`add_conversion_cache_control`]. Anthropic→OpenAI (non-Claude providers): strip all
//! `cache_control` (no equivalent).
//!
//! Both SSE converters are incremental: a `Stream<Item = Bytes>` in, a `Stream<Item = Bytes>`
//! out, line by line through [`crate::proxy::sse_lines`], piped with the bounded backpressure
//! [`crate::proxy::backpressure`] provides — the stream is never collected.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;

use rand::RngCore;
use serde_json::{json, Map, Value};

use crate::proxy::backpressure::{backpressured_byte_stream, Emitter};
use crate::proxy::sse_lines::read_sse_lines_default;
use crate::upstream::transport::ByteStream;

// ---------------------------------------------------------------------------
// Shared value helpers (JavaScript coercions the TypeScript relies on)
// ---------------------------------------------------------------------------

/// JavaScript truthiness: `undefined`/`null`/`false`/`0`/`""` are falsy.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(Value::String(s)) => !s.is_empty(),
        _ => true,
    }
}

/// `String(value)` for the shapes these converters actually see.
fn js_string(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

fn as_str(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str)
}

fn as_f64(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    }
}

fn as_i64(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        _ => None,
    }
}

fn array(value: Option<&Value>) -> Option<&Vec<Value>> {
    value.and_then(Value::as_array)
}

fn block_type(block: &Value) -> &str {
    block.get("type").and_then(Value::as_str).unwrap_or("")
}

/// `{...object, key: value}` — an existing key keeps its position, a new one is appended, which
/// is what the JSON the clients compare depends on.
fn with_key(source: &Map<String, Value>, key: &str, value: Value) -> Map<String, Value> {
    let mut out = source.clone();
    out.insert(key.to_string(), value);
    out
}

/// A JSON number that came from a JavaScript number literal.
fn number(value: f64) -> Value {
    serde_json::Number::from_f64(value).map(Value::Number).unwrap_or(Value::Null)
}

/// `crypto.randomUUID().replace(/-/g, "").slice(0, 24)`.
fn random_id_suffix() -> String {
    let mut bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn now_seconds() -> i64 {
    crate::app::now_ms() / 1000
}

// ---------------------------------------------------------------------------
// cache_control / structured output helpers
// ---------------------------------------------------------------------------

/// Deep-drop every `cache_control` key. Used only on Anthropic→OpenAI convert.
pub fn strip_cache_control(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(strip_cache_control).collect()),
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, v) in map {
                if key == "cache_control" {
                    continue;
                }
                out.insert(key.clone(), strip_cache_control(v));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// The client's structured-output request, from either Anthropic spelling: `output_config.format`
/// (current) wins over the retired top-level `output_format`, which Anthropic itself now rejects
/// but older SDKs and proxies still emit. Shared by every Anthropic-ingress converter.
pub fn anthropic_output_format(body: &Map<String, Value>) -> Option<&Value> {
    if let Some(config) = body.get("output_config").filter(|v| v.is_object()) {
        if let Some(format) = config.get("format").filter(|v| v.is_object() || v.is_array()) {
            return Some(format);
        }
    }
    body.get("output_format").filter(|v| v.is_object() || v.is_array())
}

/// Native-passthrough normalization: a retired top-level `output_format` moves to
/// `output_config.format` when the body has no current-spelling format of its own. Anthropic
/// `400`s the old field, and clients on older SDKs / proxies still send it. Returns the input
/// borrowed when nothing moves (the TypeScript returns the same object).
pub fn move_retired_output_format(body: &Map<String, Value>) -> Cow<'_, Map<String, Value>> {
    let Some(legacy) = body.get("output_format").filter(|v| v.is_object() || v.is_array()) else {
        return Cow::Borrowed(body);
    };
    let legacy = legacy.clone();
    let config = body
        .get("output_config")
        .filter(|v| v.is_object())
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut rest = body.clone();
    rest.remove("output_format");
    if config.get("format").map(Value::is_object).unwrap_or(false) {
        return Cow::Owned(rest);
    }
    rest.insert("output_config".to_string(), Value::Object(with_key(&config, "format", legacy)));
    Cow::Owned(rest)
}

/// Anthropic `metadata.user_id` → the internal request's `prompt_cache_key` (codex-only effect;
/// docs/api.md § Prompt cache). The Messages wire format has no `prompt_cache_key` field, but
/// Claude Code sends a stable per-session id here. Treated as an opaque string; the guard
/// mirrors Anthropic's own `metadata.user_id` limit (≤ 256 characters).
pub fn prompt_cache_key_from_anthropic_metadata(body: &Map<String, Value>) -> Option<String> {
    let metadata = body.get("metadata").filter(|v| v.is_object())?;
    let user_id = metadata.get("user_id").and_then(Value::as_str)?;
    let trimmed = user_id.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 256 {
        return None;
    }
    Some(trimmed.to_string())
}

// ---------------------------------------------------------------------------
// Anthropic Messages request → OpenAI Chat Completions
// ---------------------------------------------------------------------------

/// The OpenAI Chat Completions fields an Anthropic Messages request converts to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AnthropicToOpenAiChat {
    pub messages: Vec<Value>,
    pub max_tokens: Option<u64>,
    pub stream: bool,
    pub tools: Option<Value>,
    pub tool_choice: Option<Value>,
    pub response_format: Option<Value>,
    pub reasoning_effort: Option<Value>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub stop: Option<Vec<String>>,
}

/// `contentToText(content)`.
fn content_to_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|part| match part {
                Value::String(s) => s.clone(),
                Value::Object(map) if map.get("type").and_then(Value::as_str) == Some("text") => {
                    js_string(map.get("text"))
                }
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join(""),
        None | Some(Value::Null) => String::new(),
        Some(other) => other.to_string(),
    }
}

/// `anthropicBlocksToOpenAIContent(blocks)`: a plain string when every block was text, an
/// OpenAI content-part array otherwise.
fn anthropic_blocks_to_openai_content(blocks: &[Value]) -> Value {
    let mut parts: Vec<Value> = Vec::new();
    let mut only_text = true;
    let mut text_joined = String::new();
    for block in blocks {
        match block_type(block) {
            "text" => {
                let text = js_string(block.get("text"));
                text_joined.push_str(&text);
                parts.push(json!({ "type": "text", "text": text }));
            }
            "image" => {
                only_text = false;
                let source = block.get("source");
                let source_type = as_str(source.and_then(|s| s.get("type")));
                let data = as_str(source.and_then(|s| s.get("data")));
                let url = as_str(source.and_then(|s| s.get("url")));
                if source_type == Some("base64") {
                    if let Some(data) = data.filter(|d| !d.is_empty()) {
                        let media_type = as_str(source.and_then(|s| s.get("media_type")))
                            .filter(|m| !m.is_empty())
                            .unwrap_or("image/png");
                        parts.push(json!({
                            "type": "image_url",
                            "image_url": { "url": format!("data:{media_type};base64,{data}") }
                        }));
                    }
                } else if source_type == Some("url") {
                    if let Some(url) = url.filter(|u| !u.is_empty()) {
                        parts.push(json!({ "type": "image_url", "image_url": { "url": url } }));
                    }
                }
            }
            _ => {
                only_text = false;
                parts.push(block.clone());
            }
        }
    }
    if only_text {
        return Value::String(text_joined);
    }
    if parts.is_empty() {
        Value::String(text_joined)
    } else {
        Value::Array(parts)
    }
}

/// Anthropic `tool_result` → OpenAI `role: "tool"` message. OpenAI's tool message has no
/// multi-part (text + image) shape, so when the result contains `image` blocks the tool message
/// keeps the text plus a short placeholder line, and a separate `role: "user"` message carries
/// the image(s) right after it. A tool_result with no images is unchanged.
fn tool_result_to_openai(tool_use_id: Option<&Value>, content: Option<&Value>) -> (Value, Option<Value>) {
    let blocks = content.and_then(Value::as_array);
    let images: Vec<Value> = blocks
        .map(|b| b.iter().filter(|block| block_type(block) == "image").cloned().collect())
        .unwrap_or_default();
    let mut tool_message = Map::new();
    tool_message.insert("role".into(), json!("tool"));
    if let Some(id) = tool_use_id {
        tool_message.insert("tool_call_id".into(), id.clone());
    }
    let Some(blocks) = blocks.filter(|_| !images.is_empty()) else {
        tool_message.insert("content".into(), Value::String(content_to_text(content)));
        return (Value::Object(tool_message), None);
    };
    let text: String = blocks
        .iter()
        .filter(|b| block_type(b) == "text")
        .map(|b| js_string(b.get("text")))
        .collect::<Vec<_>>()
        .join("");
    let placeholder = if images.len() == 1 {
        "[image attached below]".to_string()
    } else {
        format!("[{} images attached below]", images.len())
    };
    let body = if text.is_empty() { placeholder.clone() } else { format!("{text}\n{placeholder}") };
    tool_message.insert("content".into(), Value::String(body));

    let image_parts = anthropic_blocks_to_openai_content(&images);
    let label = format!("[Image(s) from tool result {}]", js_string(tool_use_id));
    let mut content_parts = vec![json!({ "type": "text", "text": label })];
    if let Value::Array(parts) = image_parts {
        content_parts.extend(parts);
    }
    let image_message = json!({ "role": "user", "content": content_parts });
    (Value::Object(tool_message), Some(image_message))
}

/// `mapAnthropicToolChoice(tc)`.
fn map_anthropic_tool_choice(tool_choice: &Value) -> Value {
    let Some(map) = tool_choice.as_object() else { return tool_choice.clone() };
    match map.get("type").and_then(Value::as_str) {
        Some("auto") => json!("auto"),
        Some("none") => json!("none"),
        Some("any") => json!("required"),
        Some("tool") => match map.get("name").filter(|n| truthy(Some(n))) {
            Some(name) => json!({ "type": "function", "function": { "name": name } }),
            None => tool_choice.clone(),
        },
        _ => tool_choice.clone(),
    }
}

/// Anthropic Messages request → OpenAI Chat Completions fields. Strips all `cache_control`.
/// Does not invent affinity headers.
pub fn anthropic_to_openai_chat_request(body: &Map<String, Value>) -> AnthropicToOpenAiChat {
    let cleaned_value = strip_cache_control(&Value::Object(body.clone()));
    let cleaned = cleaned_value.as_object().cloned().unwrap_or_default();
    let mut messages: Vec<Value> = Vec::new();

    match cleaned.get("system") {
        Some(Value::String(system)) if !system.is_empty() => {
            messages.push(json!({ "role": "system", "content": system }));
        }
        Some(Value::Array(blocks)) => {
            // Separate system blocks are distinct instructions upstream; joining them with ""
            // would run the last line of one into the first of the next.
            let text = blocks
                .iter()
                .map(|b| if block_type(b) == "text" { js_string(b.get("text")) } else { String::new() })
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            if !text.is_empty() {
                messages.push(json!({ "role": "system", "content": text }));
            }
        }
        _ => {}
    }

    for message in array(cleaned.get("messages")).cloned().unwrap_or_default() {
        let role = js_string(message.get("role"));
        let content = message.get("content");
        if role == "assistant" {
            let Some(blocks) = content.and_then(Value::as_array) else {
                messages.push(json!({ "role": "assistant", "content": content_to_text(content) }));
                continue;
            };
            let mut text = String::new();
            let mut reasoning = String::new();
            let mut tool_calls: Vec<Value> = Vec::new();
            for block in blocks {
                match block_type(block) {
                    "text" => text.push_str(&js_string(block.get("text"))),
                    // Round-trip a prior converted turn's thinking block back into the de-facto
                    // reasoning_content field before replay upstream. redacted_thinking has no
                    // plaintext to round-trip and is dropped, not an error.
                    "thinking" => reasoning.push_str(&js_string(block.get("thinking"))),
                    "tool_use" => {
                        let mut call = Map::new();
                        if let Some(id) = block.get("id") {
                            call.insert("id".into(), id.clone());
                        }
                        call.insert("type".into(), json!("function"));
                        let mut function = Map::new();
                        if let Some(name) = block.get("name") {
                            function.insert("name".into(), name.clone());
                        }
                        let input = block.get("input").filter(|v| !v.is_null()).cloned().unwrap_or_else(|| json!({}));
                        function.insert("arguments".into(), Value::String(input.to_string()));
                        call.insert("function".into(), Value::Object(function));
                        tool_calls.push(Value::Object(call));
                    }
                    _ => {}
                }
            }
            let mut msg = Map::new();
            msg.insert("role".into(), json!("assistant"));
            msg.insert("content".into(), if text.is_empty() { Value::Null } else { Value::String(text) });
            if !reasoning.is_empty() {
                msg.insert("reasoning_content".into(), Value::String(reasoning));
            }
            if !tool_calls.is_empty() {
                msg.insert("tool_calls".into(), Value::Array(tool_calls));
            }
            messages.push(Value::Object(msg));
            continue;
        }
        if role == "user" {
            let blocks = content.and_then(Value::as_array);
            if let Some(blocks) = blocks.filter(|b| b.iter().any(|x| block_type(x) == "tool_result")) {
                for block in blocks {
                    if block_type(block) == "tool_result" {
                        let (tool_message, image_message) =
                            tool_result_to_openai(block.get("tool_use_id"), block.get("content"));
                        messages.push(tool_message);
                        if let Some(image_message) = image_message {
                            messages.push(image_message);
                        }
                    }
                }
                let non_tool: Vec<Value> =
                    blocks.iter().filter(|b| block_type(b) != "tool_result").cloned().collect();
                if !non_tool.is_empty() {
                    messages
                        .push(json!({ "role": "user", "content": anthropic_blocks_to_openai_content(&non_tool) }));
                }
                continue;
            }
            match blocks {
                Some(blocks) => messages
                    .push(json!({ "role": "user", "content": anthropic_blocks_to_openai_content(blocks) })),
                None => messages.push(json!({ "role": "user", "content": content_to_text(content) })),
            }
            continue;
        }
        // Pass through unknown roles best-effort.
        messages.push(json!({ "role": role, "content": content_to_text(content) }));
    }

    let mut out = AnthropicToOpenAiChat {
        messages,
        stream: truthy(cleaned.get("stream")),
        ..AnthropicToOpenAiChat::default()
    };
    if let Some(max_tokens) = as_i64(cleaned.get("max_tokens")) {
        out.max_tokens = u64::try_from(max_tokens).ok();
    }
    out.temperature = as_f64(cleaned.get("temperature"));
    out.top_p = as_f64(cleaned.get("top_p"));

    if let Some(sequences) = array(cleaned.get("stop_sequences")) {
        let stop: Vec<String> = sequences
            .iter()
            .filter_map(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        if !stop.is_empty() {
            out.stop = Some(stop);
        }
    }

    if let Some(tools_in) = array(cleaned.get("tools")) {
        let mut tools: Vec<Value> = Vec::new();
        for tool in tools_in {
            let Some(map) = tool.as_object() else {
                tools.push(tool.clone());
                continue;
            };
            if truthy(map.get("function")) {
                // Already OpenAI-shaped — pass through.
                tools.push(tool.clone());
                continue;
            }
            let has_input_schema = map.contains_key("input_schema");
            let tool_type = map.get("type").and_then(Value::as_str);
            if let Some(tool_type) = tool_type {
                if tool_type != "custom" && !has_input_schema {
                    // Anthropic server-side tool (web_search_*, bash_*, text_editor_*,
                    // computer_*, code_execution_*, …). Grok/codex cannot execute these, so
                    // drop it rather than forward a fake empty-schema function.
                    continue;
                }
            }
            if truthy(map.get("name")) {
                // Client-defined tool: has input_schema, or type is absent/"custom".
                let mut function = Map::new();
                function.insert("name".into(), map.get("name").cloned().unwrap_or(Value::Null));
                if let Some(description) = map.get("description") {
                    function.insert("description".into(), description.clone());
                }
                let parameters = map
                    .get("input_schema")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
                function.insert("parameters".into(), parameters);
                tools.push(json!({ "type": "function", "function": Value::Object(function) }));
                continue;
            }
            tools.push(tool.clone());
        }
        out.tools = Some(Value::Array(tools));
    }

    if let Some(tool_choice) = cleaned.get("tool_choice").filter(|v| truthy(Some(v))) {
        out.tool_choice = Some(map_anthropic_tool_choice(tool_choice));
    }

    if let Some(format) = anthropic_output_format(&cleaned) {
        if format.get("type").and_then(Value::as_str) == Some("json_schema") {
            if let Some(schema) = format.get("schema").filter(|v| truthy(Some(v))) {
                out.response_format = Some(json!({
                    "type": "json_schema",
                    "json_schema": { "name": "response", "schema": schema }
                }));
            }
        }
    }

    // Reasoning is effort-only: `thinking`/`budget_tokens` is never read here.
    // `reasoning_effort` is a nonstandard extension some Anthropic-shaped clients send
    // directly; `output_config.effort` (the native Anthropic field) is the fallback when that
    // is absent. Explicit reasoning_effort always wins.
    match cleaned.get("reasoning_effort").filter(|v| !v.is_null()) {
        Some(effort) => out.reasoning_effort = Some(effort.clone()),
        None => {
            if let Some(effort) = as_str(cleaned.get("output_config").and_then(|c| c.get("effort"))) {
                out.reasoning_effort = Some(Value::String(effort.to_string()));
            }
        }
    }

    out
}

// ---------------------------------------------------------------------------
// OpenAI chat.completion → Anthropic message
// ---------------------------------------------------------------------------

/// OpenAI-shaped `usage` → Anthropic semantics: `prompt_tokens` is cache-inclusive, Anthropic
/// `input_tokens` is not — so when the upstream reported `prompt_tokens_details.cached_tokens`,
/// subtract it out and report it as `cache_read_input_tokens` instead. Unchanged when no cache
/// details were reported (docs/api.md § Usage cache details on converted responses).
fn anthropic_usage_from_openai(usage: &Value) -> Value {
    let prompt = as_i64(usage.get("prompt_tokens")).unwrap_or(0);
    let output = as_i64(usage.get("completion_tokens")).unwrap_or(0);
    let details = usage.get("prompt_tokens_details");
    let cached = as_i64(details.and_then(|d| d.get("cached_tokens")));
    let cache_write = as_i64(details.and_then(|d| d.get("cache_write_tokens")));
    let input = prompt - cached.unwrap_or(0) - cache_write.unwrap_or(0);
    let mut out = Map::new();
    out.insert("input_tokens".into(), json!(input));
    out.insert("output_tokens".into(), json!(output));
    if let Some(cached) = cached {
        out.insert("cache_read_input_tokens".into(), json!(cached));
    }
    if let Some(cache_write) = cache_write {
        out.insert("cache_creation_input_tokens".into(), json!(cache_write));
    }
    Value::Object(out)
}

/// OpenAI `chat.completion` → Anthropic message object.
pub fn openai_to_anthropic_message(completion: &Map<String, Value>, model: &str) -> Map<String, Value> {
    let choice = array(completion.get("choices")).and_then(|c| c.first());
    let message = choice.and_then(|c| c.get("message"));
    let mut content: Vec<Value> = Vec::new();
    // The de-facto reasoning_content extension field (grok's include_reasoning output, or
    // codex's reasoning summary) becomes a leading, unsigned thinking block — no `signature`,
    // since this proxy has none to attach.
    if let Some(reasoning) = as_str(message.and_then(|m| m.get("reasoning_content"))).filter(|r| !r.is_empty()) {
        content.push(json!({ "type": "thinking", "thinking": reasoning }));
    }
    if let Some(text) = as_str(message.and_then(|m| m.get("content"))).filter(|t| !t.is_empty()) {
        content.push(json!({ "type": "text", "text": text }));
    }
    if let Some(tool_calls) = array(message.and_then(|m| m.get("tool_calls"))) {
        for call in tool_calls {
            let function = call.get("function");
            let arguments = as_str(function.and_then(|f| f.get("arguments")));
            let input = match arguments.filter(|a| !a.is_empty()) {
                Some(raw) => serde_json::from_str::<Value>(raw).unwrap_or_else(|_| {
                    let mut fallback = Map::new();
                    fallback.insert("raw".into(), json!(raw));
                    Value::Object(fallback)
                }),
                None => json!({}),
            };
            let mut block = Map::new();
            block.insert("type".into(), json!("tool_use"));
            if let Some(id) = call.get("id") {
                block.insert("id".into(), id.clone());
            }
            if let Some(name) = function.and_then(|f| f.get("name")) {
                block.insert("name".into(), name.clone());
            }
            block.insert("input".into(), input);
            content.push(Value::Object(block));
        }
    }
    let finish = as_str(choice.and_then(|c| c.get("finish_reason")));
    let stop_reason = match finish {
        Some("tool_calls") => "tool_use",
        Some("length") => "max_tokens",
        _ => "end_turn",
    };

    let usage = completion.get("usage").filter(|u| truthy(Some(u)));
    let mut out = Map::new();
    out.insert(
        "id".into(),
        Value::String(match completion.get("id").filter(|v| !v.is_null()) {
            Some(id) => js_string(Some(id)),
            None => format!("msg_{}", crate::app::now_ms()),
        }),
    );
    out.insert("type".into(), json!("message"));
    out.insert("role".into(), json!("assistant"));
    out.insert("model".into(), json!(model));
    out.insert(
        "content".into(),
        if content.is_empty() { json!([{ "type": "text", "text": "" }]) } else { Value::Array(content) },
    );
    out.insert("stop_reason".into(), json!(stop_reason));
    out.insert("stop_sequence".into(), Value::Null);
    out.insert(
        "usage".into(),
        match usage {
            Some(usage) => anthropic_usage_from_openai(usage),
            None => json!({ "input_tokens": 0, "output_tokens": 0 }),
        },
    );
    out
}

// ---------------------------------------------------------------------------
// OpenAI Chat Completions request → Anthropic Messages request
// ---------------------------------------------------------------------------

/// The fields `openaiToAnthropicMessages` reads off the internal chat request.
#[derive(Debug, Clone, Default)]
pub struct AnthropicMessagesInput {
    pub model: String,
    pub messages: Vec<Value>,
    pub max_tokens: u64,
    pub stream: Option<bool>,
    pub tools: Option<Value>,
    pub tool_choice: Option<Value>,
    pub response_format: Option<Value>,
    pub thinking: Option<Value>,
    pub output_config: Option<Value>,
    pub stop: Option<Vec<String>>,
    /// Clamped to Anthropic's [0, 1] — OpenAI's client-facing range is 0–2.
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
}

fn content_to_anthropic_blocks(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::String(text)) => vec![json!({ "type": "text", "text": text })],
        Some(Value::Array(parts)) => {
            let mut blocks: Vec<Value> = Vec::new();
            for part in parts {
                match block_type(part) {
                    "text" => {
                        let mut block = Map::new();
                        block.insert("type".into(), json!("text"));
                        if let Some(text) = part.get("text") {
                            block.insert("text".into(), text.clone());
                        }
                        blocks.push(Value::Object(block));
                    }
                    "image_url" => {
                        let url = as_str(part.get("image_url").and_then(|i| i.get("url"))).unwrap_or("");
                        match parse_data_url(url) {
                            Some((media_type, data)) => blocks.push(json!({
                                "type": "image",
                                "source": { "type": "base64", "media_type": media_type, "data": data }
                            })),
                            None => blocks.push(json!({ "type": "image", "source": { "type": "url", "url": url } })),
                        }
                    }
                    _ => blocks.push(part.clone()),
                }
            }
            if blocks.is_empty() {
                vec![json!({ "type": "text", "text": "" })]
            } else {
                blocks
            }
        }
        other => vec![json!({ "type": "text", "text": js_string(other) })],
    }
}

/// `/^data:([^;]+);base64,(.+)$/`.
fn parse_data_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("data:")?;
    let (media_type, rest) = rest.split_once(';')?;
    if media_type.is_empty() || media_type.contains(';') {
        return None;
    }
    let data = rest.strip_prefix("base64,")?;
    if data.is_empty() {
        return None;
    }
    Some((media_type, data))
}

/// `mapToolChoice(tc)` — OpenAI spelling → Anthropic spelling.
fn map_tool_choice(tool_choice: &Value) -> Value {
    if let Some(name) = tool_choice.as_str() {
        return match name {
            "required" => json!({ "type": "any" }),
            "none" => json!({ "type": "none" }),
            "auto" => json!({ "type": "auto" }),
            _ => tool_choice.clone(),
        };
    }
    if tool_choice.get("type").and_then(Value::as_str) == Some("function") {
        let mut out = Map::new();
        out.insert("type".into(), json!("tool"));
        if let Some(name) = tool_choice.get("function").and_then(|f| f.get("name")) {
            out.insert("name".into(), name.clone());
        }
        return Value::Object(out);
    }
    tool_choice.clone()
}

/// OpenAI Chat Completions request → Anthropic Messages request body. Adds no `cache_control` —
/// see [`add_conversion_cache_control`].
pub fn openai_to_anthropic_messages(input: &AnthropicMessagesInput) -> Map<String, Value> {
    let mut system_parts: Vec<Value> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();

    for message in &input.messages {
        let role = js_string(message.get("role"));
        let content = message.get("content");
        if role == "system" {
            system_parts.extend(content_to_anthropic_blocks(content));
            continue;
        }
        if role == "tool" {
            let mut result = Map::new();
            result.insert("type".into(), json!("tool_result"));
            if let Some(id) = message.get("tool_call_id") {
                result.insert("tool_use_id".into(), id.clone());
            }
            result.insert("content".into(), Value::String(content_to_text(content)));
            messages.push(json!({ "role": "user", "content": [Value::Object(result)] }));
            continue;
        }
        if role == "assistant" {
            let mut blocks: Vec<Value> = Vec::new();
            let text = content_to_text(content);
            if !text.is_empty() {
                blocks.push(json!({ "type": "text", "text": text }));
            }
            if let Some(tool_calls) = array(message.get("tool_calls")) {
                for call in tool_calls {
                    let function = call.get("function");
                    let arguments = as_str(function.and_then(|f| f.get("arguments")));
                    let args = match arguments.filter(|a| !a.is_empty()) {
                        Some(raw) => serde_json::from_str::<Value>(raw).unwrap_or_else(|_| {
                            let mut fallback = Map::new();
                            fallback.insert("raw".into(), json!(raw));
                            Value::Object(fallback)
                        }),
                        None => json!({}),
                    };
                    let mut block = Map::new();
                    block.insert("type".into(), json!("tool_use"));
                    if let Some(id) = call.get("id") {
                        block.insert("id".into(), id.clone());
                    }
                    if let Some(name) = function.and_then(|f| f.get("name")) {
                        block.insert("name".into(), name.clone());
                    }
                    block.insert("input".into(), args);
                    blocks.push(Value::Object(block));
                }
            }
            let content = if blocks.is_empty() { Value::String(text) } else { Value::Array(blocks) };
            messages.push(json!({ "role": "assistant", "content": content }));
            continue;
        }
        messages.push(json!({ "role": "user", "content": content_to_anthropic_blocks(content) }));
    }

    let mut body = Map::new();
    body.insert("model".into(), json!(input.model));
    body.insert("max_tokens".into(), json!(input.max_tokens));
    body.insert("messages".into(), Value::Array(messages));
    body.insert("stream".into(), json!(input.stream.unwrap_or(false)));

    // Sampling is constrained once thinking is on, and this surface always turns a client
    // effort into `output_config` — so forwarding a plain `temperature` + effort request would
    // 400 on a combination the client never asked for. Anthropic's rule: `temperature`/`top_k`
    // are incompatible with thinking, `top_p` only within [0.95, 1]. Thinking counts as on
    // unless explicitly disabled. Dropped rather than clamped.
    let thinking_disabled =
        input.thinking.as_ref().and_then(|t| t.get("type")).and_then(Value::as_str) == Some("disabled");
    let thinking = !thinking_disabled && (input.thinking.is_some() || input.output_config.is_some());
    if let Some(temperature) = input.temperature.filter(|_| !thinking) {
        body.insert("temperature".into(), number(temperature.clamp(0.0, 1.0)));
    }
    if let Some(top_p) = input.top_p.filter(|v| !thinking || (*v >= 0.95 && *v <= 1.0)) {
        body.insert("top_p".into(), number(top_p));
    }

    if system_parts.len() == 1 && block_type(&system_parts[0]) == "text" {
        body.insert("system".into(), system_parts[0].get("text").cloned().unwrap_or(Value::Null));
    } else if !system_parts.is_empty() {
        body.insert("system".into(), Value::Array(system_parts));
    }

    if let Some(tools) = input.tools.as_ref().and_then(Value::as_array) {
        let mapped: Vec<Value> = tools
            .iter()
            .map(|tool| match tool.get("function") {
                Some(function) if truthy(Some(function)) => {
                    let mut out = Map::new();
                    out.insert("name".into(), function.get("name").cloned().unwrap_or(Value::Null));
                    if let Some(description) = function.get("description") {
                        out.insert("description".into(), description.clone());
                    }
                    let parameters = function
                        .get("parameters")
                        .filter(|v| !v.is_null())
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
                    out.insert("input_schema".into(), parameters);
                    Value::Object(out)
                }
                _ => tool.clone(),
            })
            .collect();
        body.insert("tools".into(), Value::Array(mapped));
    }
    if let Some(tool_choice) = input.tool_choice.as_ref().filter(|v| truthy(Some(v))) {
        body.insert("tool_choice".into(), map_tool_choice(tool_choice));
    }
    if let Some(thinking) = &input.thinking {
        body.insert("thinking".into(), thinking.clone());
    }
    if let Some(output_config) = &input.output_config {
        body.insert("output_config".into(), output_config.clone());
    }
    if let Some(stop) = input.stop.as_ref().filter(|s| !s.is_empty()) {
        body.insert("stop_sequences".into(), json!(stop));
    }
    if let Some(response_format) = &input.response_format {
        if response_format.get("type").and_then(Value::as_str) == Some("json_schema") {
            if let Some(schema) =
                response_format.get("json_schema").and_then(|j| j.get("schema")).filter(|v| truthy(Some(v)))
            {
                // `output_config.format` is the current Anthropic field; the retired top-level
                // `output_format` is a hard 400 ("This field is deprecated").
                let existing =
                    body.get("output_config").and_then(Value::as_object).cloned().unwrap_or_default();
                let format = json!({ "type": "json_schema", "schema": schema });
                body.insert("output_config".into(), Value::Object(with_key(&existing, "format", format)));
            }
        }
        // json_object: best-effort, not always supported; left as an instruction-free skip.
    }
    // No cache_control here — see add_conversion_cache_control.
    body
}

// ---------------------------------------------------------------------------
// Proxy-placed prompt-cache breakpoints
// ---------------------------------------------------------------------------

/// Content block types Anthropic accepts a `cache_control` marker on.
const CACHEABLE_BLOCK_TYPES: [&str; 5] = ["text", "image", "document", "tool_use", "tool_result"];

fn cache_marker(one_hour: bool) -> Value {
    if one_hour {
        json!({ "type": "ephemeral", "ttl": "1h" })
    } else {
        json!({ "type": "ephemeral" })
    }
}

/// Copy of `message` with `marker` on its last cacheable block; `None` when it has none.
fn mark_last_cacheable_block(message: &Value, marker: &Value) -> Option<Value> {
    let map = message.as_object()?;
    match map.get("content") {
        Some(Value::String(text)) => {
            if text.is_empty() {
                return None;
            }
            let block = json!({ "type": "text", "text": text, "cache_control": marker });
            Some(Value::Object(with_key(map, "content", json!([block]))))
        }
        Some(Value::Array(content)) => {
            let mut blocks = content.clone();
            for index in (0..blocks.len()).rev() {
                let Some(block) = blocks[index].as_object() else { continue };
                if !CACHEABLE_BLOCK_TYPES.contains(&js_string(block.get("type")).as_str()) {
                    continue;
                }
                blocks[index] = Value::Object(with_key(block, "cache_control", marker.clone()));
                return Some(Value::Object(with_key(map, "content", Value::Array(blocks))));
            }
            None
        }
        _ => None,
    }
}

/// Proxy-placed prompt-cache breakpoints for the OpenAI → Anthropic conversion path
/// (docs/api.md § Prompt cache). The OpenAI wire cannot express `cache_control`, and agent
/// clients on this surface resend the whole conversation every tool round, so the proxy spends
/// Anthropic's four markers itself: the last tool and the last `system` block at the 1h TTL,
/// then the tails of the last two user turns. Message markers get the 1h TTL only when the
/// client declared a multi-request conversation via `prompt_cache_key`. Explicit block markers
/// only, never the top-level automatic field. Longer-TTL entries always precede shorter ones.
/// Call it after any system prepend. Pure: the input body is not mutated.
pub fn add_conversion_cache_control(body: &Map<String, Value>, conversation: bool) -> Map<String, Value> {
    let static_marker = cache_marker(true);
    let turn_marker = cache_marker(conversation);
    let mut out = body.clone();

    if let Some(tools) = out.get("tools").and_then(Value::as_array).filter(|t| !t.is_empty()) {
        let mut tools = tools.clone();
        let last = tools.len() - 1;
        if let Some(map) = tools[last].as_object() {
            tools[last] = Value::Object(with_key(map, "cache_control", static_marker.clone()));
            out.insert("tools".into(), Value::Array(tools));
        }
    }

    match out.get("system").cloned() {
        Some(Value::String(system)) => {
            if !system.is_empty() {
                out.insert(
                    "system".into(),
                    json!([{ "type": "text", "text": system, "cache_control": static_marker }]),
                );
            }
        }
        Some(Value::Array(blocks)) if !blocks.is_empty() => {
            let mut blocks = blocks;
            let last = blocks.len() - 1;
            if let Some(map) = blocks[last].as_object() {
                blocks[last] = Value::Object(with_key(map, "cache_control", static_marker.clone()));
                out.insert("system".into(), Value::Array(blocks));
            }
        }
        _ => {}
    }

    if let Some(messages) = out.get("messages").and_then(Value::as_array) {
        let mut messages = messages.clone();
        let mut marked = 0usize;
        for index in (0..messages.len()).rev() {
            if marked >= 2 {
                break;
            }
            let Some(map) = messages[index].as_object() else { continue };
            // First marker: the last message, whatever its role. Second: the previous user
            // turn's tail — the last request's own breakpoint.
            if marked == 1 && map.get("role").and_then(Value::as_str) != Some("user") {
                continue;
            }
            let Some(next) = mark_last_cacheable_block(&messages[index], &turn_marker) else { continue };
            messages[index] = next;
            marked += 1;
        }
        if marked > 0 {
            out.insert("messages".into(), Value::Array(messages));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Anthropic message → OpenAI chat.completion
// ---------------------------------------------------------------------------

pub fn anthropic_to_openai_response(message: &Map<String, Value>, model: &str) -> Map<String, Value> {
    let mut text = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    if let Some(content) = array(message.get("content")) {
        for block in content {
            match block_type(block) {
                "text" => text.push_str(&js_string(block.get("text"))),
                "tool_use" => {
                    let mut call = Map::new();
                    if let Some(id) = block.get("id") {
                        call.insert("id".into(), id.clone());
                    }
                    call.insert("type".into(), json!("function"));
                    let mut function = Map::new();
                    if let Some(name) = block.get("name") {
                        function.insert("name".into(), name.clone());
                    }
                    let input = block.get("input").filter(|v| !v.is_null()).cloned().unwrap_or_else(|| json!({}));
                    function.insert("arguments".into(), Value::String(input.to_string()));
                    call.insert("function".into(), Value::Object(function));
                    tool_calls.push(Value::Object(call));
                }
                _ => {}
            }
        }
    }
    let finish = match as_str(message.get("stop_reason")) {
        Some("tool_use") => "tool_calls",
        Some("max_tokens") => "length",
        _ => "stop",
    };

    let mut choice_message = Map::new();
    choice_message.insert("role".into(), json!("assistant"));
    choice_message.insert("content".into(), if text.is_empty() { Value::Null } else { Value::String(text) });
    if !tool_calls.is_empty() {
        choice_message.insert("tool_calls".into(), Value::Array(tool_calls));
    }

    let mut out = Map::new();
    out.insert(
        "id".into(),
        match message.get("id").filter(|v| !v.is_null()) {
            Some(id) => id.clone(),
            None => Value::String(format!("chatcmpl_{}", crate::app::now_ms())),
        },
    );
    out.insert("object".into(), json!("chat.completion"));
    out.insert("created".into(), json!(now_seconds()));
    out.insert("model".into(), json!(model));
    out.insert(
        "choices".into(),
        json!([{ "index": 0, "message": Value::Object(choice_message), "finish_reason": finish }]),
    );
    if let Some(usage) = message.get("usage").filter(|u| truthy(Some(u))) {
        let input = as_i64(usage.get("input_tokens")).unwrap_or(0);
        let cache_read = as_i64(usage.get("cache_read_input_tokens")).unwrap_or(0);
        let cache_creation = as_i64(usage.get("cache_creation_input_tokens")).unwrap_or(0);
        let output = as_i64(usage.get("output_tokens")).unwrap_or(0);
        let prompt = input + cache_read + cache_creation;
        // Attach the upstream cache numbers instead of discarding them (docs/api.md § Usage
        // cache details on converted responses). 0 is a valid, meaningful value here.
        out.insert(
            "usage".into(),
            json!({
                "prompt_tokens": prompt,
                "completion_tokens": output,
                "total_tokens": prompt + output,
                "prompt_tokens_details": { "cached_tokens": cache_read },
                "cache_creation_input_tokens": cache_creation,
            }),
        );
    }
    out
}

// ---------------------------------------------------------------------------
// OpenAI Chat Completions SSE → Anthropic Messages SSE
// ---------------------------------------------------------------------------

struct LiveTool {
    openai_index: i64,
    block_index: i64,
    id: String,
    id_synthesized: bool,
}

struct DeferredTool {
    openai_index: i64,
    id: Option<String>,
    name: Option<String>,
    args: String,
}

/// Incremental OpenAI → Anthropic SSE conversion. Handles text deltas, streamed tool_calls and
/// the reasoning/thinking mapping (required for Claude Code on /anthropic → grok|codex).
struct OpenAiToAnthropic {
    emitter: Emitter,
    model: String,
    msg_id: String,
    started: bool,
    text_block_open: bool,
    text_block_index: i64,
    thinking_block_open: bool,
    thinking_block_index: i64,
    /// Reasoning that arrives after a text/tool block is already open; flushed whole at finish.
    pending_thinking: String,
    next_block_index: i64,
    output_tokens: i64,
    stopped: bool,
    saw_tool_call: bool,
    /// finish_reason seen, mapped to an Anthropic stop_reason; `None` until then.
    stop_reason: Option<String>,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
    cache_read_input_tokens: Option<i64>,
    cache_creation_input_tokens: Option<i64>,
    /// Anthropic content blocks are strictly sequential, an OpenAI chunk stream is not: the
    /// first tool call streams live into an open block, text arriving while it is open is
    /// buffered, and every other tool call accumulates and is emitted complete at the end.
    live_tool: Option<LiveTool>,
    deferred_tools: Vec<DeferredTool>,
    /// Text that arrived while a tool block was open; emitted as its own block.
    pending_text: String,
    /// Tool names seen before their call could be opened (name-before-id).
    pending_tool_names: BTreeMap<i64, String>,
    /// Indices that already produced a block, so a re-sent name is not a new call.
    opened_tool_indexes: BTreeSet<i64>,
}

impl OpenAiToAnthropic {
    fn new(emitter: Emitter, model: &str) -> Self {
        Self {
            emitter,
            model: model.to_string(),
            msg_id: format!("msg_{}", random_id_suffix()),
            started: false,
            text_block_open: false,
            text_block_index: -1,
            thinking_block_open: false,
            thinking_block_index: -1,
            pending_thinking: String::new(),
            next_block_index: 0,
            output_tokens: 0,
            stopped: false,
            saw_tool_call: false,
            stop_reason: None,
            prompt_tokens: None,
            completion_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            live_tool: None,
            deferred_tools: Vec::new(),
            pending_text: String::new(),
            pending_tool_names: BTreeMap::new(),
            opened_tool_indexes: BTreeSet::new(),
        }
    }

    async fn emit_event(&self, event: &str, data: &Value) -> Result<(), io::Error> {
        self.emitter.enqueue_str(&format!("event: {event}\ndata: {data}\n\n")).await
    }

    fn input_tokens(&self) -> Option<i64> {
        self.prompt_tokens.map(|prompt| {
            prompt - self.cache_read_input_tokens.unwrap_or(0) - self.cache_creation_input_tokens.unwrap_or(0)
        })
    }

    /// `message_start.usage.input_tokens` is what an Anthropic client shows as its context size,
    /// so it carries the real count whenever the upstream has already reported one. OpenAI-shaped
    /// upstreams normally report usage only on a final chunk — after this event must be on the
    /// wire — and then it stays `0`; there is no honest number to invent and the stream cannot be
    /// buffered to wait for one.
    async fn ensure_start(&mut self) -> Result<(), io::Error> {
        if self.started {
            return Ok(());
        }
        self.started = true;
        let input = self.input_tokens().unwrap_or(0).max(0);
        let mut usage = Map::new();
        usage.insert("input_tokens".into(), json!(input));
        usage.insert("output_tokens".into(), json!(0));
        if let Some(value) = self.cache_read_input_tokens {
            usage.insert("cache_read_input_tokens".into(), json!(value));
        }
        if let Some(value) = self.cache_creation_input_tokens {
            usage.insert("cache_creation_input_tokens".into(), json!(value));
        }
        let data = json!({
            "type": "message_start",
            "message": {
                "id": self.msg_id,
                "type": "message",
                "role": "assistant",
                "model": self.model,
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": Value::Object(usage),
            }
        });
        self.emit_event("message_start", &data).await
    }

    async fn emit_block_stop(&self, index: i64) -> Result<(), io::Error> {
        self.emit_event("content_block_stop", &json!({ "type": "content_block_stop", "index": index })).await
    }

    async fn close_text_block(&mut self) -> Result<(), io::Error> {
        if !self.text_block_open {
            return Ok(());
        }
        let index = self.text_block_index;
        self.text_block_open = false;
        self.emit_block_stop(index).await
    }

    async fn close_tool_block(&mut self) -> Result<(), io::Error> {
        let Some(tool) = self.live_tool.take() else { return Ok(()) };
        self.emit_block_stop(tool.block_index).await
    }

    async fn close_thinking_block(&mut self) -> Result<(), io::Error> {
        if !self.thinking_block_open {
            return Ok(());
        }
        let index = self.thinking_block_index;
        self.thinking_block_open = false;
        self.emit_block_stop(index).await
    }

    /// Emit buffered reasoning as a complete thinking block of its own.
    async fn flush_pending_thinking(&mut self) -> Result<(), io::Error> {
        if self.pending_thinking.is_empty() {
            return Ok(());
        }
        let thinking = std::mem::take(&mut self.pending_thinking);
        let index = self.next_block_index;
        self.next_block_index += 1;
        self.emit_event(
            "content_block_start",
            &json!({ "type": "content_block_start", "index": index, "content_block": { "type": "thinking", "thinking": "" } }),
        )
        .await?;
        self.emit_event(
            "content_block_delta",
            &json!({ "type": "content_block_delta", "index": index, "delta": { "type": "thinking_delta", "thinking": thinking } }),
        )
        .await?;
        self.emit_block_stop(index).await
    }

    async fn append_thinking(&mut self, text: &str) -> Result<(), io::Error> {
        self.ensure_start().await?;
        // Reasoning normally precedes everything else. If a text or tool block is already open
        // (a late/out-of-order fragment), do not interleave — buffer it and flush as one
        // complete block at the end, same treatment as pending_text below.
        if self.text_block_open || self.live_tool.is_some() {
            self.pending_thinking.push_str(text);
            return Ok(());
        }
        if !self.thinking_block_open {
            self.thinking_block_index = self.next_block_index;
            self.next_block_index += 1;
            self.thinking_block_open = true;
            self.emit_event(
                "content_block_start",
                &json!({ "type": "content_block_start", "index": self.thinking_block_index, "content_block": { "type": "thinking", "thinking": "" } }),
            )
            .await?;
        }
        self.emit_event(
            "content_block_delta",
            &json!({ "type": "content_block_delta", "index": self.thinking_block_index, "delta": { "type": "thinking_delta", "thinking": text } }),
        )
        .await
    }

    /// Emit buffered post-tool text as a complete block of its own.
    async fn flush_pending_text(&mut self) -> Result<(), io::Error> {
        if self.pending_text.is_empty() {
            return Ok(());
        }
        let text = std::mem::take(&mut self.pending_text);
        let index = self.next_block_index;
        self.next_block_index += 1;
        self.emit_event(
            "content_block_start",
            &json!({ "type": "content_block_start", "index": index, "content_block": { "type": "text", "text": "" } }),
        )
        .await?;
        self.emit_event(
            "content_block_delta",
            &json!({ "type": "content_block_delta", "index": index, "delta": { "type": "text_delta", "text": text } }),
        )
        .await?;
        self.emit_block_stop(index).await
    }

    async fn append_text(&mut self, text: &str) -> Result<(), io::Error> {
        self.ensure_start().await?;
        // Reasoning (if any) always finishes before real content.
        self.close_thinking_block().await?;
        // Never close an open tool block for text — that would split its arguments JSON across
        // two blocks sharing one tool_use id.
        if self.live_tool.is_some() {
            self.pending_text.push_str(text);
            return Ok(());
        }
        if !self.text_block_open {
            self.text_block_index = self.next_block_index;
            self.next_block_index += 1;
            self.text_block_open = true;
            self.emit_event(
                "content_block_start",
                &json!({ "type": "content_block_start", "index": self.text_block_index, "content_block": { "type": "text", "text": "" } }),
            )
            .await?;
        }
        self.emit_event(
            "content_block_delta",
            &json!({ "type": "content_block_delta", "index": self.text_block_index, "delta": { "type": "text_delta", "text": text } }),
        )
        .await
    }

    async fn emit_args(&self, index: i64, args: &str) -> Result<(), io::Error> {
        self.emit_event(
            "content_block_delta",
            &json!({ "type": "content_block_delta", "index": index, "delta": { "type": "input_json_delta", "partial_json": args } }),
        )
        .await
    }

    fn fallback_tool_id(&self, openai_index: i64) -> String {
        format!("toolu_{openai_index}_{}", &self.msg_id[4..12])
    }

    /// A whole tool_use block at once, for calls that were held back.
    async fn emit_complete_tool_block(
        &mut self,
        openai_index: i64,
        id: Option<&str>,
        name: Option<&str>,
        args: &str,
    ) -> Result<(), io::Error> {
        let index = self.next_block_index;
        self.next_block_index += 1;
        self.saw_tool_call = true;
        let id = match id.filter(|i| !i.is_empty()) {
            Some(id) => id.to_string(),
            None => self.fallback_tool_id(openai_index),
        };
        let name = name.filter(|n| !n.is_empty()).unwrap_or("unknown");
        self.emit_event(
            "content_block_start",
            &json!({ "type": "content_block_start", "index": index, "content_block": { "type": "tool_use", "id": id, "name": name, "input": {} } }),
        )
        .await?;
        if !args.is_empty() {
            self.emit_args(index, args).await?;
        }
        self.emit_block_stop(index).await
    }

    fn take_name(&mut self, openai_index: i64, name: Option<&str>) -> Option<String> {
        let resolved = name
            .filter(|n| !n.is_empty())
            .map(str::to_string)
            .or_else(|| self.pending_tool_names.get(&openai_index).cloned());
        self.pending_tool_names.remove(&openai_index);
        resolved
    }

    async fn route_tool_fragment(
        &mut self,
        openai_index: i64,
        id: Option<&str>,
        name: Option<&str>,
        args: Option<&str>,
    ) -> Result<(), io::Error> {
        self.ensure_start().await?;
        // Once an index is deferred its later fragments must follow it, or a continuation would
        // be appended to whichever call is live instead.
        if let Some(position) = self.deferred_tools.iter().rposition(|t| t.openai_index == openai_index) {
            let distinct = {
                let deferred = &self.deferred_tools[position];
                match (id, deferred.id.as_deref()) {
                    (Some(id), Some(existing)) => !id.is_empty() && id != existing,
                    _ => false,
                }
            };
            if distinct {
                // A new id at a deferred index is a distinct call, not a continuation.
                let name = self.take_name(openai_index, name);
                self.deferred_tools.push(DeferredTool {
                    openai_index,
                    id: id.map(str::to_string),
                    name,
                    args: args.unwrap_or("").to_string(),
                });
                self.opened_tool_indexes.insert(openai_index);
                return Ok(());
            }
            let deferred = &mut self.deferred_tools[position];
            if let Some(id) = id.filter(|i| !i.is_empty()) {
                deferred.id = Some(id.to_string());
            }
            if deferred.name.is_none() {
                if let Some(name) = name.filter(|n| !n.is_empty()) {
                    deferred.name = Some(name.to_string());
                }
            }
            deferred.args.push_str(args.unwrap_or(""));
            return Ok(());
        }

        let continues_live = match &self.live_tool {
            // A late id for a block opened on arguments alone belongs to that call — its
            // block_start has already gone out under the synthesized id.
            Some(live) => {
                live.openai_index == openai_index
                    && (id.is_none() || id == Some(live.id.as_str()) || live.id_synthesized)
            }
            None => false,
        };
        if continues_live {
            if id.is_some() {
                if let Some(live) = self.live_tool.as_mut() {
                    live.id_synthesized = false;
                }
            }
            if let Some(args) = args.filter(|a| !a.is_empty()) {
                let index = self.live_tool.as_ref().expect("live tool").block_index;
                self.emit_args(index, args).await?;
            }
            return Ok(());
        }
        if self.live_tool.is_some() {
            // Another call while one is streaming: hold it back so the live block stays open and
            // its arguments stay in one piece.
            let name = self.take_name(openai_index, name);
            self.deferred_tools.push(DeferredTool {
                openai_index,
                id: id.map(str::to_string),
                name,
                args: args.unwrap_or("").to_string(),
            });
            self.opened_tool_indexes.insert(openai_index);
            return Ok(());
        }

        self.close_text_block().await?;
        self.close_thinking_block().await?;
        let block_index = self.next_block_index;
        self.next_block_index += 1;
        let resolved_id = match id.filter(|i| !i.is_empty()) {
            Some(id) => id.to_string(),
            None => self.fallback_tool_id(openai_index),
        };
        let resolved_name = self.take_name(openai_index, name).unwrap_or_else(|| "unknown".to_string());
        self.live_tool = Some(LiveTool {
            openai_index,
            block_index,
            id: resolved_id.clone(),
            id_synthesized: id.is_none(),
        });
        self.opened_tool_indexes.insert(openai_index);
        self.saw_tool_call = true;
        self.emit_event(
            "content_block_start",
            &json!({ "type": "content_block_start", "index": block_index, "content_block": { "type": "tool_use", "id": resolved_id, "name": resolved_name, "input": {} } }),
        )
        .await?;
        if let Some(args) = args.filter(|a| !a.is_empty()) {
            self.emit_args(block_index, args).await?;
        }
        Ok(())
    }

    async fn finish(&mut self, stop_reason: &str) -> Result<(), io::Error> {
        if self.stopped {
            return Ok(());
        }
        self.stopped = true;
        self.ensure_start().await?;
        self.close_text_block().await?;
        self.close_tool_block().await?;
        self.close_thinking_block().await?;
        self.flush_pending_text().await?;
        self.flush_pending_thinking().await?;
        for tool in std::mem::take(&mut self.deferred_tools) {
            self.emit_complete_tool_block(tool.openai_index, tool.id.as_deref(), tool.name.as_deref(), &tool.args)
                .await?;
        }
        // A call announced by name only, with no id and no arguments, still has to reach the
        // client — as the zero-argument call it is. An index that already produced a block is a
        // re-sent name, not a call.
        for (openai_index, name) in std::mem::take(&mut self.pending_tool_names) {
            if self.opened_tool_indexes.contains(&openai_index) {
                continue;
            }
            self.emit_complete_tool_block(openai_index, None, Some(&name), "").await?;
        }
        if self.next_block_index == 0 {
            // Upstream sent no usable events. An SSE response carrying no content block is a
            // protocol error to Anthropic clients, which retry the turn; emit a well-formed
            // empty message instead.
            let index = self.next_block_index;
            self.emit_event(
                "content_block_start",
                &json!({ "type": "content_block_start", "index": index, "content_block": { "type": "text", "text": "" } }),
            )
            .await?;
            self.next_block_index += 1;
            self.emit_block_stop(index).await?;
        }
        // Prefer tool_use if we streamed any tools even when finish_reason was missing.
        let reason = if stop_reason == "end_turn" && self.saw_tool_call { "tool_use" } else { stop_reason };
        // Anthropic input_tokens excludes cached reads; OpenAI prompt_tokens does not.
        let input_tokens = self.input_tokens();
        let mut usage = Map::new();
        if let Some(input_tokens) = input_tokens {
            usage.insert("input_tokens".into(), json!(input_tokens));
        }
        // Real upstream counts when the provider reports them; the character estimate is only a
        // floor for providers that report nothing.
        usage.insert("output_tokens".into(), json!(self.completion_tokens.unwrap_or(self.output_tokens)));
        if let Some(value) = self.cache_read_input_tokens {
            usage.insert("cache_read_input_tokens".into(), json!(value));
        }
        if let Some(value) = self.cache_creation_input_tokens {
            usage.insert("cache_creation_input_tokens".into(), json!(value));
        }
        self.emit_event(
            "message_delta",
            &json!({
                "type": "message_delta",
                "delta": { "stop_reason": reason, "stop_sequence": null },
                "usage": Value::Object(usage),
            }),
        )
        .await?;
        self.emit_event("message_stop", &json!({ "type": "message_stop" })).await
    }

    /// `Math.max(1, Math.ceil(text.length / 4))` — the character estimate used when the upstream
    /// reports no usage at all.
    fn estimate_tokens(text: &str) -> i64 {
        let len = text.chars().count() as u64;
        std::cmp::max(1, len.div_ceil(4)) as i64
    }

    async fn handle_line(&mut self, line: &str) -> Result<(), io::Error> {
        let Some(rest) = line.strip_prefix("data:") else { return Ok(()) };
        let data = rest.trim().to_string();
        if data.is_empty() {
            return Ok(());
        }
        if data == "[DONE]" {
            let reason = self
                .stop_reason
                .clone()
                .unwrap_or_else(|| if self.saw_tool_call { "tool_use".into() } else { "end_turn".into() });
            return self.finish(&reason).await;
        }
        let Ok(json) = serde_json::from_str::<Value>(&data) else { return Ok(()) };
        if self.stopped {
            return Ok(());
        }
        // A top-level `error` object with no `choices` (a codex mid-turn failure relayed as an
        // OpenAI-shaped error line) has no Anthropic chunk equivalent — surface it as its own
        // event and end the message here; no message_delta/message_stop after it.
        if let Some(error) = json.get("error").filter(|e| truthy(Some(e))) {
            if json.get("choices").is_none() {
                let message = as_str(error.get("message")).filter(|m| !m.is_empty()).unwrap_or("upstream error");
                self.emit_event(
                    "error",
                    &json!({ "type": "error", "error": { "type": "api_error", "message": message } }),
                )
                .await?;
                self.stopped = true;
                return Ok(());
            }
        }
        // Usage rides on a final chunk that carries no choices, so read it before the choice
        // guard below discards that chunk.
        if let Some(usage) = json.get("usage").filter(|u| truthy(Some(u))) {
            if let Some(value) = as_i64(usage.get("prompt_tokens")) {
                self.prompt_tokens = Some(value);
            }
            if let Some(value) = as_i64(usage.get("completion_tokens")) {
                self.completion_tokens = Some(value);
            }
            let details = usage.get("prompt_tokens_details");
            if let Some(value) = as_i64(details.and_then(|d| d.get("cached_tokens"))) {
                self.cache_read_input_tokens = Some(value);
            }
            if let Some(value) = as_i64(details.and_then(|d| d.get("cache_write_tokens"))) {
                self.cache_creation_input_tokens = Some(value);
            }
        }
        let Some(choice) = array(json.get("choices")).and_then(|c| c.first()).cloned() else { return Ok(()) };
        // The turn is over once finish_reason lands; anything after it would have to be emitted
        // past message_stop.
        if self.stop_reason.is_some() || self.stopped {
            return Ok(());
        }
        let delta = choice.get("delta");
        // Reasoning (grok include_reasoning / codex reasoning summary) arrives before real
        // content — see append_thinking above.
        if let Some(reasoning) = as_str(delta.and_then(|d| d.get("reasoning_content"))).filter(|r| !r.is_empty()) {
            let reasoning = reasoning.to_string();
            self.output_tokens += Self::estimate_tokens(&reasoning);
            self.append_thinking(&reasoning).await?;
        }
        if let Some(content) = as_str(delta.and_then(|d| d.get("content"))).filter(|c| !c.is_empty()) {
            let content = content.to_string();
            self.output_tokens += Self::estimate_tokens(&content);
            self.append_text(&content).await?;
        }

        if let Some(tool_calls) = array(delta.and_then(|d| d.get("tool_calls"))).cloned() {
            for call in tool_calls {
                let openai_index = as_i64(call.get("index")).unwrap_or(0);
                let function = call.get("function");
                let id = as_str(call.get("id")).map(str::to_string);
                let name = as_str(function.and_then(|f| f.get("name"))).map(str::to_string);
                let args = as_str(function.and_then(|f| f.get("arguments"))).map(str::to_string);
                // A name with no id yet is not enough to open a block: doing so burns a fallback
                // id that the real id would then have to supersede with a second, empty block.
                if id.is_some() || args.is_some() {
                    if let Some(args) = args.as_deref().filter(|a| !a.is_empty()) {
                        self.output_tokens += Self::estimate_tokens(args);
                    }
                    self.route_tool_fragment(openai_index, id.as_deref(), name.as_deref(), args.as_deref())
                        .await?;
                } else if let Some(name) = name.filter(|n| !n.is_empty()) {
                    // Name alone cannot open a block yet; remember it for the fragment that can.
                    self.pending_tool_names.insert(openai_index, name);
                }
            }
        }

        if let Some(finish_reason) = choice.get("finish_reason").filter(|f| truthy(Some(f))) {
            // Record it, but do not close the message yet: the usage chunk rides after this one,
            // before [DONE].
            self.stop_reason = Some(
                match finish_reason.as_str() {
                    Some("tool_calls") => "tool_use",
                    Some("length") => "max_tokens",
                    _ => "end_turn",
                }
                .to_string(),
            );
        }
        Ok(())
    }
}

/// OpenAI Chat Completions SSE → Anthropic Messages SSE.
pub fn openai_sse_to_anthropic_stream(body: ByteStream, model: &str) -> ByteStream {
    let model = model.to_string();
    backpressured_byte_stream(move |emitter| async move {
        let mut converter = OpenAiToAnthropic::new(emitter, &model);
        let mut lines = read_sse_lines_default(body);
        use futures::StreamExt;
        while let Some(line) = lines.next().await {
            converter.handle_line(&line?).await?;
        }
        if !converter.stopped {
            let reason = converter
                .stop_reason
                .clone()
                .unwrap_or_else(|| if converter.saw_tool_call { "tool_use".into() } else { "end_turn".into() });
            converter.finish(&reason).await?;
        }
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Anthropic Messages SSE → OpenAI Chat Completions SSE
// ---------------------------------------------------------------------------

struct AnthropicToOpenAi {
    emitter: Emitter,
    model: String,
    id: String,
    sent_role: bool,
    finish_reason: Option<String>,
    /// Anthropic content block index → OpenAI `tool_calls` index (only tool_use blocks).
    tool_index_by_block: HashMap<i64, i64>,
    next_tool_index: i64,
    /// The SSE event name must survive chunk boundaries: the `event:` line and its `data:` line
    /// routinely arrive in different reads.
    event: String,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
    saw_usage: bool,
    /// Cache numbers ride on message_start's usage (the input side); `None` until it is seen,
    /// then always a number (Anthropic usage always defines these fields).
    cache_read_input_tokens: Option<i64>,
    cache_creation_input_tokens: Option<i64>,
    /// Set once the message has concluded — normally or via a mid-stream `event: error`.
    stopped: bool,
}

impl AnthropicToOpenAi {
    fn new(emitter: Emitter, model: &str) -> Self {
        Self {
            emitter,
            model: model.to_string(),
            id: format!("chatcmpl_{}", random_id_suffix()),
            sent_role: false,
            finish_reason: None,
            tool_index_by_block: HashMap::new(),
            next_tool_index: 0,
            event: String::new(),
            prompt_tokens: None,
            completion_tokens: None,
            saw_usage: false,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            stopped: false,
        }
    }

    async fn emit(&self, value: &Value) -> Result<(), io::Error> {
        self.emitter.enqueue_str(&format!("data: {value}\n\n")).await
    }

    fn chunk(&self, delta: Value, finish_reason: Value) -> Value {
        json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": now_seconds(),
            "model": self.model,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }],
        })
    }

    async fn ensure_role(&mut self) -> Result<(), io::Error> {
        if self.sent_role {
            return Ok(());
        }
        let chunk = self.chunk(json!({ "role": "assistant", "content": "" }), Value::Null);
        self.emit(&chunk).await?;
        self.sent_role = true;
        Ok(())
    }

    async fn handle_line(&mut self, line: &str) -> Result<(), io::Error> {
        if let Some(rest) = line.strip_prefix("event:") {
            self.event = rest.trim().to_string();
            return Ok(());
        }
        let Some(rest) = line.strip_prefix("data:") else { return Ok(()) };
        let data = rest.trim().to_string();
        if data.is_empty() {
            return Ok(());
        }
        if let Ok(json) = serde_json::from_str::<Value>(&data) {
            if self.stopped {
                self.event = String::new();
                return Ok(());
            }
            match self.event.as_str() {
                "message_start" => {
                    if let Some(usage) = json.get("message").and_then(|m| m.get("usage")).filter(|u| truthy(Some(u)))
                    {
                        // Same summation anthropic_to_openai_response uses for the non-stream
                        // response: cache tokens count as prompt tokens.
                        let cache_read = as_i64(usage.get("cache_read_input_tokens")).unwrap_or(0);
                        let cache_creation = as_i64(usage.get("cache_creation_input_tokens")).unwrap_or(0);
                        self.prompt_tokens =
                            Some(as_i64(usage.get("input_tokens")).unwrap_or(0) + cache_read + cache_creation);
                        self.cache_read_input_tokens = Some(cache_read);
                        self.cache_creation_input_tokens = Some(cache_creation);
                        self.saw_usage = true;
                    }
                }
                "content_block_start" => {
                    let block = json.get("content_block");
                    let block_index = as_i64(json.get("index")).unwrap_or(0);
                    if as_str(block.and_then(|b| b.get("type"))) == Some("tool_use") {
                        self.ensure_role().await?;
                        let tool_index = self.next_tool_index;
                        self.next_tool_index += 1;
                        self.tool_index_by_block.insert(block_index, tool_index);
                        let mut function = Map::new();
                        if let Some(name) = block.and_then(|b| b.get("name")) {
                            function.insert("name".into(), name.clone());
                        }
                        function.insert("arguments".into(), json!(""));
                        let mut call = Map::new();
                        call.insert("index".into(), json!(tool_index));
                        if let Some(id) = block.and_then(|b| b.get("id")) {
                            call.insert("id".into(), id.clone());
                        }
                        call.insert("type".into(), json!("function"));
                        call.insert("function".into(), Value::Object(function));
                        let chunk = self.chunk(json!({ "tool_calls": [Value::Object(call)] }), Value::Null);
                        self.emit(&chunk).await?;
                    }
                }
                "content_block_delta" => {
                    let delta = json.get("delta");
                    let block_index = as_i64(json.get("index")).unwrap_or(0);
                    let delta_type = as_str(delta.and_then(|d| d.get("type")));
                    if delta_type == Some("text_delta") {
                        if let Some(text) = as_str(delta.and_then(|d| d.get("text"))).filter(|t| !t.is_empty()) {
                            let text = text.to_string();
                            self.ensure_role().await?;
                            let chunk = self.chunk(json!({ "content": text }), Value::Null);
                            self.emit(&chunk).await?;
                        }
                    } else if delta_type == Some("input_json_delta") {
                        if let Some(partial) = as_str(delta.and_then(|d| d.get("partial_json"))) {
                            let partial = partial.to_string();
                            let tool_index = self.tool_index_by_block.get(&block_index).copied().unwrap_or(0);
                            self.ensure_role().await?;
                            let chunk = self.chunk(
                                json!({ "tool_calls": [{ "index": tool_index, "function": { "arguments": partial } }] }),
                                Value::Null,
                            );
                            self.emit(&chunk).await?;
                        }
                    }
                }
                "message_delta" => {
                    match as_str(json.get("delta").and_then(|d| d.get("stop_reason"))) {
                        Some("tool_use") => self.finish_reason = Some("tool_calls".into()),
                        Some("max_tokens") => self.finish_reason = Some("length".into()),
                        Some(reason) if !reason.is_empty() => self.finish_reason = Some("stop".into()),
                        _ => {}
                    }
                    if let Some(usage) = json.get("usage").filter(|u| truthy(Some(u))) {
                        if let Some(output) = as_i64(usage.get("output_tokens")) {
                            self.completion_tokens = Some(output);
                            self.saw_usage = true;
                        }
                        // Newer API revisions repeat the input-side fields here. `input_tokens`
                        // is the *uncached* count, so it must be merged field-wise and re-summed
                        // with the cache fields — never assigned straight into the
                        // cache-inclusive prompt total.
                        if let Some(value) = as_i64(usage.get("cache_read_input_tokens")) {
                            self.cache_read_input_tokens = Some(value);
                        }
                        if let Some(value) = as_i64(usage.get("cache_creation_input_tokens")) {
                            self.cache_creation_input_tokens = Some(value);
                        }
                        if let Some(input) = as_i64(usage.get("input_tokens")) {
                            self.prompt_tokens = Some(
                                input
                                    + self.cache_read_input_tokens.unwrap_or(0)
                                    + self.cache_creation_input_tokens.unwrap_or(0),
                            );
                            self.saw_usage = true;
                        }
                    }
                }
                "message_stop" => {
                    let finish = self.finish_reason.clone().unwrap_or_else(|| "stop".into());
                    let mut payload =
                        self.chunk(json!({}), Value::String(finish)).as_object().cloned().unwrap_or_default();
                    // Usage rides on the finish chunk, only when some count was actually seen.
                    if self.saw_usage {
                        let prompt = self.prompt_tokens.unwrap_or(0);
                        let completion = self.completion_tokens.unwrap_or(0);
                        let mut usage = Map::new();
                        usage.insert("prompt_tokens".into(), json!(prompt));
                        usage.insert("completion_tokens".into(), json!(completion));
                        usage.insert("total_tokens".into(), json!(prompt + completion));
                        // Only known once message_start's usage was actually seen.
                        if let Some(cache_read) = self.cache_read_input_tokens {
                            usage.insert("prompt_tokens_details".into(), json!({ "cached_tokens": cache_read }));
                            usage.insert(
                                "cache_creation_input_tokens".into(),
                                json!(self.cache_creation_input_tokens.unwrap_or(0)),
                            );
                        }
                        payload.insert("usage".into(), Value::Object(usage));
                    }
                    self.emit(&Value::Object(payload)).await?;
                    self.emitter.enqueue_str("data: [DONE]\n\n").await?;
                    self.stopped = true;
                }
                "error" => {
                    let error = json.get("error");
                    let message =
                        as_str(error.and_then(|e| e.get("message"))).filter(|m| !m.is_empty()).unwrap_or("upstream error");
                    let error_type =
                        as_str(error.and_then(|e| e.get("type"))).filter(|t| !t.is_empty()).unwrap_or("api_error");
                    self.emit(&json!({ "error": { "message": message, "type": error_type } })).await?;
                    self.stopped = true;
                }
                _ => {}
            }
        }
        self.event = String::new();
        Ok(())
    }
}

/// Anthropic Messages SSE → OpenAI Chat Completions SSE. Handles `text_delta` and `tool_use`
/// (`input_json_delta`) streaming.
pub fn anthropic_sse_to_openai_stream(body: ByteStream, model: &str) -> ByteStream {
    let model = model.to_string();
    backpressured_byte_stream(move |emitter| async move {
        let mut converter = AnthropicToOpenAi::new(emitter, &model);
        let mut lines = read_sse_lines_default(body);
        use futures::StreamExt;
        while let Some(line) = lines.next().await {
            converter.handle_line(&line?).await?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests;
