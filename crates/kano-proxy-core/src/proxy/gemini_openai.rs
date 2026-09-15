//! Port of apps/api/src/proxy/gemini_openai.ts (docs/providers.md § Antigravity,
//! docs/api.md § Conversions).
//!
//! OpenAI Chat Completions ↔ Gemini `GenerateContent`, for the antigravity
//! adapter's `/openai/v1` surface.
//!
//! Mirrors CLIProxyAPI
//! `internal/translator/antigravity/openai/chat-completions/*` for the wire
//! details: system messages become `systemInstruction`, assistant `tool_calls`
//! become `functionCall` parts and the matching `role: "tool"` messages become
//! `functionResponse` parts in a following `user` turn, `response_format`
//! becomes `responseMimeType` + `responseSchema`, and `reasoning_effort`
//! becomes `thinkingConfig`.

use std::collections::{HashMap, VecDeque};

use bytes::Bytes;
use serde_json::{json, Map, Value};

use crate::providers::types::ChatCompletionRequest;
use crate::providers::ProviderId;
use crate::proxy::gemini_wire::{
    block_reason, first_finish_reason, gemini_parts, has_no_candidates, inline_data_of,
    normalize_gemini_usage, openai_finish_reason, random_hex, sanitize_json_schema,
    schema_dialect_for, unwrap_antigravity_response, SchemaDialect, SseDataLines,
};
use crate::upstream::transport::ByteStream;
use crate::utils::audio::audio_inline;
use crate::utils::reasoning::map_reasoning;

// ── Request: OpenAI → Gemini ───────────────────────────────────────────────

/// `data:<mime>;base64,<payload>`; the mime may not contain `;` or `,`.
fn parse_data_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (mime, payload) = rest.split_once(";base64,")?;
    if mime.is_empty() || mime.contains(';') || mime.contains(',') || payload.is_empty() {
        return None;
    }
    Some((mime.to_string(), payload.to_string()))
}

/// JavaScript truthiness for a JSON value (`schema ? … : …`).
fn json_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn text_part(text: &str) -> Value {
    json!({ "text": text })
}

fn inline_part(mime_type: &str, data: &str) -> Value {
    json!({ "inlineData": { "mimeType": mime_type, "data": data } })
}

/// A remote `https://` image URL is dropped rather than forwarded: Gemini has
/// no URL image part on this endpoint, and fetching it server-side would make
/// the proxy an open fetcher.
fn image_url_part(url: Option<&Value>) -> Option<Value> {
    let url = url?.as_str()?;
    let (mime, data) = parse_data_url(url.trim())?;
    Some(inline_part(&mime, &data))
}

fn content_to_parts(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::String(s)) => {
            if s.is_empty() {
                vec![]
            } else {
                vec![text_part(s)]
            }
        }
        Some(Value::Array(items)) => {
            let mut parts = Vec::new();
            for raw in items {
                if !raw.is_object() {
                    continue;
                }
                match raw.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(text) = raw.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                parts.push(text_part(text));
                            }
                        }
                    }
                    Some("image_url") => {
                        if let Some(part) =
                            image_url_part(raw.get("image_url").and_then(|v| v.get("url")))
                        {
                            parts.push(part);
                        }
                    }
                    Some("input_audio") => {
                        // Gemini has one inline part for every modality; audio differs
                        // from an image only by mime (docs/api.md § Audio input). A format
                        // with no mime never gets here — the route already answered
                        // `unsupported_audio_format`.
                        if let Some(audio) = audio_inline(raw) {
                            parts.push(inline_part(&audio.mime_type, &audio.data));
                        }
                    }
                    _ => {}
                }
            }
            parts
        }
        _ => vec![],
    }
}

/// Tool results arrive as their own OpenAI messages; Gemini wants them inside a user turn.
fn tool_response_part(name: &str, call_id: &str, content: Option<&Value>) -> Value {
    let result = match content {
        Some(Value::String(s)) => {
            serde_json::from_str::<Value>(s).unwrap_or_else(|_| Value::String(s.clone()))
        }
        Some(other) => other.clone(),
        None => Value::Null,
    };
    let result = if result.is_null() { json!({}) } else { result };
    let mut response = Map::new();
    if !call_id.is_empty() {
        response.insert("id".into(), json!(call_id));
    }
    response.insert("name".into(), json!(name));
    response.insert("response".into(), json!({ "result": result }));
    json!({ "functionResponse": Value::Object(response) })
}

fn push_content(contents: &mut Vec<Value>, role: &str, parts: Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    // Gemini rejects two consecutive turns with the same role, and a tool result
    // following a user message is exactly that shape — merge instead.
    if let Some(last) = contents.last_mut() {
        if last.get("role").and_then(Value::as_str) == Some(role) {
            if let Some(existing) = last.get_mut("parts").and_then(Value::as_array_mut) {
                existing.extend(parts);
                return;
            }
        }
    }
    contents.push(json!({ "role": role, "parts": parts }));
}

fn messages_to_gemini(messages: &[Value]) -> (Vec<Value>, Option<Value>) {
    let mut system_parts: Vec<Value> = Vec::new();
    let mut contents: Vec<Value> = Vec::new();
    // tool_call_id → function name, so a later `role: "tool"` can be named.
    let mut call_names: HashMap<String, String> = HashMap::new();

    for message in messages {
        if !message.is_object() {
            continue;
        }
        let role = message.get("role").and_then(Value::as_str).unwrap_or("");
        let content = message.get("content");

        if role == "system" || role == "developer" {
            system_parts.extend(content_to_parts(content));
            continue;
        }

        if role == "user" {
            push_content(&mut contents, "user", content_to_parts(content));
            continue;
        }

        if role == "tool" {
            let call_id = message
                .get("tool_call_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            let name = call_names
                .get(call_id)
                .cloned()
                .or_else(|| message.get("name").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            push_content(&mut contents, "user", vec![tool_response_part(&name, call_id, content)]);
            continue;
        }

        if role == "assistant" {
            let mut parts: Vec<Value> = Vec::new();
            // The signature is Gemini's own `thoughtSignature` coming back; echoing it
            // is what keeps multi-turn thinking valid upstream. Gemini itself emits
            // signature-only thought parts (no text), and the response side exposes
            // exactly that shape — a replayed message whose `reasoning_content` is
            // empty but whose signature is set still counts.
            let reasoning_text = message
                .get("reasoning_content")
                .and_then(Value::as_str)
                .unwrap_or("");
            let reasoning_signature = message
                .get("reasoning_signature")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !reasoning_text.is_empty() || !reasoning_signature.is_empty() {
                let mut part = Map::new();
                part.insert("text".into(), json!(reasoning_text));
                part.insert("thought".into(), json!(true));
                if !reasoning_signature.is_empty() {
                    part.insert("thoughtSignature".into(), json!(reasoning_signature));
                }
                parts.push(Value::Object(part));
            }
            parts.extend(content_to_parts(content));
            let empty = Vec::new();
            let calls = message
                .get("tool_calls")
                .and_then(Value::as_array)
                .unwrap_or(&empty);
            for call in calls {
                let name = match call
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .filter(|n| !n.is_empty())
                {
                    Some(n) => n.to_string(),
                    None => continue,
                };
                let id = call.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                if !id.is_empty() {
                    call_names.insert(id.clone(), name.clone());
                }
                let raw_args = call
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                // A tool call the model never finished emitting is worse than useless
                // as a string here — send an empty object so the turn still validates.
                let args = if raw_args.trim().is_empty() {
                    json!({})
                } else {
                    serde_json::from_str::<Value>(raw_args).unwrap_or_else(|_| json!({}))
                };
                let mut function_call = Map::new();
                if !id.is_empty() {
                    function_call.insert("id".into(), json!(id));
                }
                function_call.insert("name".into(), json!(name));
                function_call.insert("args".into(), args);
                let mut part = Map::new();
                part.insert("functionCall".into(), Value::Object(function_call));
                // Gemini can sign the functionCall part itself; the echoed extension
                // restores the signature exactly where it came from.
                if let Some(sig) = call
                    .get("thought_signature")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    part.insert("thoughtSignature".into(), json!(sig));
                }
                parts.push(Value::Object(part));
            }
            push_content(&mut contents, "model", parts);
        }
    }
    crate::proxy::gemini_wire::open_with_user_turn(&mut contents);

    let system_instruction = if system_parts.is_empty() {
        None
    } else {
        Some(json!({ "role": "user", "parts": system_parts }))
    };
    (contents, system_instruction)
}

fn map_tools(tools: Option<&Value>, dialect: SchemaDialect) -> Option<Value> {
    let tools = tools?.as_array()?;
    let mut declarations: Vec<Value> = Vec::new();
    for raw in tools {
        if !raw.is_object() {
            continue;
        }
        if raw.get("type").and_then(Value::as_str) != Some("function") {
            continue;
        }
        let Some(function) = raw.get("function").filter(|f| f.is_object()) else {
            continue;
        };
        let name = match function
            .get("name")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty())
        {
            Some(n) => n,
            None => continue,
        };
        let parameters = match function.get("parameters") {
            Some(p) if !p.is_null() => sanitize_json_schema(p, dialect),
            _ => json!({ "type": "object", "properties": {} }),
        };
        let mut declaration = Map::new();
        declaration.insert("name".into(), json!(name));
        if let Some(description) = function.get("description").and_then(Value::as_str) {
            declaration.insert("description".into(), json!(description));
        }
        declaration.insert("parameters".into(), parameters);
        declarations.push(Value::Object(declaration));
    }
    if declarations.is_empty() {
        None
    } else {
        Some(json!([{ "functionDeclarations": declarations }]))
    }
}

fn map_tool_choice(tool_choice: Option<&Value>) -> Option<Value> {
    match tool_choice {
        Some(Value::String(s)) if s == "auto" => Some(json!({ "functionCallingConfig": { "mode": "AUTO" } })),
        Some(Value::String(s)) if s == "none" => Some(json!({ "functionCallingConfig": { "mode": "NONE" } })),
        Some(Value::String(s)) if s == "required" => Some(json!({ "functionCallingConfig": { "mode": "ANY" } })),
        Some(v) if v.is_object() || v.is_array() => {
            let name = v
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty())?;
            Some(json!({ "functionCallingConfig": { "mode": "ANY", "allowedFunctionNames": [name] } }))
        }
        _ => None,
    }
}

fn map_response_format(response_format: Option<&Value>, dialect: SchemaDialect) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(format) = response_format.filter(|v| v.is_object() || v.is_array()) else {
        return out;
    };
    match format.get("type").and_then(Value::as_str) {
        Some("json_object") => {
            out.insert("responseMimeType".into(), json!("application/json"));
        }
        Some("json_schema") => {
            out.insert("responseMimeType".into(), json!("application/json"));
            if let Some(schema) = format
                .get("json_schema")
                .and_then(|j| j.get("schema"))
                .filter(|s| json_truthy(s))
            {
                out.insert("responseSchema".into(), sanitize_json_schema(schema, dialect));
            }
        }
        _ => {}
    }
    out
}

/// Builds the inner `request` object. The adapter wraps it in the antigravity
/// envelope (`model` / `project` / `requestId` / `sessionId`).
pub fn openai_to_gemini_request(req: &ChatCompletionRequest) -> Value {
    // Claude behind Antigravity rejects a union its Gemini sibling accepts, so
    // the schema dialect follows the model family (gemini_wire.rs).
    let dialect = schema_dialect_for(&req.model);
    let (contents, system_instruction) = messages_to_gemini(&req.messages);
    let mut generation_config = Map::new();
    if let Some(t) = req.temperature {
        generation_config.insert("temperature".into(), json!(t));
    }
    if let Some(p) = req.top_p {
        generation_config.insert("topP".into(), json!(p));
    }
    if let Some(m) = req.max_tokens {
        generation_config.insert("maxOutputTokens".into(), json!(m));
    }
    if let Some(stop) = req.stop.as_ref().filter(|s| !s.is_empty()) {
        generation_config.insert("stopSequences".into(), json!(stop));
    }
    for (k, v) in map_response_format(req.response_format.as_ref(), dialect) {
        generation_config.insert(k, v);
    }

    let mapped = map_reasoning(ProviderId::Antigravity, req.reasoning_effort);
    match mapped.get("thinkingConfig") {
        Some(thinking) => {
            let disabled = thinking.get("thinkingBudget").is_some();
            let mut config = thinking.as_object().cloned().unwrap_or_default();
            // Without this the model still thinks but returns no thought parts, so
            // `reasoning_content` would silently vanish for every caller.
            config.insert("includeThoughts".into(), json!(!disabled));
            generation_config.insert("thinkingConfig".into(), Value::Object(config));
        }
        None => {
            generation_config.insert("thinkingConfig".into(), json!({ "includeThoughts": true }));
        }
    }

    let tools = map_tools(req.tools.as_ref(), dialect);
    let tool_config = tools
        .as_ref()
        .and_then(|_| map_tool_choice(req.tool_choice.as_ref()));

    let mut out = Map::new();
    out.insert("contents".into(), json!(contents));
    if let Some(system) = system_instruction {
        out.insert("systemInstruction".into(), system);
    }
    if let Some(tools) = tools {
        out.insert("tools".into(), tools);
    }
    if let Some(tool_config) = tool_config {
        out.insert("toolConfig".into(), tool_config);
    }
    if !generation_config.is_empty() {
        out.insert("generationConfig".into(), Value::Object(generation_config));
    }
    Value::Object(out)
}

// ── Response: Gemini → OpenAI ──────────────────────────────────────────────

fn tool_call_from_part(part: &Value, index: usize) -> Option<Value> {
    let call = part.get("functionCall")?;
    let name = call
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty())?;
    let id = call
        .get("id")
        .and_then(Value::as_str)
        .filter(|i| !i.is_empty())
        .map(str::to_string)
        // Gemini only sometimes returns an id; a stable synthetic one is required
        // because the client has to echo it back on the tool result.
        .unwrap_or_else(|| format!("call_{index}_{}", random_hex(16)));
    let args = call.get("args").cloned().unwrap_or(Value::Null);
    let args = if args.is_null() { json!({}) } else { args };
    let mut out = Map::new();
    out.insert("id".into(), json!(id));
    out.insert("index".into(), json!(index));
    out.insert("type".into(), json!("function"));
    out.insert(
        "function".into(),
        json!({ "name": name, "arguments": serde_json::to_string(&args).unwrap_or_else(|_| "{}".into()) }),
    );
    if let Some(sig) = part
        .get("thoughtSignature")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        out.insert("thought_signature".into(), json!(sig));
    }
    Some(Value::Object(out))
}

fn usage_to_openai(resp: Option<&Value>) -> Option<Value> {
    let usage = normalize_gemini_usage(resp.and_then(|r| r.get("usageMetadata")))?;
    let completion = if usage.completion_tokens.is_none() && usage.reasoning_tokens.is_none() {
        None
    } else {
        Some(usage.completion_tokens.unwrap_or(0) + usage.reasoning_tokens.unwrap_or(0))
    };
    let mut out = Map::new();
    out.insert("prompt_tokens".into(), json!(usage.prompt_tokens.unwrap_or(0)));
    out.insert("completion_tokens".into(), json!(completion.unwrap_or(0)));
    out.insert(
        "total_tokens".into(),
        json!(usage
            .total_tokens
            .unwrap_or_else(|| usage.prompt_tokens.unwrap_or(0) + completion.unwrap_or(0))),
    );
    if let Some(cached) = usage.cached_tokens {
        out.insert("prompt_tokens_details".into(), json!({ "cached_tokens": cached }));
    }
    if let Some(reasoning) = usage.reasoning_tokens {
        out.insert(
            "completion_tokens_details".into(),
            json!({ "reasoning_tokens": reasoning }),
        );
    }
    Some(Value::Object(out))
}

/// Non-stream `v1internal:generateContent` body → an OpenAI `chat.completion`.
pub fn gemini_response_to_openai(json_body: &Value, model: &str) -> Value {
    let resp = unwrap_antigravity_response(json_body);
    let parts = gemini_parts(resp);
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut reasoning_signature = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut images: Vec<Value> = Vec::new();

    for part in parts {
        if part.get("functionCall").is_some_and(|v| !v.is_null()) {
            if let Some(call) = tool_call_from_part(part, tool_calls.len()) {
                tool_calls.push(call);
            }
            continue;
        }
        if let Some(inline) = inline_data_of(part) {
            images.push(json!({
                "type": "image_url",
                "index": images.len(),
                "image_url": { "url": format!("data:{};base64,{}", inline.mime_type, inline.data) }
            }));
            continue;
        }
        // A signature can ride on a thought part with no visible text, and Gemini
        // signs plain text parts too (think-then-answer, no tool call) — both are
        // the turn's chain of thought, so both go out as `reasoning_signature`.
        // functionCall parts never reach here; they carry their own extension.
        if let Some(sig) = part
            .get("thoughtSignature")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            reasoning_signature = sig.to_string();
        }
        let text = part.get("text").and_then(Value::as_str).unwrap_or("");
        if text.is_empty() {
            continue;
        }
        if part.get("thought").and_then(Value::as_bool).unwrap_or(false) {
            reasoning.push_str(text);
        } else {
            content.push_str(text);
        }
    }

    // A safety-blocked prompt is a valid response with no candidates and a
    // `promptFeedback.blockReason` — surface it as a content filter, never as a
    // successful blank answer the client cannot tell apart from real output.
    let blocked = has_no_candidates(resp) && block_reason(resp).is_some();
    let finish_reason = if !tool_calls.is_empty() {
        "tool_calls"
    } else if blocked {
        "content_filter"
    } else {
        openai_finish_reason(first_finish_reason(resp))
    };
    let usage = usage_to_openai(resp);

    let mut message = Map::new();
    message.insert("role".into(), json!("assistant"));
    message.insert(
        "content".into(),
        if content.is_empty() { Value::Null } else { json!(content) },
    );
    if !reasoning.is_empty() {
        message.insert("reasoning_content".into(), json!(reasoning));
    }
    if !reasoning_signature.is_empty() {
        message.insert("reasoning_signature".into(), json!(reasoning_signature));
    }
    if !tool_calls.is_empty() {
        message.insert("tool_calls".into(), json!(tool_calls));
    }
    if !images.is_empty() {
        message.insert("images".into(), json!(images));
    }

    let id = resp
        .and_then(|r| r.get("responseId"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("chatcmpl_{}", random_hex(24)));

    let mut out = Map::new();
    out.insert("id".into(), json!(id));
    out.insert("object".into(), json!("chat.completion"));
    out.insert("created".into(), json!(crate::app::now_ms() / 1000));
    out.insert("model".into(), json!(model));
    out.insert(
        "choices".into(),
        json!([{ "index": 0, "message": Value::Object(message), "finish_reason": finish_reason }]),
    );
    if let Some(usage) = usage {
        out.insert("usage".into(), usage);
    }
    Value::Object(out)
}

// ── Streaming ──────────────────────────────────────────────────────────────

struct OpenAiStream {
    lines: SseDataLines,
    id: String,
    created: i64,
    model: String,
    out: VecDeque<Bytes>,
    finished: bool,
    tool_index: usize,
    image_index: usize,
    saw_tool_call: bool,
    finish_reason: Option<String>,
    /// A candidate reported a `finishReason` — Gemini's terminal frame. A clean
    /// EOF without one is a truncated stream, not a completion.
    saw_terminal: bool,
    block_reason: String,
    usage: Option<Value>,
    role_sent: bool,
}

impl OpenAiStream {
    fn emit(&mut self, payload: Value) {
        let text = format!("data: {}\n\n", serde_json::to_string(&payload).unwrap_or_default());
        self.out.push_back(Bytes::from(text));
    }

    fn chunk(&mut self, delta: Map<String, Value>) {
        let delta = if self.role_sent {
            delta
        } else {
            self.role_sent = true;
            let mut with_role = Map::new();
            with_role.insert("role".into(), json!("assistant"));
            for (k, v) in delta {
                with_role.insert(k, v);
            }
            with_role
        };
        let payload = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{ "index": 0, "delta": Value::Object(delta), "finish_reason": Value::Null }]
        });
        self.emit(payload);
    }

    /// A clean EOF is not a completion: without a terminal Gemini frame the
    /// stream was truncated (no error is raised for a quiet network close), so
    /// end the turn with the documented stream error, never a fabricated `stop`.
    /// A blocked prompt is the one no-terminal shape that *is* a real answer —
    /// Gemini reports it via promptFeedback with no candidate.
    fn terminal(&mut self) {
        if !self.saw_terminal && self.block_reason.is_empty() {
            self.emit(json!({
                "error": { "message": "upstream stream ended before completion", "type": "upstream_error" }
            }));
            return;
        }
        let terminal_finish = if self.saw_tool_call {
            "tool_calls".to_string()
        } else if !self.saw_terminal && !self.block_reason.is_empty() {
            "content_filter".to_string()
        } else {
            self.finish_reason.clone().unwrap_or_else(|| "stop".to_string())
        };
        let mut payload = Map::new();
        payload.insert("id".into(), json!(self.id));
        payload.insert("object".into(), json!("chat.completion.chunk"));
        payload.insert("created".into(), json!(self.created));
        payload.insert("model".into(), json!(self.model));
        payload.insert(
            "choices".into(),
            json!([{ "index": 0, "delta": {}, "finish_reason": terminal_finish }]),
        );
        if let Some(usage) = self.usage.clone() {
            payload.insert("usage".into(), usage);
        }
        self.emit(Value::Object(payload));
        self.out.push_back(Bytes::from_static(b"data: [DONE]\n\n"));
    }

    fn frame(&mut self, data: &str) {
        let Ok(json_value) = serde_json::from_str::<Value>(data) else {
            return;
        };
        let resp = unwrap_antigravity_response(&json_value);
        if let Some(reason) = block_reason(resp) {
            self.block_reason = reason.to_string();
        }
        for part in gemini_parts(resp).to_vec() {
            if part.get("functionCall").is_some_and(|v| !v.is_null()) {
                let Some(call) = tool_call_from_part(&part, self.tool_index) else {
                    continue;
                };
                self.tool_index += 1;
                self.saw_tool_call = true;
                let mut delta = Map::new();
                delta.insert("tool_calls".into(), json!([call]));
                self.chunk(delta);
                continue;
            }
            if let Some(inline) = inline_data_of(&part) {
                // Monotonic like the non-stream converter — clients that assemble
                // streamed images by index must not see them all collapse onto slot 0.
                let index = self.image_index;
                self.image_index += 1;
                let mut delta = Map::new();
                delta.insert(
                    "images".into(),
                    json!([{
                        "type": "image_url",
                        "index": index,
                        "image_url": { "url": format!("data:{};base64,{}", inline.mime_type, inline.data) }
                    }]),
                );
                self.chunk(delta);
                continue;
            }
            let text = part.get("text").and_then(Value::as_str).unwrap_or("");
            let signature = part
                .get("thoughtSignature")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            if part.get("thought").and_then(Value::as_bool).unwrap_or(false) {
                let mut delta = Map::new();
                if !text.is_empty() {
                    delta.insert("reasoning_content".into(), json!(text));
                }
                // The signature can ride on a thought part with no visible text.
                if let Some(sig) = signature {
                    delta.insert("reasoning_signature".into(), json!(sig));
                }
                if !delta.is_empty() {
                    self.chunk(delta);
                }
                continue;
            }
            if text.is_empty() {
                continue;
            }
            // A signature on a plain text part is the same chain of thought a thought
            // part carries — it goes out on this text delta, or it is lost for the
            // client's next replay.
            let mut delta = Map::new();
            delta.insert("content".into(), json!(text));
            if let Some(sig) = signature {
                delta.insert("reasoning_signature".into(), json!(sig));
            }
            self.chunk(delta);
        }
        if let Some(reason) = first_finish_reason(resp).filter(|r| !r.is_empty()) {
            self.saw_terminal = true;
            self.finish_reason = Some(openai_finish_reason(Some(reason)).to_string());
        }
        if let Some(usage) = usage_to_openai(resp) {
            self.usage = Some(usage);
        }
    }

    async fn step(&mut self) -> Option<Bytes> {
        loop {
            if let Some(bytes) = self.out.pop_front() {
                return Some(bytes);
            }
            if self.finished {
                return None;
            }
            match self.lines.next_data().await {
                None => {
                    self.finished = true;
                    self.terminal();
                }
                Some(Ok(data)) if data == "[DONE]" => {
                    self.finished = true;
                    self.terminal();
                }
                Some(Ok(data)) => self.frame(&data),
                // Mid-stream upstream failure: an OpenAI-shaped error line ends the
                // turn, never a fabricated successful finish (same rule as codex).
                Some(Err(e)) => {
                    self.finished = true;
                    self.emit(json!({ "error": { "message": e.to_string(), "type": "upstream_error" } }));
                }
            }
        }
    }
}

/// `v1internal:streamGenerateContent?alt=sse` → OpenAI `chat.completion.chunk`
/// SSE. Each upstream frame carries whole parts (Gemini streams by part, not by
/// token boundary within a part), so one frame becomes one chunk; the terminal
/// chunk carries `finish_reason` and `usage`.
///
/// Pull-driven: each poll consumes upstream frames only until it has one frame
/// to hand downstream, so a slow client applies backpressure to the paid
/// upstream generation. Dropping the returned stream drops the upstream body,
/// which is what the TypeScript `cancel()` hook did explicitly.
pub fn gemini_sse_to_openai_stream(body: ByteStream, model: &str) -> ByteStream {
    let state = OpenAiStream {
        lines: SseDataLines::new(body),
        id: format!("chatcmpl_{}", random_hex(24)),
        created: crate::app::now_ms() / 1000,
        model: model.to_string(),
        out: VecDeque::new(),
        finished: false,
        tool_index: 0,
        image_index: 0,
        saw_tool_call: false,
        finish_reason: None,
        saw_terminal: false,
        block_reason: String::new(),
        usage: None,
        role_sent: false,
    };
    Box::pin(futures::stream::unfold(state, |mut state| async move {
        state.step().await.map(|bytes| (Ok(bytes), state))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::reasoning::ReasoningEffort;
    use futures::StreamExt;

    fn req(patch: Value) -> ChatCompletionRequest {
        let mut r = ChatCompletionRequest {
            model: "antigravity/gemini-3-flash".into(),
            raw_model: "antigravity/gemini-3-flash".into(),
            upstream_model: "gemini-3-flash".into(),
            ..Default::default()
        };
        for (key, value) in patch.as_object().expect("patch is an object") {
            match key.as_str() {
                "model" => r.model = value.as_str().unwrap().to_string(),
                "messages" => r.messages = value.as_array().unwrap().clone(),
                "tools" => r.tools = Some(value.clone()),
                "tool_choice" => r.tool_choice = Some(value.clone()),
                "response_format" => r.response_format = Some(value.clone()),
                "temperature" => r.temperature = value.as_f64(),
                "top_p" => r.top_p = value.as_f64(),
                "max_tokens" => r.max_tokens = value.as_u64(),
                "stop" => {
                    r.stop = Some(
                        value
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|v| v.as_str().unwrap().to_string())
                            .collect(),
                    )
                }
                other => panic!("unhandled patch key {other}"),
            }
        }
        r
    }

    fn body_from(chunks: Vec<Vec<u8>>) -> ByteStream {
        Box::pin(futures::stream::iter(
            chunks
                .into_iter()
                .map(|c| Ok::<Bytes, std::io::Error>(Bytes::from(c)))
                .collect::<Vec<_>>(),
        ))
    }

    /// Frames as one SSE body, then split at awkward boundaries: every chunk is
    /// 7 bytes, so frames and JSON tokens are cut mid-way.
    fn sse(frames: &[Value]) -> ByteStream {
        let text: String = frames
            .iter()
            .map(|f| format!("data: {}\n\n", serde_json::to_string(f).unwrap()))
            .collect();
        let bytes = text.into_bytes();
        body_from(bytes.chunks(7).map(<[u8]>::to_vec).collect())
    }

    async fn read_sse(mut stream: ByteStream) -> String {
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.unwrap());
        }
        String::from_utf8(out).unwrap()
    }

    fn chunks_of(raw: &str) -> Vec<Value> {
        raw.split("\n\n")
            .map(|block| block.trim_start_matches("data:").trim())
            .filter(|d| !d.is_empty() && *d != "[DONE]")
            .map(|d| serde_json::from_str(d).unwrap())
            .collect()
    }

    fn delta_of(chunk: &Value) -> &Value {
        &chunk["choices"][0]["delta"]
    }

    fn declarations(out: &Value) -> &Value {
        &out["tools"][0]["functionDeclarations"]
    }

    // ── openaiToGeminiRequest ──────────────────────────────────────────────

    #[test]
    fn opens_an_assistant_first_history_with_a_user_turn() {
        let out = openai_to_gemini_request(&req(json!({
            "messages": [
                { "role": "assistant", "content": null, "tool_calls": [
                    { "id": "call_1", "type": "function", "function": { "name": "read_file", "arguments": "{\"path\":\"a.md\"}" } }
                ] },
                { "role": "tool", "tool_call_id": "call_1", "content": "# a" },
                { "role": "user", "content": "hi" }
            ]
        })));
        assert_eq!(
            out["contents"][0],
            json!({ "role": "user", "parts": [{ "text": "(conversation start)" }] })
        );
        assert_eq!(out["contents"][1]["role"], "model");
        assert_eq!(
            out["contents"][1]["parts"][0],
            json!({ "functionCall": { "id": "call_1", "name": "read_file", "args": { "path": "a.md" } } })
        );
        assert_eq!(out["contents"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn lifts_system_messages_into_system_instruction() {
        let out = openai_to_gemini_request(&req(json!({
            "messages": [
                { "role": "system", "content": "be terse" },
                { "role": "user", "content": "hi" },
                { "role": "assistant", "content": "hello" },
                { "role": "user", "content": "again" }
            ]
        })));
        assert_eq!(
            out["systemInstruction"],
            json!({ "role": "user", "parts": [{ "text": "be terse" }] })
        );
        assert_eq!(
            out["contents"],
            json!([
                { "role": "user", "parts": [{ "text": "hi" }] },
                { "role": "model", "parts": [{ "text": "hello" }] },
                { "role": "user", "parts": [{ "text": "again" }] }
            ])
        );
    }

    #[test]
    fn round_trips_a_tool_call_and_its_result() {
        let out = openai_to_gemini_request(&req(json!({
            "messages": [
                { "role": "user", "content": "weather?" },
                { "role": "assistant", "content": null, "tool_calls": [
                    { "id": "call_1", "type": "function", "function": { "name": "get_weather", "arguments": "{\"city\":\"Taipei\"}" } }
                ] },
                { "role": "tool", "tool_call_id": "call_1", "content": "{\"temp\":30}" }
            ]
        })));
        assert_eq!(
            out["contents"][1],
            json!({ "role": "model", "parts": [{ "functionCall": { "id": "call_1", "name": "get_weather", "args": { "city": "Taipei" } } }] })
        );
        assert_eq!(
            out["contents"][2],
            json!({ "role": "user", "parts": [{ "functionResponse": { "id": "call_1", "name": "get_weather", "response": { "result": { "temp": 30 } } } }] })
        );
    }

    #[test]
    fn replays_reasoning_content_with_its_signature() {
        let out = openai_to_gemini_request(&req(json!({
            "messages": [
                { "role": "user", "content": "hi" },
                { "role": "assistant", "content": "answer", "reasoning_content": "thought about it", "reasoning_signature": "sig-1" }
            ]
        })));
        assert_eq!(
            out["contents"][1]["parts"][0],
            json!({ "text": "thought about it", "thought": true, "thoughtSignature": "sig-1" })
        );
    }

    #[test]
    fn replays_a_signature_only_assistant_message() {
        let out = openai_to_gemini_request(&req(json!({
            "messages": [
                { "role": "user", "content": "hi" },
                { "role": "assistant", "content": "answer", "reasoning_signature": "sig-2" }
            ]
        })));
        assert_eq!(
            out["contents"][1]["parts"][0],
            json!({ "text": "", "thought": true, "thoughtSignature": "sig-2" })
        );
    }

    #[test]
    fn merges_consecutive_same_role_turns() {
        let out = openai_to_gemini_request(&req(json!({
            "messages": [
                { "role": "user", "content": "a" },
                { "role": "user", "content": "b" }
            ]
        })));
        assert_eq!(
            out["contents"],
            json!([{ "role": "user", "parts": [{ "text": "a" }, { "text": "b" }] }])
        );
    }

    #[test]
    fn inlines_a_base64_image_and_drops_a_remote_url() {
        let out = openai_to_gemini_request(&req(json!({
            "messages": [{ "role": "user", "content": [
                { "type": "text", "text": "what is this" },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAB" } },
                { "type": "image_url", "image_url": { "url": "https://example.com/a.png" } }
            ] }]
        })));
        assert_eq!(
            out["contents"][0]["parts"],
            json!([
                { "text": "what is this" },
                { "inlineData": { "mimeType": "image/png", "data": "AAAB" } }
            ])
        );
    }

    #[test]
    fn maps_response_format_json_schema() {
        let out = openai_to_gemini_request(&req(json!({
            "response_format": { "type": "json_schema", "json_schema": { "schema": {
                "type": "object",
                "additionalProperties": false,
                "properties": { "name": { "type": "string", "title": "Name" } }
            } } }
        })));
        assert_eq!(out["generationConfig"]["responseMimeType"], "application/json");
        // additionalProperties / title have no Gemini Schema field and are stripped.
        assert_eq!(
            out["generationConfig"]["responseSchema"],
            json!({ "type": "object", "properties": { "name": { "type": "string" } } })
        );
    }

    #[test]
    fn folds_the_two_nullable_spellings() {
        let out = openai_to_gemini_request(&req(json!({
            "model": "gemini-3-flash",
            "tools": [{ "type": "function", "function": { "name": "f", "parameters": {
                "type": "object",
                "properties": {
                    "mode": { "anyOf": [{ "type": "string", "enum": ["a"] }, { "type": "null" }] },
                    "size": { "type": ["integer", "null"], "minimum": 1 }
                }
            } } }]
        })));
        let params = &declarations(&out)[0]["parameters"];
        // Single surviving branch is inlined — no anyOf wrapper left behind.
        assert_eq!(
            params["properties"]["mode"],
            json!({ "type": "string", "enum": ["a"], "nullable": true })
        );
        assert_eq!(
            params["properties"]["size"],
            json!({ "type": "integer", "minimum": 1, "nullable": true })
        );
    }

    #[test]
    fn keeps_a_real_union_for_gemini_and_drops_it_for_claude() {
        let parameters = json!({
            "type": "object",
            "properties": { "v": { "anyOf": [{ "type": "string" }, { "type": "integer" }], "description": "d" } }
        });
        let params_for = |model: &str| {
            let out = openai_to_gemini_request(&req(json!({
                "model": model,
                "tools": [{ "type": "function", "function": { "name": "f", "parameters": parameters } }]
            })));
            declarations(&out)[0]["parameters"]["properties"]["v"].clone()
        };
        // Measured 2026-08-22: Gemini accepts a two-branch union; the Claude model
        // behind the same endpoint answers "input_schema: JSON schema is invalid".
        assert_eq!(
            params_for("gemini-3-flash"),
            json!({ "anyOf": [{ "type": "string" }, { "type": "integer" }], "description": "d" })
        );
        // Unconstrained but valid, and the description still guides the model.
        assert_eq!(params_for("claude-opus-4-6-thinking"), json!({ "description": "d" }));
    }

    #[test]
    fn gives_every_array_an_items_schema() {
        let parameters = json!({
            "type": "object",
            "properties": { "query": { "type": "object", "properties": {
                "where": { "type": "array", "maxItems": 10, "items": {
                    "type": "array",
                    "prefixItems": [{ "type": "string" }, { "type": "string", "enum": ["eq", "ne"] }, {}]
                } },
                "legacyTuple": { "type": "array", "items": [{ "type": "string" }, { "type": "integer" }] },
                "bare": { "type": "array", "description": "no items at all" },
                "nullableList": { "type": ["array", "null"] }
            } } }
        });
        let props_for = |model: &str| {
            let out = openai_to_gemini_request(&req(json!({
                "model": model,
                "tools": [{ "type": "function", "function": { "name": "f", "parameters": parameters } }]
            })));
            declarations(&out)[0]["parameters"]["properties"]["query"]["properties"].clone()
        };
        let expected = json!({
            "where": { "type": "array", "maxItems": 10, "items": { "type": "array", "items": {} } },
            "legacyTuple": { "type": "array", "items": {} },
            "bare": { "type": "array", "description": "no items at all", "items": {} },
            "nullableList": { "type": "array", "nullable": true, "items": {} }
        });
        assert_eq!(props_for("gemini-3.8-flash-high"), expected);
        assert_eq!(props_for("claude-opus-4-6-thinking"), expected);
    }

    #[test]
    fn strips_property_names_and_every_unknown_keyword() {
        let out = openai_to_gemini_request(&req(json!({
            "tools": [{ "type": "function", "function": { "name": "edit", "parameters": {
                "type": "object",
                "propertyNames": { "pattern": "^[a-z]+$" },
                "uniqueItems": true,
                "$comment": "ignore me",
                "dependentRequired": { "a": ["b"] },
                "properties": { "path": { "type": "string", "minLength": 1 } },
                "required": ["path"]
            } } }]
        })));
        assert_eq!(
            declarations(&out)[0]["parameters"],
            json!({
                "type": "object",
                "properties": { "path": { "type": "string", "minLength": 1 } },
                "required": ["path"]
            })
        );
    }

    #[test]
    fn keeps_the_schema_fields_gemini_supports() {
        let out = openai_to_gemini_request(&req(json!({
            "tools": [{ "type": "function", "function": { "name": "search", "parameters": {
                "type": "object",
                "properties": {
                    "q": { "type": "string", "pattern": "^.+$", "maxLength": 40, "example": "hi" },
                    "n": { "type": "integer", "minimum": 1, "maximum": 10, "format": "int32" },
                    "tags": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": 5 },
                    "mode": { "type": "string", "enum": ["a", "b"], "nullable": true }
                },
                "required": ["q"],
                "propertyOrdering": ["q", "n"],
                "minProperties": 1,
                "maxProperties": 4
            } } }]
        })));
        let params = declarations(&out)[0]["parameters"].clone();
        let mut keys: Vec<String> = params.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "maxProperties",
                "minProperties",
                "properties",
                "propertyOrdering",
                "required",
                "type"
            ]
        );
        assert_eq!(
            params["properties"]["n"],
            json!({ "type": "integer", "minimum": 1, "maximum": 10, "format": "int32" })
        );
    }

    #[test]
    fn keeps_a_property_named_like_a_stripped_keyword() {
        let out = openai_to_gemini_request(&req(json!({
            "tools": [{ "type": "function", "function": { "name": "annotate", "parameters": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "title": "Label" },
                    "default": { "type": "boolean" }
                },
                "required": ["title"]
            } } }]
        })));
        assert_eq!(
            declarations(&out)[0]["parameters"],
            json!({
                "type": "object",
                "properties": { "title": { "type": "string" }, "default": { "type": "boolean" } },
                "required": ["title"]
            })
        );
    }

    #[test]
    fn carries_a_tool_call_thought_signature_back_to_the_part() {
        let out = openai_to_gemini_request(&req(json!({
            "messages": [
                { "role": "user", "content": "go" },
                { "role": "assistant", "content": null, "tool_calls": [
                    { "id": "call_1", "type": "function", "function": { "name": "search", "arguments": "{}" }, "thought_signature": "sig-fc" }
                ] },
                { "role": "tool", "tool_call_id": "call_1", "content": "{}" }
            ]
        })));
        assert_eq!(
            out["contents"][1]["parts"][0],
            json!({ "functionCall": { "id": "call_1", "name": "search", "args": {} }, "thoughtSignature": "sig-fc" })
        );
    }

    #[test]
    fn maps_reasoning_effort_to_thinking_config() {
        let mut r = req(json!({}));
        r.reasoning_effort = Some(ReasoningEffort::Medium);
        assert_eq!(
            openai_to_gemini_request(&r)["generationConfig"]["thinkingConfig"],
            json!({ "thinkingLevel": "medium", "includeThoughts": true })
        );
    }

    #[test]
    fn clamps_an_above_ceiling_effort_down_to_high() {
        for effort in [ReasoningEffort::XHigh, ReasoningEffort::Max] {
            let mut r = req(json!({}));
            r.reasoning_effort = Some(effort);
            assert_eq!(
                openai_to_gemini_request(&r)["generationConfig"]["thinkingConfig"]["thinkingLevel"],
                "high"
            );
        }
    }

    #[test]
    fn effort_none_is_a_zero_budget_with_thoughts_off() {
        let mut r = req(json!({}));
        r.reasoning_effort = Some(ReasoningEffort::None);
        assert_eq!(
            openai_to_gemini_request(&r)["generationConfig"]["thinkingConfig"],
            json!({ "thinkingBudget": 0, "includeThoughts": false })
        );
    }

    #[test]
    fn carries_sampling_stops_and_max_tokens() {
        let out = openai_to_gemini_request(&req(json!({
            "temperature": 0.4, "top_p": 0.9, "max_tokens": 128, "stop": ["END"]
        })));
        let config = &out["generationConfig"];
        assert_eq!(config["temperature"], json!(0.4));
        assert_eq!(config["topP"], json!(0.9));
        assert_eq!(config["maxOutputTokens"], json!(128));
        assert_eq!(config["stopSequences"], json!(["END"]));
    }

    #[test]
    fn maps_tools_and_a_forced_tool_choice() {
        let out = openai_to_gemini_request(&req(json!({
            "tools": [{ "type": "function", "function": {
                "name": "search",
                "description": "look up",
                "parameters": { "type": "object", "properties": { "q": { "type": "string" } } }
            } }],
            "tool_choice": { "type": "function", "function": { "name": "search" } }
        })));
        assert_eq!(
            out["tools"],
            json!([{ "functionDeclarations": [{
                "name": "search",
                "description": "look up",
                "parameters": { "type": "object", "properties": { "q": { "type": "string" } } }
            }] }])
        );
        assert_eq!(
            out["toolConfig"],
            json!({ "functionCallingConfig": { "mode": "ANY", "allowedFunctionNames": ["search"] } })
        );
    }

    #[test]
    fn omits_tool_config_when_there_are_no_tools() {
        let out = openai_to_gemini_request(&req(json!({ "tool_choice": "auto" })));
        assert!(out.get("toolConfig").is_none());
    }

    // ── geminiResponseToOpenAI ─────────────────────────────────────────────

    #[test]
    fn splits_thought_parts_into_reasoning_content_and_maps_usage() {
        let out = gemini_response_to_openai(
            &json!({ "response": {
                "candidates": [{ "content": { "role": "model", "parts": [
                    { "text": "thinking about it", "thought": true },
                    { "text": "the answer" }
                ] }, "finishReason": "STOP" }],
                "usageMetadata": {
                    "promptTokenCount": 10, "candidatesTokenCount": 4, "thoughtsTokenCount": 6,
                    "cachedContentTokenCount": 3, "totalTokenCount": 20
                }
            } }),
            "antigravity/gemini-3-flash",
        );
        let choice = &out["choices"][0];
        assert_eq!(choice["message"]["content"], "the answer");
        assert_eq!(choice["message"]["reasoning_content"], "thinking about it");
        assert_eq!(choice["finish_reason"], "stop");
        assert_eq!(
            out["usage"],
            json!({
                "prompt_tokens": 10,
                // thoughts are billed output too, so both halves make up completion_tokens
                "completion_tokens": 10,
                "total_tokens": 20,
                "prompt_tokens_details": { "cached_tokens": 3 },
                "completion_tokens_details": { "reasoning_tokens": 6 }
            })
        );
    }

    #[test]
    fn emits_tool_calls_and_overrides_finish_reason() {
        let out = gemini_response_to_openai(
            &json!({ "response": { "candidates": [{
                "content": { "parts": [{ "functionCall": { "id": "fc1", "name": "search", "args": { "q": "x" } } }] },
                "finishReason": "STOP"
            }] } }),
            "antigravity/gemini-3-flash",
        );
        assert_eq!(out["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            out["choices"][0]["message"]["tool_calls"],
            json!([{ "id": "fc1", "index": 0, "type": "function", "function": { "name": "search", "arguments": "{\"q\":\"x\"}" } }])
        );
    }

    #[test]
    fn surfaces_the_thought_signature_as_reasoning_signature() {
        let out = gemini_response_to_openai(
            &json!({ "response": { "candidates": [{ "content": { "parts": [
                { "text": "hmm", "thought": true, "thoughtSignature": "sig-9" },
                { "text": "done" }
            ] }, "finishReason": "STOP" }] } }),
            "m",
        );
        assert_eq!(out["choices"][0]["message"]["reasoning_content"], "hmm");
        // The opaque signature must be exposed so the client can echo it back.
        assert_eq!(out["choices"][0]["message"]["reasoning_signature"], "sig-9");
    }

    #[test]
    fn surfaces_a_text_part_signature_as_reasoning_signature() {
        let out = gemini_response_to_openai(
            &json!({ "response": { "candidates": [{ "content": { "parts": [
                { "text": "reasoning", "thought": true },
                { "text": "answer", "thoughtSignature": "sig-t" }
            ] }, "finishReason": "STOP" }] } }),
            "m",
        );
        assert_eq!(out["choices"][0]["message"]["content"], "answer");
        assert_eq!(out["choices"][0]["message"]["reasoning_signature"], "sig-t");
    }

    #[test]
    fn maps_a_candidate_less_safety_block_to_content_filter() {
        let out = gemini_response_to_openai(
            &json!({ "response": { "promptFeedback": { "blockReason": "SAFETY" } } }),
            "m",
        );
        assert_eq!(out["choices"][0]["finish_reason"], "content_filter");
        assert_eq!(out["choices"][0]["message"]["content"], Value::Null);
    }

    #[test]
    fn exposes_a_function_call_signature_on_the_tool_call() {
        let out = gemini_response_to_openai(
            &json!({ "response": { "candidates": [{ "content": { "parts": [
                { "functionCall": { "id": "fc1", "name": "search", "args": {} }, "thoughtSignature": "sig-fc" }
            ] }, "finishReason": "STOP" }] } }),
            "m",
        );
        assert_eq!(
            out["choices"][0]["message"]["tool_calls"][0]["thought_signature"],
            "sig-fc"
        );
    }

    #[test]
    fn maps_a_safety_finish_to_content_filter() {
        let out = gemini_response_to_openai(
            &json!({ "response": { "candidates": [{ "content": { "parts": [] }, "finishReason": "SAFETY" }] } }),
            "m",
        );
        assert_eq!(out["choices"][0]["finish_reason"], "content_filter");
    }

    #[test]
    fn maps_max_tokens_to_length() {
        let out = gemini_response_to_openai(
            &json!({ "response": { "candidates": [{ "content": { "parts": [{ "text": "…" }] }, "finishReason": "MAX_TOKENS" }] } }),
            "m",
        );
        assert_eq!(out["choices"][0]["finish_reason"], "length");
    }

    // ── geminiSseToOpenAIStream ────────────────────────────────────────────

    #[tokio::test]
    async fn assembles_text_reasoning_and_a_terminal_usage_chunk() {
        let raw = read_sse(gemini_sse_to_openai_stream(
            sse(&[
                json!({ "response": { "candidates": [{ "content": { "parts": [{ "text": "think", "thought": true }] } }] } }),
                json!({ "response": { "candidates": [{ "content": { "parts": [{ "text": "Hel" }] } }] } }),
                json!({ "response": { "candidates": [{ "content": { "parts": [{ "text": "lo" }] } }] } }),
                json!({ "response": {
                    "candidates": [{ "finishReason": "STOP" }],
                    "usageMetadata": { "promptTokenCount": 5, "candidatesTokenCount": 2, "totalTokenCount": 7 }
                } }),
            ]),
            "antigravity/gemini-3-flash",
        ))
        .await;
        assert!(raw.ends_with("data: [DONE]\n\n"));
        let parsed = chunks_of(&raw);
        assert_eq!(delta_of(&parsed[0])["role"], "assistant");
        assert_eq!(delta_of(&parsed[0])["reasoning_content"], "think");
        assert_eq!(delta_of(&parsed[1])["content"], "Hel");
        assert_eq!(delta_of(&parsed[2])["content"], "lo");
        let final_chunk = parsed.last().unwrap();
        assert_eq!(final_chunk["choices"][0]["finish_reason"], "stop");
        assert_eq!(final_chunk["usage"]["prompt_tokens"], 5);
        assert_eq!(final_chunk["usage"]["completion_tokens"], 2);
    }

    #[tokio::test]
    async fn finishes_as_tool_calls_once_a_function_call_streamed() {
        let raw = read_sse(gemini_sse_to_openai_stream(
            sse(&[
                json!({ "response": { "candidates": [{ "content": { "parts": [{ "functionCall": { "name": "search", "args": { "q": "x" } } }] } }] } }),
                json!({ "response": { "candidates": [{ "finishReason": "STOP" }] } }),
            ]),
            "m",
        ))
        .await;
        let parsed = chunks_of(&raw);
        let call = &delta_of(&parsed[0])["tool_calls"][0];
        assert_eq!(call["index"], 0);
        assert_eq!(call["function"], json!({ "name": "search", "arguments": "{\"q\":\"x\"}" }));
        assert_eq!(
            parsed.last().unwrap()["choices"][0]["finish_reason"],
            "tool_calls"
        );
    }

    #[tokio::test]
    async fn streams_the_thought_signature_as_a_reasoning_signature_delta() {
        let raw = read_sse(gemini_sse_to_openai_stream(
            sse(&[
                json!({ "response": { "candidates": [{ "content": { "parts": [{ "text": "think", "thought": true, "thoughtSignature": "sig-1" }] } }] } }),
                json!({ "response": { "candidates": [{ "content": { "parts": [{ "text": "hi" }] }, "finishReason": "STOP" }] } }),
            ]),
            "m",
        ))
        .await;
        let parsed = chunks_of(&raw);
        assert_eq!(delta_of(&parsed[0])["reasoning_content"], "think");
        assert_eq!(delta_of(&parsed[0])["reasoning_signature"], "sig-1");
    }

    #[tokio::test]
    async fn streams_a_text_part_signature_alongside_its_content_delta() {
        let raw = read_sse(gemini_sse_to_openai_stream(
            sse(&[
                json!({ "response": { "candidates": [{ "content": { "parts": [{ "text": "reasoning", "thought": true }] } }] } }),
                json!({ "response": { "candidates": [{ "content": { "parts": [{ "text": "answer", "thoughtSignature": "sig-t" }] }, "finishReason": "STOP" }] } }),
            ]),
            "m",
        ))
        .await;
        let found = chunks_of(&raw).into_iter().any(|c| {
            delta_of(&c)["content"] == json!("answer") && delta_of(&c)["reasoning_signature"] == json!("sig-t")
        });
        assert!(found);
    }

    #[tokio::test]
    async fn gives_each_streamed_image_its_own_monotonic_index() {
        let image_frame = |data: &str| {
            json!({ "response": { "candidates": [{ "content": { "parts": [{ "inlineData": { "mimeType": "image/png", "data": data } }] } }] } })
        };
        let raw = read_sse(gemini_sse_to_openai_stream(
            sse(&[
                image_frame("AAA1"),
                image_frame("AAA2"),
                json!({ "response": { "candidates": [{ "finishReason": "STOP" }] } }),
            ]),
            "m",
        ))
        .await;
        let indexes: Vec<Value> = chunks_of(&raw)
            .iter()
            .filter(|c| delta_of(c)["images"].is_array())
            .map(|c| delta_of(c)["images"][0]["index"].clone())
            .collect();
        // Clients assembling streamed images by index must not see them all
        // collapse onto slot 0.
        assert_eq!(indexes, vec![json!(0), json!(1)]);
    }

    #[tokio::test]
    async fn ends_an_unterminated_stream_with_an_error_line() {
        let raw = read_sse(gemini_sse_to_openai_stream(
            sse(&[
                json!({ "response": { "candidates": [{ "content": { "parts": [{ "text": "par" }] } }] } }),
            ]),
            "m",
        ))
        .await;
        assert!(!raw.contains("[DONE]"));
        let parsed = chunks_of(&raw);
        assert_eq!(parsed.last().unwrap()["error"]["type"], "upstream_error");
    }

    #[tokio::test]
    async fn finishes_a_candidate_less_safety_block_as_content_filter() {
        let raw = read_sse(gemini_sse_to_openai_stream(
            sse(&[json!({ "response": { "promptFeedback": { "blockReason": "SAFETY" } } })]),
            "m",
        ))
        .await;
        assert!(raw.ends_with("data: [DONE]\n\n"));
        let parsed = chunks_of(&raw);
        assert_eq!(
            parsed.last().unwrap()["choices"][0]["finish_reason"],
            "content_filter"
        );
    }

    #[tokio::test]
    async fn ends_the_turn_on_a_mid_stream_upstream_error() {
        let body: ByteStream = Box::pin(futures::stream::iter(vec![
            Ok(Bytes::from_static(
                b"data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"a\"}]}}]}}\n\n",
            )),
            Err(std::io::Error::other("connection reset")),
        ]));
        let raw = read_sse(gemini_sse_to_openai_stream(body, "m")).await;
        let parsed = chunks_of(&raw);
        assert!(!raw.contains("[DONE]"));
        assert_eq!(parsed.last().unwrap()["error"]["type"], "upstream_error");
        assert_eq!(parsed.last().unwrap()["error"]["message"], "connection reset");
    }

    #[tokio::test]
    async fn does_not_drain_upstream_ahead_of_client_demand() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let served = Arc::new(AtomicUsize::new(0));
        let counter = served.clone();
        let body: ByteStream = Box::pin(futures::stream::unfold((), move |()| {
            let counter = counter.clone();
            async move {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                if n >= 20 {
                    return None;
                }
                let frame = json!({ "response": { "candidates": [{ "content": { "parts": [{ "text": format!("chunk-{n}") }] } }] } });
                let text = format!("data: {}\n\n", serde_json::to_string(&frame).unwrap());
                Some((Ok(Bytes::from(text)), ()))
            }
        }));
        let mut converted = gemini_sse_to_openai_stream(body, "m");
        let _ = converted.next().await;
        // The pull-driven pump must be waiting on downstream demand, not
        // buffering the remaining generation.
        tokio::task::yield_now().await;
        assert!(served.load(Ordering::SeqCst) < 6);
    }
}
