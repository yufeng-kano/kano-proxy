//! Grok ↔ Anthropic conversion (docs/providers.md § Grok,
//! docs/api.md § "Grok reasoning").
//!
//! Anthropic Messages ↔ xAI Responses for `/anthropic` → grok. Maps
//! `reasoning.encrypted_content` ↔ `thinking.signature`; never invents Claude-native
//! signatures. Streaming conversion is incremental: upstream bytes are split into SSE
//! lines and each line is turned into Anthropic events without buffering the turn.

use std::collections::HashMap;

use bytes::Bytes;
use futures::stream::{Stream, StreamExt};
use serde_json::{json, Map, Value};

use crate::providers::grok_encrypted_content::is_valid_grok_encrypted_content;
use crate::providers::ProviderId;
use crate::proxy::anthropic_client_tools;
use crate::proxy::gemini_wire::{truthy, UPSTREAM_HEARTBEAT};
use crate::proxy::openai_anthropic::{anthropic_output_format, strip_cache_control};
use crate::proxy::sse_lines::read_sse_lines_default;
use crate::utils::reasoning::{map_reasoning, parse_reasoning_effort, ReasoningEffort};

// ── Request conversion ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GrokThinkingMode {
    Disabled,
    Enabled,
    #[default]
    Default,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GrokAnthropicConvertResult {
    pub body: Value,
    pub thinking_mode: GrokThinkingMode,
}

/// The adapter turns this into a `400 invalid_request_error`, keeping convert pure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid reasoning_effort")]
pub struct InvalidGrokReasoningEffortError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GrokThinkingResolution {
    thinking_mode: GrokThinkingMode,
    effort: Option<ReasoningEffort>,
}

/// Resolve whether to expose encrypted reasoning and which effort to send.
/// `thinking.type=disabled` is authoritative — effort fields are ignored.
/// `budget_tokens` is intentionally ignored (effort-only).
fn resolve_grok_thinking_effort(body: &Value) -> GrokThinkingResolution {
    let thinking_type = body
        .get("thinking")
        .filter(|t| t.is_object())
        .and_then(|t| t.get("type"))
        .and_then(Value::as_str)
        .map(str::to_lowercase)
        .unwrap_or_default();

    // Disabled wins over any effort field — do not map effort when off.
    if thinking_type == "disabled" {
        return GrokThinkingResolution { thinking_mode: GrokThinkingMode::Disabled, effort: None };
    }

    let raw_effort: Option<&Value> = match body.get("reasoning_effort") {
        Some(v) if !v.is_null() => Some(v),
        _ => body.get("output_config").and_then(|c| c.get("effort")).filter(|e| e.is_string()),
    };
    let effort = parse_reasoning_effort(raw_effort).effort();

    if thinking_type == "enabled" || thinking_type == "adaptive" || thinking_type == "auto" {
        return GrokThinkingResolution {
            thinking_mode: GrokThinkingMode::Enabled,
            effort: Some(effort.unwrap_or(ReasoningEffort::Medium)),
        };
    }
    if effort.is_some() {
        return GrokThinkingResolution { thinking_mode: GrokThinkingMode::Enabled, effort };
    }
    // No thinking object and no effort: still ask for encrypted reasoning so
    // Claude Code multi-turn can continue when the client omits thinking config.
    GrokThinkingResolution { thinking_mode: GrokThinkingMode::Default, effort: None }
}

/// Pure builder — unit-testable without network. `replay_encrypted_content` is injected
/// from the replay cache when the client omitted the signature but the session continues.
pub fn anthropic_to_grok_responses(
    body: &Value,
    upstream_model: &str,
    replay_encrypted_content: Option<&str>,
) -> Result<GrokAnthropicConvertResult, InvalidGrokReasoningEffortError> {
    let cleaned = strip_cache_control(body);
    let GrokThinkingResolution { thinking_mode, effort } = resolve_grok_thinking_effort(&cleaned);
    if parse_reasoning_effort(cleaned.get("reasoning_effort")).is_invalid() {
        return Err(InvalidGrokReasoningEffortError);
    }
    let config_effort = cleaned.get("output_config").and_then(|c| c.get("effort")).filter(|e| e.is_string());
    if config_effort.is_some()
        && cleaned.get("reasoning_effort").is_none_or(Value::is_null)
        && parse_reasoning_effort(config_effort).is_invalid()
    {
        return Err(InvalidGrokReasoningEffortError);
    }

    let mapped = map_reasoning(ProviderId::Grok, effort);
    let replay = if thinking_mode != GrokThinkingMode::Disabled { replay_encrypted_content } else { None };
    let Converted { input, instructions } = anthropic_messages_to_grok_input(&cleaned, replay);

    let mut out = Map::new();
    out.insert("model".into(), Value::String(upstream_model.to_string()));
    out.insert("input".into(), Value::Array(input));
    out.insert("stream".into(), Value::Bool(true));
    out.insert("store".into(), Value::Bool(false));
    if !instructions.is_empty() {
        out.insert("instructions".into(), Value::String(instructions));
    }

    // Disabled: omit include and reasoning entirely — do not send effort "none"
    // (unverified on xAI) and do not honor a concurrent output_config.effort.
    if thinking_mode != GrokThinkingMode::Disabled {
        out.insert("include".into(), json!(["reasoning.encrypted_content"]));
        if let Some(e) = mapped.get("reasoning_effort").and_then(Value::as_str) {
            if e != "none" {
                out.insert("reasoning".into(), json!({ "effort": e }));
            }
        }
    }

    if let Some(max_tokens) = cleaned.get("max_tokens").filter(|v| v.is_number()) {
        out.insert("max_output_tokens".into(), max_tokens.clone());
    }
    // Pin the shared surface default (docs/providers.md).
    let temperature = cleaned.get("temperature").filter(|v| v.is_number()).cloned().unwrap_or(json!(1));
    out.insert("temperature".into(), temperature);
    if let Some(top_p) = cleaned.get("top_p").filter(|v| v.is_number()) {
        out.insert("top_p".into(), top_p.clone());
    }

    // stop_sequences: Responses has no Chat Completions `stop` equivalent — dropped
    // (same as codex). See docs/api.md grok Anthropic row.
    if let Some(of) = cleaned.as_object().and_then(anthropic_output_format) {
        let schema = of.get("schema").filter(|s| truthy(s));
        if of.get("type").and_then(Value::as_str) == Some("json_schema") {
            if let Some(schema) = schema {
                out.insert(
                    "text".into(),
                    json!({
                        "format": {
                            "type": "json_schema",
                            "name": "response",
                            "schema": schema,
                            "strict": false,
                        }
                    }),
                );
            }
        }
    }

    if let Some(tools) = cleaned.get("tools").and_then(Value::as_array) {
        let tools = map_anthropic_tools_to_responses(tools);
        if !tools.is_empty() {
            out.insert("tools".into(), Value::Array(tools));
            out.insert("tool_choice".into(), map_anthropic_tool_choice_to_responses(cleaned.get("tool_choice")));
        }
    }

    Ok(GrokAnthropicConvertResult { body: Value::Object(out), thinking_mode })
}

struct Converted {
    input: Vec<Value>,
    instructions: String,
}

fn anthropic_messages_to_grok_input(body: &Value, replay_encrypted_content: Option<&str>) -> Converted {
    let instructions = system_to_instructions(body.get("system"));
    let mut input: Vec<Value> = Vec::new();
    let replay = replay_encrypted_content.filter(|r| is_valid_grok_encrypted_content(r));

    let empty: Vec<Value> = Vec::new();
    let messages = body.get("messages").and_then(Value::as_array).unwrap_or(&empty);
    let last_assistant =
        messages.iter().rposition(|m| m.get("role").and_then(Value::as_str) == Some("assistant"));
    for (mi, m) in messages.iter().enumerate() {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("");

        if role == "user" {
            let blocks = m.get("content").and_then(Value::as_array);
            if let Some(blocks) = blocks {
                if blocks.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result")) {
                    for b in blocks {
                        if b.get("type").and_then(Value::as_str) == Some("tool_result") {
                            input.extend(tool_result_to_responses(b));
                        }
                    }
                    let non_tool: Vec<Value> = blocks
                        .iter()
                        .filter(|b| b.get("type").and_then(Value::as_str) != Some("tool_result"))
                        .cloned()
                        .collect();
                    if !non_tool.is_empty() {
                        input.push(json!({
                            "type": "message",
                            "role": "user",
                            "content": anthropic_blocks_to_input_content(&non_tool),
                        }));
                    }
                    continue;
                }
            }
            let content = match blocks {
                Some(blocks) => Value::Array(anthropic_blocks_to_input_content(blocks)),
                None => json!([{ "type": "input_text", "text": content_to_text(m.get("content")) }]),
            };
            input.push(json!({ "type": "message", "role": "user", "content": content }));
            continue;
        }

        if role == "assistant" {
            let blocks = m.get("content").and_then(Value::as_array);
            let mut text = String::new();
            let mut tool_uses: Vec<&Value> = Vec::new();
            let mut signature: Option<&str> = None;

            match blocks {
                Some(blocks) => {
                    for b in blocks {
                        match b.get("type").and_then(Value::as_str) {
                            Some("text") => text.push_str(&js_string(b.get("text"))),
                            Some("thinking") => {
                                // Only forward signatures that pass the Grok transport check —
                                // Claude-native / GPT / Gemini envelopes are dropped, never
                                // replayed as encrypted_content.
                                if let Some(sig) = b.get("signature").and_then(Value::as_str) {
                                    if is_valid_grok_encrypted_content(sig) {
                                        signature = Some(sig);
                                    }
                                }
                            }
                            Some("tool_use") => tool_uses.push(b),
                            _ => {}
                        }
                    }
                }
                None => text = content_to_text(m.get("content")),
            }

            // Prefer a validated client signature; else inject the session replay for the
            // trailing assistant turn when the client stripped signatures.
            let encrypted = signature.or(if last_assistant == Some(mi) { replay } else { None });
            if let Some(encrypted) = encrypted {
                input.push(json!({
                    "type": "reasoning",
                    "summary": [],
                    "content": null,
                    "encrypted_content": encrypted,
                }));
            }

            if !text.is_empty() {
                input.push(json!({
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": text }],
                }));
            }
            for tu in tool_uses {
                let mut call = Map::new();
                call.insert("type".into(), Value::String("function_call".into()));
                insert_if_defined(&mut call, "call_id", tu.get("id"));
                insert_if_defined(&mut call, "name", tu.get("name"));
                let args = tu.get("input").filter(|v| !v.is_null()).cloned().unwrap_or_else(|| json!({}));
                call.insert("arguments".into(), Value::String(args.to_string()));
                input.push(Value::Object(call));
            }
            continue;
        }

        // Unknown roles: best-effort user text.
        input.push(json!({
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": content_to_text(m.get("content")) }],
        }));
    }

    Converted { input, instructions }
}

fn system_to_instructions(system: Option<&Value>) -> String {
    match system {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .map(|b| {
                if b.get("type").and_then(Value::as_str) == Some("text") {
                    js_string(b.get("text"))
                } else {
                    String::new()
                }
            })
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

fn anthropic_blocks_to_input_content(blocks: &[Value]) -> Vec<Value> {
    let mut parts: Vec<Value> = Vec::new();
    for b in blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => parts.push(json!({ "type": "input_text", "text": js_string(b.get("text")) })),
            Some("image") => {
                let src = b.get("source");
                let src_type = src.and_then(|s| s.get("type")).and_then(Value::as_str);
                let data = src.and_then(|s| s.get("data")).and_then(Value::as_str).filter(|d| !d.is_empty());
                let url = src.and_then(|s| s.get("url")).and_then(Value::as_str).filter(|u| !u.is_empty());
                if src_type == Some("base64") {
                    if let Some(data) = data {
                        let media_type = src
                            .and_then(|s| s.get("media_type"))
                            .and_then(Value::as_str)
                            .filter(|m| !m.is_empty())
                            .unwrap_or("image/png");
                        parts.push(json!({
                            "type": "input_image",
                            "image_url": format!("data:{media_type};base64,{data}"),
                        }));
                    }
                } else if src_type == Some("url") {
                    if let Some(url) = url {
                        parts.push(json!({ "type": "input_image", "image_url": url }));
                    }
                }
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        parts.push(json!({ "type": "input_text", "text": "" }));
    }
    parts
}

fn tool_result_to_responses(b: &Value) -> Vec<Value> {
    let call_id = b.get("tool_use_id");
    let content = b.get("content");
    let blocks = content.and_then(Value::as_array);
    let images: Vec<Value> = blocks
        .map(|bs| bs.iter().filter(|x| x.get("type").and_then(Value::as_str) == Some("image")).cloned().collect())
        .unwrap_or_default();
    let mut out: Vec<Value> = Vec::new();

    if blocks.is_none() || images.is_empty() {
        let mut item = Map::new();
        item.insert("type".into(), Value::String("function_call_output".into()));
        insert_if_defined(&mut item, "call_id", call_id);
        item.insert("output".into(), Value::String(content_to_text(content)));
        out.push(Value::Object(item));
        return out;
    }

    let blocks = blocks.expect("checked above");
    let text: String = blocks
        .iter()
        .filter(|x| x.get("type").and_then(Value::as_str) == Some("text"))
        .map(|x| js_string(x.get("text")))
        .collect();
    let placeholder = if images.len() == 1 {
        "[image attached below]".to_string()
    } else {
        format!("[{} images attached below]", images.len())
    };
    let mut item = Map::new();
    item.insert("type".into(), Value::String("function_call_output".into()));
    insert_if_defined(&mut item, "call_id", call_id);
    item.insert(
        "output".into(),
        Value::String(if text.is_empty() { placeholder.clone() } else { format!("{text}\n{placeholder}") }),
    );
    out.push(Value::Object(item));

    let mut content_parts = vec![json!({
        "type": "input_text",
        "text": format!("[Image(s) from tool result {}]", js_template(call_id)),
    })];
    content_parts.extend(anthropic_blocks_to_input_content(&images));
    out.push(json!({ "type": "message", "role": "user", "content": content_parts }));
    out
}

fn map_anthropic_tools_to_responses(tools: &[Value]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for t in tools {
        let t = match t.as_object() {
            Some(t) => t,
            None => continue,
        };
        if let Some(fnc) = t.get("function").filter(|f| truthy(f)) {
            let mut item = Map::new();
            item.insert("type".into(), Value::String("function".into()));
            insert_if_defined(&mut item, "name", fnc.get("name"));
            insert_if_defined(&mut item, "description", fnc.get("description"));
            insert_if_defined(&mut item, "parameters", fnc.get("parameters"));
            out.push(Value::Object(item));
            continue;
        }
        // The bash and text editor tools come back with their parameters and convert below.
        let t = anthropic_client_tools::with_schema(t);
        let has_input_schema = t.contains_key("input_schema");
        let tool_type = t.get("type").and_then(Value::as_str);
        if let Some(tool_type) = tool_type {
            if tool_type != "custom" && !has_input_schema {
                // Any other Anthropic-defined tool — drop (same as Chat Completions convert).
                continue;
            }
        }
        if t.get("name").is_some_and(truthy) {
            let mut item = Map::new();
            item.insert("type".into(), Value::String("function".into()));
            insert_if_defined(&mut item, "name", t.get("name"));
            insert_if_defined(&mut item, "description", t.get("description"));
            let parameters = t
                .get("input_schema")
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
            item.insert("parameters".into(), parameters);
            out.push(Value::Object(item));
        }
    }
    out
}

fn map_anthropic_tool_choice_to_responses(tc: Option<&Value>) -> Value {
    let tc = match tc {
        Some(v) if v.is_object() => v,
        _ => return Value::String("auto".into()),
    };
    match tc.get("type").and_then(Value::as_str) {
        Some("auto") => Value::String("auto".into()),
        Some("none") => Value::String("none".into()),
        Some("any") => Value::String("required".into()),
        Some("tool") => match tc.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()) {
            Some(name) => json!({ "type": "function", "name": name }),
            None => Value::String("auto".into()),
        },
        _ => Value::String("auto".into()),
    }
}

fn content_to_text(content: Option<&Value>) -> String {
    match content {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|p| match p {
                Value::String(s) => s.clone(),
                p if p.get("type").and_then(Value::as_str) == Some("text") => js_string(p.get("text")),
                _ => String::new(),
            })
            .collect(),
        Some(Value::Object(_)) => "[object Object]".to_string(),
        Some(v) => v.to_string(),
    }
}

/// Trailing assistant plaintext — the replay cache's match key.
pub fn last_assistant_text_from_anthropic_messages(body: &Value) -> String {
    let empty: Vec<Value> = Vec::new();
    let messages = body.get("messages").and_then(Value::as_array).unwrap_or(&empty);
    for m in messages.iter().rev() {
        if m.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        if let Some(blocks) = m.get("content").and_then(Value::as_array) {
            let mut text = String::new();
            for b in blocks {
                if b.get("type").and_then(Value::as_str) == Some("text") {
                    text.push_str(&js_string(b.get("text")));
                }
            }
            return text;
        }
        return m.get("content").and_then(Value::as_str).unwrap_or("").to_string();
    }
    String::new()
}

// ── JSON coercions ─────────────────────────────────────────────────────────

/// `String(v ?? "")` for a possibly-missing JSON value.
fn js_string(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(_)) => "[object Object]".to_string(),
        Some(other) => other.to_string(),
    }
}

/// Template-literal `${v}`: a missing key is `undefined`, an explicit null is `null`.
fn js_template(v: Option<&Value>) -> String {
    match v {
        None => "undefined".to_string(),
        Some(Value::Null) => "null".to_string(),
        Some(other) => js_string(Some(other)),
    }
}

/// `JSON.stringify` drops `undefined` values — a missing source key stays missing.
fn insert_if_defined(map: &mut Map<String, Value>, key: &str, value: Option<&Value>) {
    if let Some(v) = value {
        map.insert(key.to_string(), v.clone());
    }
}

// ── Response / stream conversion ───────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrokTurnOutcome {
    Replayable { encrypted_content: String, assistant_text: String },
    Clear,
}

type TurnOutcomeSink = Box<dyn FnMut(GrokTurnOutcome) + Send>;

#[derive(Default)]
pub struct GrokStreamOptions {
    thinking_mode: GrokThinkingMode,
    on_turn_outcome: Option<TurnOutcomeSink>,
}

impl GrokStreamOptions {
    pub fn thinking_mode(mut self, mode: GrokThinkingMode) -> Self {
        self.thinking_mode = mode;
        self
    }
    pub fn on_turn_outcome(mut self, sink: impl FnMut(GrokTurnOutcome) + Send + 'static) -> Self {
        self.on_turn_outcome = Some(Box::new(sink));
        self
    }
}

/// Responses SSE → Anthropic Messages SSE.
///
/// Emits `signature_delta` from `reasoning.encrypted_content` when present. Upstream EOF
/// without `response.completed` is an Anthropic `event: error`, not a fabricated
/// successful `message_stop` (mirrors the codex mid-turn failure path).
pub fn grok_responses_sse_to_anthropic_stream<S>(
    body: S,
    model: &str,
    opts: GrokStreamOptions,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send
where
    S: Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static,
{
    let converter = Converter::new(model, opts);
    let lines = read_sse_lines_default(Box::pin(body));
    futures::stream::unfold((lines, converter, false), |(mut lines, mut converter, finished)| async move {
        if finished {
            return None;
        }
        loop {
            match lines.next().await {
                Some(Ok(line)) => {
                    converter.handle_line(&line);
                    let out = converter.take_output();
                    if !out.is_empty() {
                        return Some((Ok(out), (lines, converter, false)));
                    }
                    // An event held back behind open reasoning still re-arms the relay's idle
                    // timer, so a long tool call generated before reasoning closes is not read
                    // as silence (docs/api.md § Keepalive and idle timeout). Only after
                    // `message_start`: a heartbeat before any real frame would count as client
                    // output and cost a failed turn its pre-first-byte error frame and lease
                    // release.
                    if line.starts_with("data:") && converter.started && converter.reasoning_open && !converter.stopped {
                        return Some((Ok(Bytes::from_static(UPSTREAM_HEARTBEAT)), (lines, converter, false)));
                    }
                }
                Some(Err(e)) => return Some((Err(e), (lines, converter, true))),
                None => {
                    converter.end_of_stream();
                    let out = converter.take_output();
                    return if out.is_empty() { None } else { Some((Ok(out), (lines, converter, true))) };
                }
            }
        }
    })
}

/// Output held back while a reasoning item is open (see `Converter::reasoning_open`).
enum Deferred {
    AppendText(String),
    ToolAdded { item_id: String, call_id: String, name: String },
    ToolArgsDelta { item_id: String, delta: String },
    ToolDone { item_id: String, call_id: String, name: String, args: Option<String> },
}

struct LiveTool {
    item_id: String,
    block_index: i64,
    saw_args: bool,
}

struct Converter {
    model: String,
    emit_thinking: bool,
    msg_id: String,
    started: bool,
    stopped: bool,
    next_block_index: i64,
    text_block_open: bool,
    text_block_index: i64,
    thinking_block_open: bool,
    thinking_block_index: i64,
    pending_thinking: String,
    saw_tool_call: bool,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
    cache_read_input_tokens: Option<i64>,
    assistant_text: String,
    encrypted_content: String,
    thinking_signature_pending: String,
    // cli-chat-proxy often emits function_call / output_text before
    // reasoning.output_item.done. Closing the thinking block early would either drop the
    // final signature or emit a preliminary blob — both break the next turn (and Claude
    // Code fork/subagent inherits that assistant message).
    reasoning_open: bool,
    deferred: Vec<Deferred>,
    live_tool: Option<LiveTool>,
    pending_args: HashMap<String, String>,
    on_turn_outcome: Option<TurnOutcomeSink>,
    out: Vec<u8>,
}

impl Converter {
    fn new(model: &str, opts: GrokStreamOptions) -> Self {
        Self {
            model: model.to_string(),
            emit_thinking: opts.thinking_mode != GrokThinkingMode::Disabled,
            msg_id: new_message_id(),
            started: false,
            stopped: false,
            next_block_index: 0,
            text_block_open: false,
            text_block_index: -1,
            thinking_block_open: false,
            thinking_block_index: -1,
            pending_thinking: String::new(),
            saw_tool_call: false,
            prompt_tokens: None,
            completion_tokens: None,
            cache_read_input_tokens: None,
            assistant_text: String::new(),
            encrypted_content: String::new(),
            thinking_signature_pending: String::new(),
            reasoning_open: false,
            deferred: Vec::new(),
            live_tool: None,
            pending_args: HashMap::new(),
            on_turn_outcome: opts.on_turn_outcome,
            out: Vec::new(),
        }
    }

    fn take_output(&mut self) -> Bytes {
        Bytes::from(std::mem::take(&mut self.out))
    }

    fn emit_event(&mut self, event: &str, data: Value) {
        self.out.extend_from_slice(format!("event: {event}\ndata: {data}\n\n").as_bytes());
    }

    fn flush_deferred(&mut self) {
        let queued = std::mem::take(&mut self.deferred);
        for action in queued {
            self.run_deferred(action);
        }
    }

    /// Run now, or after the in-flight reasoning item finishes.
    fn after_reasoning(&mut self, action: Deferred) {
        if self.reasoning_open {
            self.deferred.push(action);
        } else {
            self.run_deferred(action);
        }
    }

    fn run_deferred(&mut self, action: Deferred) {
        match action {
            Deferred::AppendText(text) => self.append_text(&text),
            Deferred::ToolAdded { item_id, call_id, name } => {
                if self.live_tool.as_ref().is_none_or(|t| t.item_id != item_id) {
                    if self.live_tool.is_some() {
                        self.close_tool();
                    }
                    self.open_tool(&item_id, &call_id, &name);
                }
            }
            Deferred::ToolArgsDelta { item_id, delta } => {
                let matched = self.live_tool.as_ref().is_some_and(|t| t.item_id == item_id);
                if matched {
                    let index = {
                        let tool = self.live_tool.as_mut().expect("matched");
                        tool.saw_args = true;
                        tool.block_index
                    };
                    self.emit_event(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": { "type": "input_json_delta", "partial_json": delta },
                        }),
                    );
                } else {
                    self.pending_args.entry(item_id).or_default().push_str(&delta);
                }
            }
            Deferred::ToolDone { item_id, call_id, name, args } => {
                if self.live_tool.as_ref().is_none_or(|t| t.item_id != item_id) {
                    if self.live_tool.is_some() {
                        self.close_tool();
                    }
                    let open_id =
                        if item_id.is_empty() { format!("item_{}", self.next_block_index) } else { item_id.clone() };
                    self.open_tool(&open_id, &call_id, &name);
                }
                let pending = match (&self.live_tool, &args) {
                    (Some(tool), Some(args)) if !tool.saw_args && !args.is_empty() => Some((tool.block_index, args.clone())),
                    _ => None,
                };
                if let Some((index, args)) = pending {
                    if let Some(tool) = self.live_tool.as_mut() {
                        tool.saw_args = true;
                    }
                    self.emit_event(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": { "type": "input_json_delta", "partial_json": args },
                        }),
                    );
                }
                self.close_tool();
            }
        }
    }

    /// Responses usage, from whichever event carries it. Called for *every* event, not
    /// just the terminal one: `message_start.usage.input_tokens` is the client's context
    /// indicator (docs/api.md).
    fn harvest_usage(&mut self, usage: Option<&Value>) {
        let usage = match usage {
            Some(u) if u.is_object() => u,
            _ => return,
        };
        if let Some(v) = as_number(usage.get("input_tokens")) {
            self.prompt_tokens = Some(v);
        }
        // Responses output_tokens already includes reasoning for xAI.
        if let Some(v) = as_number(usage.get("output_tokens")) {
            self.completion_tokens = Some(v);
        }
        if let Some(v) = as_number(usage.get("input_tokens_details").and_then(|d| d.get("cached_tokens"))) {
            self.cache_read_input_tokens = Some(v);
        }
    }

    fn ensure_start(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        let input = match self.prompt_tokens {
            Some(p) => p - self.cache_read_input_tokens.unwrap_or(0),
            None => 0,
        };
        let mut usage = Map::new();
        usage.insert("input_tokens".into(), json!(input.max(0)));
        usage.insert("output_tokens".into(), json!(0));
        if let Some(cache) = self.cache_read_input_tokens {
            usage.insert("cache_read_input_tokens".into(), json!(cache));
        }
        let model = self.model.clone();
        let msg_id = self.msg_id.clone();
        self.emit_event(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": msg_id,
                    "type": "message",
                    "role": "assistant",
                    "model": model,
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": Value::Object(usage),
                },
            }),
        );
    }

    fn close_text(&mut self) {
        if !self.text_block_open {
            return;
        }
        let index = self.text_block_index;
        self.emit_event("content_block_stop", json!({ "type": "content_block_stop", "index": index }));
        self.text_block_open = false;
    }

    fn close_tool(&mut self) {
        let index = match &self.live_tool {
            Some(tool) => tool.block_index,
            None => return,
        };
        self.emit_event("content_block_stop", json!({ "type": "content_block_stop", "index": index }));
        self.live_tool = None;
    }

    fn close_thinking(&mut self) {
        if !self.thinking_block_open {
            return;
        }
        let index = self.thinking_block_index;
        if !self.thinking_signature_pending.is_empty() {
            let signature = std::mem::take(&mut self.thinking_signature_pending);
            self.emit_event(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": { "type": "signature_delta", "signature": signature },
                }),
            );
        }
        self.emit_event("content_block_stop", json!({ "type": "content_block_stop", "index": index }));
        self.thinking_block_open = false;
    }

    fn open_thinking(&mut self) {
        if !self.emit_thinking || self.thinking_block_open {
            return;
        }
        if self.text_block_open || self.live_tool.is_some() {
            return;
        }
        self.ensure_start();
        self.thinking_block_index = self.next_block_index;
        self.next_block_index += 1;
        self.thinking_block_open = true;
        let index = self.thinking_block_index;
        self.emit_event(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": index,
                "content_block": { "type": "thinking", "thinking": "" },
            }),
        );
    }

    fn append_thinking(&mut self, text: &str) {
        if !self.emit_thinking || text.is_empty() {
            return;
        }
        if self.text_block_open || self.live_tool.is_some() {
            self.pending_thinking.push_str(text);
            return;
        }
        self.open_thinking();
        let index = self.thinking_block_index;
        self.emit_event(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "thinking_delta", "thinking": text },
            }),
        );
    }

    fn flush_pending_thinking(&mut self) {
        if self.pending_thinking.is_empty() {
            return;
        }
        let thinking = std::mem::take(&mut self.pending_thinking);
        let index = self.next_block_index;
        self.next_block_index += 1;
        let mut content_block = Map::new();
        content_block.insert("type".into(), Value::String("thinking".into()));
        content_block.insert("thinking".into(), Value::String(String::new()));
        if !self.encrypted_content.is_empty() {
            content_block.insert("signature".into(), Value::String(self.encrypted_content.clone()));
        }
        self.emit_event(
            "content_block_start",
            json!({ "type": "content_block_start", "index": index, "content_block": Value::Object(content_block) }),
        );
        self.emit_event(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "thinking_delta", "thinking": thinking },
            }),
        );
        self.emit_event("content_block_stop", json!({ "type": "content_block_stop", "index": index }));
    }

    fn append_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.ensure_start();
        self.close_thinking();
        if self.live_tool.is_some() {
            return;
        }
        self.assistant_text.push_str(text);
        if !self.text_block_open {
            self.text_block_index = self.next_block_index;
            self.next_block_index += 1;
            self.text_block_open = true;
            let index = self.text_block_index;
            self.emit_event(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": { "type": "text", "text": "" },
                }),
            );
        }
        let index = self.text_block_index;
        self.emit_event(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "text_delta", "text": text },
            }),
        );
    }

    fn open_tool(&mut self, item_id: &str, call_id: &str, name: &str) {
        self.ensure_start();
        self.close_text();
        self.close_thinking();
        let block_index = self.next_block_index;
        self.next_block_index += 1;
        self.live_tool = Some(LiveTool { item_id: item_id.to_string(), block_index, saw_args: false });
        self.saw_tool_call = true;
        let tool_name = if name.is_empty() { "unknown" } else { name };
        self.emit_event(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": block_index,
                "content_block": { "type": "tool_use", "id": call_id, "name": tool_name, "input": {} },
            }),
        );
        if let Some(stashed) = self.pending_args.remove(item_id).filter(|s| !s.is_empty()) {
            if let Some(tool) = self.live_tool.as_mut() {
                tool.saw_args = true;
            }
            self.emit_event(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": block_index,
                    "delta": { "type": "input_json_delta", "partial_json": stashed },
                }),
            );
        }
    }

    fn emit_upstream_error(&mut self, message: &str) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.emit_event("error", json!({ "type": "error", "error": { "type": "api_error", "message": message } }));
    }

    fn finish(&mut self, reason: &str) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.ensure_start();
        // Settle any in-flight reasoning first so deferred tool/text blocks still follow a
        // signature_delta when upstream omitted `.done`.
        self.close_thinking();
        self.reasoning_open = false;
        self.flush_deferred();
        self.close_text();
        self.close_tool();
        self.flush_pending_thinking();
        if self.next_block_index == 0 {
            let index = self.next_block_index;
            self.emit_event(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": { "type": "text", "text": "" },
                }),
            );
            self.emit_event("content_block_stop", json!({ "type": "content_block_stop", "index": index }));
            self.next_block_index += 1;
        }
        let final_reason = if reason == "end_turn" && self.saw_tool_call { "tool_use" } else { reason };
        let input_tokens = match (self.prompt_tokens, self.cache_read_input_tokens) {
            (Some(p), Some(c)) => Some(p - c),
            _ => self.prompt_tokens,
        };
        let mut usage = Map::new();
        if let Some(input_tokens) = input_tokens {
            usage.insert("input_tokens".into(), json!(input_tokens));
        }
        usage.insert("output_tokens".into(), json!(self.completion_tokens.unwrap_or(0)));
        if let Some(cache) = self.cache_read_input_tokens {
            usage.insert("cache_read_input_tokens".into(), json!(cache));
        }
        self.emit_event(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": final_reason, "stop_sequence": null },
                "usage": Value::Object(usage),
            }),
        );
        self.emit_event("message_stop", json!({ "type": "message_stop" }));

        if self.on_turn_outcome.is_none() {
            return;
        }
        // `encrypted_content` only ever holds a value that passed `valid_encrypted`.
        let outcome = if self.emit_thinking && !self.encrypted_content.is_empty() {
            GrokTurnOutcome::Replayable {
                encrypted_content: self.encrypted_content.clone(),
                assistant_text: self.assistant_text.clone(),
            }
        } else {
            // Completed turn with no replayable state (disabled / no ciphertext) must not
            // leave a prior turn's entry for a later inject.
            GrokTurnOutcome::Clear
        };
        if let Some(sink) = self.on_turn_outcome.as_mut() {
            sink(outcome);
        }
    }

    fn handle_line(&mut self, line: &str) {
        if !line.starts_with("data:") {
            return;
        }
        let data = line[5..].trim();
        if data.is_empty() || data == "[DONE]" {
            return;
        }
        if self.stopped {
            return;
        }
        let Ok(ev) = serde_json::from_str::<Value>(data) else {
            return;
        };
        self.handle_event(&ev);
    }

    fn handle_event(&mut self, ev: &Value) {
        // Every event, not just the terminal one — see harvest_usage.
        self.harvest_usage(ev.get("response").and_then(|r| r.get("usage")));
        let ev_type = ev.get("type").and_then(Value::as_str).unwrap_or("");
        if ev_type == "response.failed" || ev_type == "error" {
            let message = ev
                .get("response")
                .and_then(|r| r.get("error"))
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .filter(|m| !m.is_empty())
                .or_else(|| ev.get("error").and_then(|e| e.get("message")).and_then(Value::as_str).filter(|m| !m.is_empty()))
                .or_else(|| ev.get("message").and_then(Value::as_str).filter(|m| !m.is_empty()))
                .unwrap_or("upstream error")
                .to_string();
            self.emit_upstream_error(&message);
            return;
        }

        let item = ev.get("item");
        let item_type = item.and_then(|i| i.get("type")).and_then(Value::as_str).unwrap_or("");
        let delta = ev.get("delta").and_then(Value::as_str).filter(|d| !d.is_empty());

        if ev_type == "response.reasoning_summary_text.delta" {
            if let Some(delta) = delta {
                self.append_thinking(delta);
            }
        } else if ev_type == "response.output_text.delta" {
            if let Some(delta) = delta {
                self.after_reasoning(Deferred::AppendText(delta.to_string()));
            }
        } else if ev_type == "response.output_item.added" && item_type == "reasoning" {
            self.reasoning_open = true;
            // Pre-content encrypted snapshot — keep for fallback only; the final value
            // arrives on output_item.done. Do not close the thinking block until then.
            if let Some(enc) = valid_encrypted(item) {
                self.thinking_signature_pending = enc.clone();
                if self.encrypted_content.is_empty() {
                    self.encrypted_content = enc;
                }
            }
            self.open_thinking();
        } else if ev_type == "response.output_item.done" && item_type == "reasoning" {
            if let Some(enc) = valid_encrypted(item) {
                self.encrypted_content = enc.clone();
                self.thinking_signature_pending = enc;
            }
            // Summary text may only appear on the done item.
            let summary_text = summary_text_from_item(item);
            if !summary_text.is_empty() {
                self.append_thinking(&summary_text);
            }
            // Signature-only reasoning (no summary text streamed): still open a thinking
            // block so signature_delta can be delivered.
            if !self.thinking_signature_pending.is_empty() {
                self.open_thinking();
            }
            self.close_thinking();
            self.reasoning_open = false;
            self.flush_deferred();
        } else if ev_type == "response.output_item.added" && item_type == "function_call" {
            let item = item.expect("item_type implies item");
            let raw_id = str_field(item, "id");
            let raw_call_id = str_field(item, "call_id");
            let item_id = if !raw_id.is_empty() {
                raw_id
            } else if !raw_call_id.is_empty() {
                raw_call_id.clone()
            } else {
                format!("item_{}", self.next_block_index)
            };
            let call_id = if raw_call_id.is_empty() { item_id.clone() } else { raw_call_id };
            let name = non_empty_or(str_field(item, "name"), "unknown");
            self.after_reasoning(Deferred::ToolAdded { item_id, call_id, name });
        } else if ev_type == "response.function_call_arguments.delta" {
            let item_id = ev.get("item_id").and_then(Value::as_str).unwrap_or("").to_string();
            let delta = ev.get("delta").and_then(Value::as_str).unwrap_or("").to_string();
            if item_id.is_empty() || delta.is_empty() {
                return;
            }
            self.after_reasoning(Deferred::ToolArgsDelta { item_id, delta });
        } else if ev_type == "response.output_item.done" && item_type == "function_call" {
            let item = item.expect("item_type implies item");
            let raw_id = str_field(item, "id");
            let raw_call_id = str_field(item, "call_id");
            let item_id = if !raw_id.is_empty() { raw_id } else { raw_call_id.clone() };
            let call_id = if !raw_call_id.is_empty() {
                raw_call_id
            } else if !item_id.is_empty() {
                item_id.clone()
            } else {
                format!("call_{}", self.next_block_index)
            };
            let name = non_empty_or(str_field(item, "name"), "unknown");
            let args = item.get("arguments").and_then(Value::as_str).map(str::to_string);
            self.after_reasoning(Deferred::ToolDone { item_id, call_id, name, args });
        } else if ev_type == "response.completed" || ev_type == "response.done" {
            // Also harvest encrypted_content from the completed output if stream events
            // omitted it.
            if self.encrypted_content.is_empty() {
                if let Some(output) = ev.get("response").and_then(|r| r.get("output")).and_then(Value::as_array) {
                    for item in output {
                        if item.get("type").and_then(Value::as_str) == Some("reasoning") {
                            if let Some(enc) = valid_encrypted(Some(item)) {
                                self.encrypted_content = enc;
                            }
                        }
                        if item.get("type").and_then(Value::as_str) == Some("message") {
                            if let Some(parts) = item.get("content").and_then(Value::as_array) {
                                for part in parts {
                                    if part.get("type").and_then(Value::as_str) == Some("output_text") {
                                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                                            if self.assistant_text.is_empty() {
                                                self.assistant_text.push_str(text);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            self.finish(if self.saw_tool_call { "tool_use" } else { "end_turn" });
        }
    }

    /// `response.completed` and every upstream error stop the converter, so an EOF that
    /// finds it still running is a truncated upstream: never fabricate a successful
    /// message_stop.
    fn end_of_stream(&mut self) {
        self.emit_upstream_error("upstream stalled: stream ended before response.completed");
    }
}

fn valid_encrypted(item: Option<&Value>) -> Option<String> {
    let enc = item?.get("encrypted_content")?.as_str()?;
    if is_valid_grok_encrypted_content(enc) {
        Some(enc.to_string())
    } else {
        None
    }
}

fn str_field(item: &Value, key: &str) -> String {
    item.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn non_empty_or(value: String, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_string()
    } else {
        value
    }
}

fn as_number(v: Option<&Value>) -> Option<i64> {
    let v = v?;
    if !v.is_number() {
        return None;
    }
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))
}

fn summary_text_from_item(item: Option<&Value>) -> String {
    let summary = match item.and_then(|i| i.get("summary")).and_then(Value::as_array) {
        Some(s) => s,
        None => return String::new(),
    };
    let mut parts: Vec<String> = Vec::new();
    for part in summary {
        match part {
            Value::String(s) => parts.push(s.clone()),
            part => {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    parts.push(text.to_string());
                }
            }
        }
    }
    parts.join("\n\n")
}

fn new_message_id() -> String {
    let mut bytes = [0u8; 12];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
    format!("msg_{}", hex::encode(bytes))
}

// ── Non-streaming collection ───────────────────────────────────────────────

struct BlockAcc {
    block_type: String,
    thinking: Option<String>,
    text: Option<String>,
    signature: Option<String>,
    id: Option<Value>,
    name: Option<Value>,
    partial: Option<String>,
}

/// Collect Responses SSE into one Anthropic message object (non-stream clients).
/// Returns either the message or `{"error":{"message","type"}}`.
pub async fn collect_grok_responses_sse_to_anthropic<S>(body: S, model: &str, opts: GrokStreamOptions) -> Value
where
    S: Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static,
{
    let stream = grok_responses_sse_to_anthropic_stream(body, model, opts);
    let mut lines = read_sse_lines_default(Box::pin(stream));

    let mut event = String::new();
    let mut error: Option<Value> = None;
    let mut completed = false;
    let mut content: Vec<Value> = Vec::new();
    let mut stop_reason = "end_turn".to_string();
    let mut usage = json!({ "input_tokens": 0, "output_tokens": 0 });
    let mut msg_id = format!("msg_{}", crate::app::now_ms());
    let mut open: HashMap<i64, BlockAcc> = HashMap::new();

    while let Some(line) = lines.next().await {
        let line = match line {
            Ok(line) => line,
            // A turn the transport cut short never completed: an error, never the partial
            // content dressed as an `end_turn` message (docs/api.md "Grok reasoning"). A
            // transport error after `message_stop` leaves the completed turn as it is.
            Err(_) => {
                if !completed {
                    error.get_or_insert_with(|| json!({ "message": "upstream stream error", "type": "api_error" }));
                }
                break;
            }
        };
        if let Some(rest) = line.strip_prefix("event:") {
            event = rest.trim().to_string();
            continue;
        }
        let rest = match line.strip_prefix("data:") {
            Some(r) => r.trim(),
            None => continue,
        };
        if rest.is_empty() {
            continue;
        }
        if let Ok(json) = serde_json::from_str::<Value>(rest) {
            if event == "error" || json.get("type").and_then(Value::as_str) == Some("error") {
                let err = json.get("error");
                error = Some(json!({
                    "message": err
                        .and_then(|e| e.get("message"))
                        .and_then(Value::as_str)
                        .filter(|m| !m.is_empty())
                        .unwrap_or("upstream error"),
                    "type": err
                        .and_then(|e| e.get("type"))
                        .and_then(Value::as_str)
                        .filter(|t| !t.is_empty())
                        .unwrap_or("api_error"),
                }));
            } else if event == "message_start" {
                if let Some(id) = json.get("message").and_then(|m| m.get("id")).and_then(Value::as_str).filter(|i| !i.is_empty())
                {
                    msg_id = id.to_string();
                }
            } else if event == "content_block_start" {
                let index = as_number(json.get("index")).unwrap_or(0);
                let block = json.get("content_block");
                let block_type = block.and_then(|b| b.get("type")).and_then(Value::as_str).unwrap_or("text").to_string();
                open.insert(
                    index,
                    BlockAcc {
                        thinking: (block_type == "thinking").then(String::new),
                        text: (block_type == "text").then(String::new),
                        signature: block.and_then(|b| b.get("signature")).and_then(Value::as_str).map(str::to_string),
                        id: block.and_then(|b| b.get("id")).filter(|v| v.is_string()).cloned(),
                        name: block.and_then(|b| b.get("name")).filter(|v| v.is_string()).cloned(),
                        partial: (block_type == "tool_use").then(String::new),
                        block_type,
                    },
                );
            } else if event == "content_block_delta" {
                let index = as_number(json.get("index")).unwrap_or(0);
                let delta = json.get("delta");
                if let Some(block) = open.get_mut(&index) {
                    match delta.and_then(|d| d.get("type")).and_then(Value::as_str) {
                        Some("thinking_delta") => {
                            block.thinking.get_or_insert_with(String::new).push_str(&js_string(delta.and_then(|d| d.get("thinking"))));
                        }
                        Some("signature_delta") => {
                            block.signature = Some(js_string(delta.and_then(|d| d.get("signature"))));
                        }
                        Some("text_delta") => {
                            block.text.get_or_insert_with(String::new).push_str(&js_string(delta.and_then(|d| d.get("text"))));
                        }
                        Some("input_json_delta") => {
                            block
                                .partial
                                .get_or_insert_with(String::new)
                                .push_str(&js_string(delta.and_then(|d| d.get("partial_json"))));
                        }
                        _ => {}
                    }
                }
            } else if event == "content_block_stop" {
                let index = as_number(json.get("index")).unwrap_or(0);
                if let Some(block) = open.remove(&index) {
                    match block.block_type.as_str() {
                        "thinking" => {
                            let mut thinking_block = Map::new();
                            thinking_block.insert("type".into(), Value::String("thinking".into()));
                            thinking_block.insert("thinking".into(), Value::String(block.thinking.unwrap_or_default()));
                            if let Some(signature) = block.signature.filter(|s| !s.is_empty()) {
                                thinking_block.insert("signature".into(), Value::String(signature));
                            }
                            content.push(Value::Object(thinking_block));
                        }
                        "text" => content.push(json!({ "type": "text", "text": block.text.unwrap_or_default() })),
                        "tool_use" => {
                            let partial = block.partial.unwrap_or_default();
                            let source = if partial.is_empty() { "{}".to_string() } else { partial.clone() };
                            let input = serde_json::from_str::<Value>(&source).unwrap_or_else(|_| json!({ "raw": partial }));
                            let mut item = Map::new();
                            item.insert("type".into(), Value::String("tool_use".into()));
                            insert_if_defined(&mut item, "id", block.id.as_ref());
                            insert_if_defined(&mut item, "name", block.name.as_ref());
                            item.insert("input".into(), input);
                            content.push(Value::Object(item));
                        }
                        _ => {}
                    }
                }
            } else if event == "message_delta" {
                if let Some(reason) =
                    json.get("delta").and_then(|d| d.get("stop_reason")).and_then(Value::as_str).filter(|r| !r.is_empty())
                {
                    stop_reason = reason.to_string();
                }
                if let Some(u) = json.get("usage").filter(|u| u.is_object()) {
                    usage = u.clone();
                }
            } else if event == "message_stop" {
                completed = true;
            }
        }
        event.clear();
    }

    if let Some(error) = error {
        return json!({ "error": error });
    }
    json!({
        "id": msg_id,
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": if content.is_empty() { json!([{ "type": "text", "text": "" }]) } else { Value::Array(content) },
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::grok_encrypted_content::test_fixtures::fake_grok_encrypted_content;
    use std::sync::{Arc, Mutex};

    fn sse(events: &[Value]) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static {
        let text: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
        futures::stream::once(async move { Ok(Bytes::from(text)) })
    }

    async fn read_sse(stream: impl Stream<Item = Result<Bytes, std::io::Error>> + Send) -> String {
        futures::pin_mut!(stream);
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.expect("stream chunk"));
        }
        String::from_utf8(out).expect("utf8")
    }

    // ── resolve_grok_thinking_effort ───────────────────────────────────────

    #[test]
    fn resolves_thinking_mode_and_effort() {
        let resolution = |thinking_mode, effort| GrokThinkingResolution { thinking_mode, effort };
        for (body, expected) in [
            // Disabled is authoritative: no effort field is honoured alongside it.
            (
                json!({ "thinking": { "type": "disabled" }, "output_config": { "effort": "high" }, "reasoning_effort": "xhigh" }),
                resolution(GrokThinkingMode::Disabled, None),
            ),
            (
                json!({ "thinking": { "type": "adaptive" }, "output_config": { "effort": "high" } }),
                resolution(GrokThinkingMode::Enabled, Some(ReasoningEffort::High)),
            ),
            (json!({ "thinking": { "type": "adaptive" } }), resolution(GrokThinkingMode::Enabled, Some(ReasoningEffort::Medium))),
            // Effort-only: budget_tokens is not mapped.
            (
                json!({ "thinking": { "type": "enabled", "budget_tokens": 4096 } }),
                resolution(GrokThinkingMode::Enabled, Some(ReasoningEffort::Medium)),
            ),
            (json!({ "output_config": { "effort": "low" } }), resolution(GrokThinkingMode::Enabled, Some(ReasoningEffort::Low))),
            (json!({}), resolution(GrokThinkingMode::Default, None)),
        ] {
            assert_eq!(resolve_grok_thinking_effort(&body), expected, "{body}");
        }
    }

    // ── anthropic_to_grok_responses ────────────────────────────────────────

    fn convert(body: Value, replay: Option<&str>) -> GrokAnthropicConvertResult {
        anthropic_to_grok_responses(&body, "grok-4.5", replay).expect("converts")
    }

    #[test]
    fn sends_include_encrypted_content_and_reasoning_effort_for_adaptive() {
        let r = convert(
            json!({
                "model": "grok-4.5",
                "max_tokens": 100,
                "thinking": { "type": "adaptive" },
                "output_config": { "effort": "xhigh" },
                "messages": [{ "role": "user", "content": "hi" }]
            }),
            None,
        );
        assert_eq!(r.thinking_mode, GrokThinkingMode::Enabled);
        assert_eq!(r.body["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(r.body["reasoning"], json!({ "effort": "xhigh" }));
        assert_eq!(r.body["store"], json!(false));
        assert_eq!(r.body["stream"], json!(true));
        assert_eq!(r.body["temperature"], json!(1));
    }

    #[test]
    fn disabled_plus_explicit_effort_sends_no_include_or_reasoning() {
        let r = convert(
            json!({
                "thinking": { "type": "disabled" },
                "output_config": { "effort": "high" },
                "reasoning_effort": "xhigh",
                "messages": [{ "role": "user", "content": "hi" }]
            }),
            None,
        );
        assert_eq!(r.thinking_mode, GrokThinkingMode::Disabled);
        assert!(r.body.get("include").is_none());
        assert!(r.body.get("reasoning").is_none());
    }

    #[test]
    fn maps_validated_thinking_signature_to_a_reasoning_input_item() {
        let sig = fake_grok_encrypted_content(11);
        let r = convert(
            json!({
                "messages": [
                    { "role": "user", "content": "hi" },
                    {
                        "role": "assistant",
                        "content": [
                            { "type": "thinking", "thinking": "secret plan", "signature": sig },
                            { "type": "text", "text": "hello" }
                        ]
                    },
                    { "role": "user", "content": "again" }
                ]
            }),
            None,
        );
        let input = r.body["input"].as_array().expect("input");
        let reasoning = input.iter().find(|i| i["type"] == "reasoning").expect("reasoning item");
        assert_eq!(reasoning["type"], "reasoning");
        assert_eq!(reasoning["encrypted_content"], json!(sig));
    }

    #[test]
    fn replays_no_reasoning_item_for_a_foreign_or_missing_signature() {
        let provider_prefixed = format!("claude#{}", fake_grok_encrypted_content(12));
        for thinking in [
            json!({ "type": "thinking", "thinking": "gpt", "signature": "gAAAAABopenai-encrypted-content-blob" }),
            json!({ "type": "thinking", "thinking": "x", "signature": provider_prefixed }),
            // Unsigned plaintext never becomes an invented signature.
            json!({ "type": "thinking", "thinking": "unsigned only" }),
        ] {
            let r = convert(
                json!({
                    "messages": [
                        { "role": "assistant", "content": [thinking.clone(), { "type": "text", "text": "hi" }] },
                        { "role": "user", "content": "next" }
                    ]
                }),
                None,
            );
            let input = r.body["input"].as_array().expect("input");
            assert!(!input.iter().any(|i| i["type"] == "reasoning"), "{thinking}");
        }
    }

    #[test]
    fn injects_replay_encrypted_content_when_client_omitted_signature() {
        let replay = fake_grok_encrypted_content(13);
        let r = convert(
            json!({
                "messages": [
                    { "role": "user", "content": "hi" },
                    { "role": "assistant", "content": [{ "type": "text", "text": "first" }] },
                    { "role": "user", "content": "again" },
                    { "role": "assistant", "content": [{ "type": "text", "text": "second" }] },
                    { "role": "user", "content": "more" }
                ]
            }),
            Some(&replay),
        );
        // The session ciphertext belongs to the trailing assistant turn only, never an earlier one.
        let kinds: Vec<(&str, &str)> = r.body["input"]
            .as_array()
            .expect("input")
            .iter()
            .map(|i| {
                let label = i["content"][0]["text"].as_str().or(i["encrypted_content"].as_str()).unwrap_or("");
                (i["type"].as_str().unwrap_or(""), label)
            })
            .collect();
        assert_eq!(
            kinds,
            [
                ("message", "hi"),
                ("message", "first"),
                ("message", "again"),
                ("reasoning", replay.as_str()),
                ("message", "second"),
                ("message", "more"),
            ]
        );
    }

    #[test]
    fn puts_system_text_into_instructions() {
        let r = convert(json!({ "system": "be brief", "messages": [{ "role": "user", "content": "hi" }] }), None);
        assert_eq!(r.body["instructions"], "be brief");
    }

    #[test]
    fn drops_stop_sequences_and_maps_either_structured_output_spelling() {
        let schema = json!({ "type": "object", "properties": { "a": { "type": "string" } } });
        for spelling in [
            json!({ "output_config": { "format": { "type": "json_schema", "schema": schema } } }),
            json!({ "output_format": { "type": "json_schema", "schema": schema } }),
        ] {
            let mut body = json!({ "messages": [{ "role": "user", "content": "hi" }], "stop_sequences": ["END"] });
            body.as_object_mut().unwrap().extend(spelling.as_object().unwrap().clone());
            let r = convert(body, None);
            assert!(r.body.get("stop").is_none());
            assert!(r.body.get("stop_sequences").is_none());
            assert_eq!(
                r.body["text"],
                json!({ "format": { "type": "json_schema", "name": "response", "schema": schema, "strict": false } }),
                "{spelling}"
            );
        }
    }

    #[test]
    fn converts_tools_and_drops_server_side_anthropic_tools() {
        let r = convert(
            json!({
                "messages": [{ "role": "user", "content": "hi" }],
                "tools": [
                    { "name": "lookup", "description": "d", "input_schema": { "type": "object", "properties": {} } },
                    { "type": "web_search_20250305", "name": "web_search" }
                ]
            }),
            None,
        );
        assert_eq!(
            r.body["tools"],
            json!([{ "type": "function", "name": "lookup", "description": "d", "parameters": { "type": "object", "properties": {} } }])
        );
    }

    #[test]
    fn converts_the_bash_tool_with_its_parameters() {
        let r = convert(
            json!({
                "messages": [{ "role": "user", "content": "hi" }],
                "tools": [{ "type": "bash_20250124", "name": "bash" }]
            }),
            None,
        );
        let tool = &r.body["tools"][0];
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["name"], "bash");
        assert_eq!(tool["parameters"]["properties"]["command"]["type"], "string");
    }

    #[test]
    fn invalid_reasoning_effort_is_a_sentinel_error() {
        let err = anthropic_to_grok_responses(
            &json!({ "reasoning_effort": "turbo", "messages": [{ "role": "user", "content": "hi" }] }),
            "grok-4.5",
            None,
        );
        assert_eq!(err, Err(InvalidGrokReasoningEffortError));
        let err = anthropic_to_grok_responses(
            &json!({ "output_config": { "effort": "turbo" }, "messages": [] }),
            "grok-4.5",
            None,
        );
        assert_eq!(err, Err(InvalidGrokReasoningEffortError));
    }

    // ── grok_responses_sse_to_anthropic_stream ─────────────────────────────

    #[tokio::test]
    async fn emits_signature_delta_from_reasoning_encrypted_content() {
        let enc_pre = fake_grok_encrypted_content(21);
        let enc_final = fake_grok_encrypted_content(22);
        let stream = grok_responses_sse_to_anthropic_stream(
            sse(&[
                json!({ "type": "response.output_item.added", "item": { "type": "reasoning", "encrypted_content": enc_pre } }),
                json!({ "type": "response.reasoning_summary_text.delta", "delta": "thinking " }),
                json!({ "type": "response.reasoning_summary_text.delta", "delta": "hard" }),
                json!({
                    "type": "response.output_item.done",
                    "item": { "type": "reasoning", "encrypted_content": enc_final, "summary": [{ "type": "summary_text", "text": "" }] }
                }),
                json!({ "type": "response.output_text.delta", "delta": "answer" }),
                json!({ "type": "response.completed", "response": { "usage": { "input_tokens": 10, "output_tokens": 5 } } }),
            ]),
            "grok/grok-4.5",
            GrokStreamOptions::default(),
        );
        let out = read_sse(stream).await;
        assert!(out.contains(r#""type":"thinking""#));
        assert!(out.contains(r#""type":"thinking_delta""#));
        assert!(out.contains(r#""thinking":"thinking ""#));
        assert!(out.contains(r#""type":"signature_delta""#));
        assert!(out.contains(&format!(r#""signature":"{enc_final}""#)));
        assert!(!out.contains(&enc_pre));
        assert!(out.contains(r#""type":"text_delta""#));
        assert!(out.contains(r#""text":"answer""#));
    }

    #[tokio::test]
    async fn defers_function_call_until_reasoning_done_so_the_final_signature_is_kept() {
        let enc_pre = fake_grok_encrypted_content(31);
        let enc_final = fake_grok_encrypted_content(32);
        let stream = grok_responses_sse_to_anthropic_stream(
            sse(&[
                json!({ "type": "response.output_item.added", "item": { "type": "reasoning", "encrypted_content": enc_pre } }),
                json!({ "type": "response.reasoning_summary_text.delta", "delta": "spawn worker" }),
                json!({
                    "type": "response.output_item.added",
                    "item": { "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "Agent" }
                }),
                json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_1", "delta": r#"{"prompt":"explore"}"# }),
                json!({
                    "type": "response.output_item.done",
                    "item": { "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "Agent", "arguments": r#"{"prompt":"explore"}"# }
                }),
                json!({
                    "type": "response.output_item.done",
                    "item": { "type": "reasoning", "encrypted_content": enc_final, "summary": [{ "type": "summary_text", "text": "" }] }
                }),
                json!({ "type": "response.completed", "response": { "usage": { "input_tokens": 10, "output_tokens": 20 } } }),
            ]),
            "grok/grok-4.5",
            GrokStreamOptions::default(),
        );
        let out = read_sse(stream).await;
        let sig_idx = out.find(&format!(r#""signature":"{enc_final}""#));
        let tool_idx = out.find(r#""type":"tool_use""#);
        assert!(sig_idx.is_some());
        assert!(tool_idx.is_some());
        assert!(sig_idx < tool_idx);
        assert!(!out.contains(&enc_pre));
        assert!(out.contains(r#""name":"Agent""#));
        assert!(out.contains(r#""stop_reason":"tool_use""#));
    }

    #[tokio::test]
    async fn a_frame_held_back_behind_open_reasoning_still_reaches_the_relay_as_a_heartbeat() {
        let stream = grok_responses_sse_to_anthropic_stream(
            sse(&[
                json!({ "type": "response.output_item.added", "item": { "type": "reasoning" } }),
                json!({
                    "type": "response.output_item.added",
                    "item": { "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "Write" }
                }),
                json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_1", "delta": "{\"a\":" }),
                json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_1", "delta": "1}" }),
            ]),
            "grok/grok-4.5",
            GrokStreamOptions::default(),
        );
        futures::pin_mut!(stream);
        let mut heartbeats = 0;
        while let Some(chunk) = stream.next().await {
            if chunk.expect("chunk").as_ref() == UPSTREAM_HEARTBEAT {
                heartbeats += 1;
            }
        }
        // The tool call and both argument deltas wait for reasoning to close; each still
        // re-arms the idle timer.
        assert_eq!(heartbeats, 3);
    }

    #[tokio::test]
    async fn the_preamble_before_any_content_sends_no_heartbeat() {
        // A heartbeat is client output: before `message_start` it would turn a turn that
        // fails with nothing to show into one that delivered bytes (no error frame, lease
        // consumed instead of released).
        let created = json!({ "type": "response.created", "response": {} });
        let in_progress = json!({ "type": "response.in_progress", "response": {} });
        let body = futures::stream::iter(vec![
            Ok(Bytes::from(format!("data: {created}\n\ndata: {in_progress}\n\n"))),
            Err(std::io::Error::other("connection reset")),
        ]);
        let stream = grok_responses_sse_to_anthropic_stream(body, "grok/grok-4.5", GrokStreamOptions::default());
        let items: Vec<_> = stream.collect().await;
        assert_eq!(items.len(), 1, "{items:?}");
        assert!(items[0].is_err());
    }

    #[tokio::test]
    async fn hidden_reasoning_sends_no_heartbeat_before_message_start() {
        let stream = grok_responses_sse_to_anthropic_stream(
            sse(&[
                json!({ "type": "response.output_item.added", "item": { "type": "reasoning" } }),
                json!({
                    "type": "response.output_item.added",
                    "item": { "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "Write" }
                }),
                json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_1", "delta": "{}" }),
            ]),
            "grok/grok-4.5",
            GrokStreamOptions::default().thinking_mode(GrokThinkingMode::Disabled),
        );
        // Nothing reached the client before the cut, so it must still get the error event as
        // its first bytes.
        let chunks: Vec<Bytes> = stream.map(|c| c.expect("chunk")).collect().await;
        assert!(chunks.iter().all(|c| c.as_ref() != UPSTREAM_HEARTBEAT), "{chunks:?}");
        assert!(chunks[0].starts_with(b"event: error"), "{chunks:?}");
    }

    #[tokio::test]
    async fn suppresses_thinking_blocks_when_thinking_mode_disabled() {
        let stream = grok_responses_sse_to_anthropic_stream(
            sse(&[
                json!({ "type": "response.reasoning_summary_text.delta", "delta": "should hide" }),
                json!({ "type": "response.output_text.delta", "delta": "ok" }),
                json!({ "type": "response.completed", "response": {} }),
            ]),
            "grok/grok-4.5",
            GrokStreamOptions::default().thinking_mode(GrokThinkingMode::Disabled),
        );
        let out = read_sse(stream).await;
        assert!(!out.contains("thinking"));
        assert!(out.contains(r#""text":"ok""#));
    }

    #[tokio::test]
    async fn clears_replay_state_on_completed_disabled_turns() {
        let outcomes: Arc<Mutex<Vec<GrokTurnOutcome>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = outcomes.clone();
        let stream = grok_responses_sse_to_anthropic_stream(
            sse(&[
                json!({ "type": "response.output_text.delta", "delta": "ok" }),
                json!({ "type": "response.completed", "response": {} }),
            ]),
            "grok/grok-4.5",
            GrokStreamOptions::default()
                .thinking_mode(GrokThinkingMode::Disabled)
                .on_turn_outcome(move |o| sink.lock().expect("lock").push(o)),
        );
        read_sse(stream).await;
        assert_eq!(*outcomes.lock().expect("lock"), vec![GrokTurnOutcome::Clear]);
    }

    #[tokio::test]
    async fn captures_encrypted_content_for_the_replay_cache_callback() {
        let enc = fake_grok_encrypted_content(23);
        let captured: Arc<Mutex<Option<GrokTurnOutcome>>> = Arc::new(Mutex::new(None));
        let sink = captured.clone();
        let stream = grok_responses_sse_to_anthropic_stream(
            sse(&[
                json!({ "type": "response.output_item.done", "item": { "type": "reasoning", "encrypted_content": enc } }),
                json!({ "type": "response.output_text.delta", "delta": "hi there" }),
                json!({ "type": "response.completed", "response": {} }),
            ]),
            "grok/grok-4.5",
            GrokStreamOptions::default().on_turn_outcome(move |o| {
                if matches!(o, GrokTurnOutcome::Replayable { .. }) {
                    *sink.lock().expect("lock") = Some(o);
                }
            }),
        );
        read_sse(stream).await;
        assert_eq!(
            *captured.lock().expect("lock"),
            Some(GrokTurnOutcome::Replayable { encrypted_content: enc, assistant_text: "hi there".into() })
        );
    }

    #[tokio::test]
    async fn emits_anthropic_error_when_upstream_ends_without_response_completed() {
        let stream = grok_responses_sse_to_anthropic_stream(
            sse(&[json!({ "type": "response.output_text.delta", "delta": "partial" })]),
            "grok/grok-4.5",
            GrokStreamOptions::default(),
        );
        let out = read_sse(stream).await;
        assert!(out.contains("event: error"));
        assert!(out.contains("stream ended before response.completed"));
        assert!(!out.contains("event: message_stop"));
    }

    #[tokio::test]
    async fn streams_incrementally_across_chunk_boundaries() {
        // A single SSE event split mid-JSON must still convert once reassembled.
        let chunks = vec![
            Bytes::from_static(b"data: {\"type\":\"response.output_te"),
            Bytes::from_static(b"xt.delta\",\"delta\":\"split\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{}}\n\n"),
        ];
        let stream = grok_responses_sse_to_anthropic_stream(
            futures::stream::iter(chunks.into_iter().map(Ok)),
            "grok/grok-4.5",
            GrokStreamOptions::default(),
        );
        let out = read_sse(stream).await;
        assert!(out.contains(r#""text":"split""#));
        assert!(out.contains("event: message_stop"));
    }

    // ── collect_grok_responses_sse_to_anthropic ────────────────────────────

    #[tokio::test]
    async fn builds_a_signed_thinking_block_on_the_non_stream_message() {
        let enc = fake_grok_encrypted_content(24);
        let msg = collect_grok_responses_sse_to_anthropic(
            sse(&[
                json!({
                    "type": "response.output_item.done",
                    "item": { "type": "reasoning", "encrypted_content": enc, "summary": [{ "type": "summary_text", "text": "plan" }] }
                }),
                json!({ "type": "response.output_text.delta", "delta": "done" }),
                json!({ "type": "response.completed", "response": { "usage": { "input_tokens": 3, "output_tokens": 2 } } }),
            ]),
            "grok/grok-4.5",
            GrokStreamOptions::default(),
        )
        .await;
        assert!(msg.get("error").is_none());
        assert_eq!(
            msg["content"],
            json!([
                { "type": "thinking", "thinking": "plan", "signature": enc },
                { "type": "text", "text": "done" }
            ])
        );
    }

    #[tokio::test]
    async fn returns_error_for_truncated_upstream_sse() {
        let msg = collect_grok_responses_sse_to_anthropic(
            sse(&[json!({ "type": "response.output_text.delta", "delta": "partial" })]),
            "grok/grok-4.5",
            GrokStreamOptions::default(),
        )
        .await;
        assert_eq!(msg["error"]["type"], "api_error");
    }

    #[tokio::test]
    async fn a_transport_error_mid_turn_is_an_error_not_a_partial_success() {
        let delta = json!({ "type": "response.output_text.delta", "delta": "partial" });
        let body = futures::stream::iter(vec![
            Ok(Bytes::from(format!("data: {delta}\n\n"))),
            Err(std::io::Error::other("connection reset")),
        ]);
        let msg = collect_grok_responses_sse_to_anthropic(body, "grok/grok-4.5", GrokStreamOptions::default()).await;
        assert_eq!(msg["error"]["type"], "api_error", "{msg}");
        assert!(msg.get("content").is_none(), "{msg}");
    }

    #[tokio::test]
    async fn a_transport_error_after_the_turn_completed_keeps_the_message() {
        let delta = json!({ "type": "response.output_text.delta", "delta": "done" });
        let completed = json!({
            "type": "response.completed",
            "response": { "usage": { "input_tokens": 3, "output_tokens": 1 } }
        });
        let body = futures::stream::iter(vec![
            Ok(Bytes::from(format!("data: {delta}\n\n"))),
            Ok(Bytes::from(format!("data: {completed}\n\n"))),
            Err(std::io::Error::other("connection reset")),
        ]);
        let msg = collect_grok_responses_sse_to_anthropic(body, "grok/grok-4.5", GrokStreamOptions::default()).await;
        assert!(msg.get("error").is_none(), "{msg}");
        assert_eq!(msg["stop_reason"], "end_turn");
        assert_eq!(msg["content"], json!([{ "type": "text", "text": "done" }]));
    }
}
