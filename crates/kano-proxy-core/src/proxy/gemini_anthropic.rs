//! Port of apps/api/src/proxy/gemini_anthropic.ts (docs/providers.md § Antigravity,
//! docs/api.md § Conversions).
//!
//! Anthropic Messages ↔ Gemini `GenerateContent`, for the antigravity adapter's
//! `/anthropic` surface.
//!
//! Mirrors CLIProxyAPI `internal/translator/antigravity/claude/*`: the
//! Anthropic `system` field becomes `systemInstruction`, `tool_use` /
//! `tool_result` blocks become `functionCall` / `functionResponse` parts,
//! `thinking` blocks round-trip through `thought` parts carrying
//! `thoughtSignature`, and `thinking.budget_tokens` / `output_config.effort`
//! become `thinkingConfig`.
//!
//! This is a **conversion**, not the Claude-native passthrough claude-code
//! gets: `cache_control` has no Gemini equivalent and is dropped, and the tool
//! loop guard therefore applies on this ingress like it does for grok.

use std::collections::{HashMap, VecDeque};

use bytes::Bytes;
use serde_json::{json, Map, Value};

use crate::providers::ProviderId;
use crate::proxy::gemini_wire::{
    anthropic_stop_reason, block_reason, first_finish_reason, gemini_parts, has_no_candidates,
    inline_data_of, normalize_gemini_usage, open_with_user_turn, random_hex, sanitize_json_schema,
    schema_dialect_for, unwrap_antigravity_response, SchemaDialect, SseDataLines,
};
use crate::upstream::transport::ByteStream;
use crate::utils::reasoning::{map_reasoning, parse_reasoning_effort};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid reasoning_effort")]
pub struct InvalidGeminiReasoningEffortError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GeminiThinkingMode {
    Disabled,
    Enabled,
    #[default]
    Default,
}

// ── Request: Anthropic → Gemini ────────────────────────────────────────────

fn system_to_parts(system: Option<&Value>) -> Vec<Value> {
    match system {
        Some(Value::String(s)) => {
            if s.is_empty() {
                vec![]
            } else {
                vec![json!({ "text": s })]
            }
        }
        Some(Value::Array(items)) => {
            let mut parts = Vec::new();
            for raw in items {
                if let Value::String(s) = raw {
                    if !s.is_empty() {
                        parts.push(json!({ "text": s }));
                    }
                    continue;
                }
                if !raw.is_object() {
                    continue;
                }
                if raw.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(text) = raw.get("text").and_then(Value::as_str) {
                        if !text.is_empty() {
                            parts.push(json!({ "text": text }));
                        }
                    }
                }
            }
            parts
        }
        _ => vec![],
    }
}

/// An Anthropic `tool_result` body is free-form; keep JSON as JSON, text as text.
fn tool_result_value(content: Option<&Value>) -> Value {
    match content {
        Some(Value::String(s)) => Value::String(s.clone()),
        Some(Value::Array(blocks)) => {
            let text: String = blocks
                .iter()
                .filter(|b| b.is_object())
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect();
            Value::String(text)
        }
        Some(Value::Null) | None => json!({}),
        Some(other) => other.clone(),
    }
}

fn base64_image_part(source: Option<&Value>) -> Option<Value> {
    let source = source?;
    if source.get("type").and_then(Value::as_str) != Some("base64") {
        return None;
    }
    let data = source
        .get("data")
        .and_then(Value::as_str)
        .filter(|d| !d.is_empty())?;
    let media_type = source
        .get("media_type")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .unwrap_or("image/png");
    Some(json!({ "inlineData": { "mimeType": media_type, "data": data } }))
}

fn blocks_to_parts(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::String(s)) => {
            if s.is_empty() {
                vec![]
            } else {
                vec![json!({ "text": s })]
            }
        }
        Some(Value::Array(blocks)) => {
            let mut parts = Vec::new();
            for block in blocks {
                if !block.is_object() {
                    continue;
                }
                match block.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text" => {
                        if let Some(text) = block.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                parts.push(json!({ "text": text }));
                            }
                        }
                    }
                    "thinking" => {
                        let thinking_text =
                            block.get("thinking").and_then(Value::as_str).unwrap_or("");
                        // The signature is Gemini's own opaque `thoughtSignature` coming
                        // back; echoing it is what keeps multi-turn thinking valid. Gemini
                        // itself emits signature-only thought parts (no text), so a
                        // replayed block whose text is empty but whose signature is set
                        // still counts.
                        let signature =
                            block.get("signature").and_then(Value::as_str).unwrap_or("");
                        if !thinking_text.is_empty() || !signature.is_empty() {
                            let mut part = Map::new();
                            part.insert("text".into(), json!(thinking_text));
                            part.insert("thought".into(), json!(true));
                            if !signature.is_empty() {
                                part.insert("thoughtSignature".into(), json!(signature));
                            }
                            parts.push(Value::Object(part));
                        }
                    }
                    "image" => {
                        if let Some(part) = base64_image_part(block.get("source")) {
                            parts.push(part);
                        }
                    }
                    "tool_use" => {
                        let Some(name) = block
                            .get("name")
                            .and_then(Value::as_str)
                            .filter(|n| !n.is_empty())
                        else {
                            continue;
                        };
                        let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                        let mut call = Map::new();
                        if !id.is_empty() {
                            call.insert("id".into(), json!(id));
                        }
                        call.insert("name".into(), json!(name));
                        let input = block.get("input").cloned().unwrap_or(Value::Null);
                        call.insert("args".into(), if input.is_null() { json!({}) } else { input });
                        parts.push(json!({ "functionCall": Value::Object(call) }));
                    }
                    "tool_result" => {
                        let id = block.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                        let mut response = Map::new();
                        if !id.is_empty() {
                            response.insert("id".into(), json!(id));
                        }
                        // Anthropic identifies a result only by tool_use_id; the name is
                        // filled in by the caller, which tracks id → name across the turn.
                        response.insert("name".into(), json!(""));
                        let value = tool_result_value(block.get("content"));
                        let is_error = block
                            .get("is_error")
                            .map(truthy)
                            .unwrap_or(false);
                        response.insert(
                            "response".into(),
                            if is_error {
                                json!({ "error": value })
                            } else {
                                json!({ "result": value })
                            },
                        );
                        parts.push(json!({ "functionResponse": Value::Object(response) }));
                        // A tool result can carry base64 images (screenshots, renders).
                        // Gemini's functionResponse has no image field, but the same user
                        // turn can carry inlineData parts alongside it — append them rather
                        // than silently sending the model only the textual fragment.
                        if let Some(inner_blocks) = block.get("content").and_then(Value::as_array) {
                            for inner in inner_blocks {
                                if !inner.is_object() {
                                    continue;
                                }
                                if inner.get("type").and_then(Value::as_str) != Some("image") {
                                    continue;
                                }
                                if let Some(part) = base64_image_part(inner.get("source")) {
                                    parts.push(part);
                                }
                            }
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

/// JavaScript truthiness for a JSON value.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn map_anthropic_tools(tools: Option<&Value>, dialect: SchemaDialect) -> Option<Value> {
    let tools = tools?.as_array()?;
    let mut declarations: Vec<Value> = Vec::new();
    for tool in tools {
        if !tool.is_object() {
            continue;
        }
        let name = tool.get("name").and_then(Value::as_str).unwrap_or("");
        // Anthropic server-side tools (`type: "web_search_20250305"` and friends)
        // have no function schema and no Gemini equivalent — skip, never forge one.
        let Some(input_schema) = tool.get("input_schema").filter(|s| truthy(s)) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let mut declaration = Map::new();
        declaration.insert("name".into(), json!(name));
        if let Some(description) = tool.get("description").and_then(Value::as_str) {
            declaration.insert("description".into(), json!(description));
        }
        declaration.insert("parameters".into(), sanitize_json_schema(input_schema, dialect));
        declarations.push(Value::Object(declaration));
    }
    if declarations.is_empty() {
        None
    } else {
        Some(json!([{ "functionDeclarations": declarations }]))
    }
}

fn map_anthropic_tool_choice(tool_choice: Option<&Value>) -> Option<Value> {
    let choice = tool_choice.filter(|v| v.is_object() || v.is_array())?;
    match choice.get("type").and_then(Value::as_str).unwrap_or("") {
        "auto" => Some(json!({ "functionCallingConfig": { "mode": "AUTO" } })),
        "none" => Some(json!({ "functionCallingConfig": { "mode": "NONE" } })),
        "any" => Some(json!({ "functionCallingConfig": { "mode": "ANY" } })),
        "tool" => match choice
            .get("name")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty())
        {
            Some(name) => Some(
                json!({ "functionCallingConfig": { "mode": "ANY", "allowedFunctionNames": [name] } }),
            ),
            None => Some(json!({ "functionCallingConfig": { "mode": "ANY" } })),
        },
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeminiThinking {
    pub mode: GeminiThinkingMode,
    pub thinking_config: Value,
}

/// Anthropic states thinking two ways and this proxy accepts both: a `thinking`
/// object (`disabled` / `enabled` + `budget_tokens` / `adaptive`) and the
/// effort ladder (`output_config.effort`, or the `reasoning_effort` extension).
/// Budget wins when explicitly given — it is the more specific instruction.
pub fn resolve_gemini_thinking(
    body: &Value,
) -> Result<GeminiThinking, InvalidGeminiReasoningEffortError> {
    let thinking = body.get("thinking").filter(|v| v.is_object() || v.is_array());
    let thinking_type = thinking
        .map(|t| match t.get("type") {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
        })
        .unwrap_or_default()
        .to_lowercase();

    if thinking_type == "disabled" {
        return Ok(GeminiThinking {
            mode: GeminiThinkingMode::Disabled,
            thinking_config: json!({ "thinkingBudget": 0, "includeThoughts": false }),
        });
    }

    if thinking_type == "enabled" {
        if let Some(budget) = thinking.and_then(|t| t.get("budget_tokens")).filter(|b| b.is_number())
        {
            return Ok(GeminiThinking {
                mode: GeminiThinkingMode::Enabled,
                thinking_config: json!({ "thinkingBudget": budget, "includeThoughts": true }),
            });
        }
    }

    let raw_effort = body
        .get("output_config")
        .and_then(|c| c.get("effort"))
        .filter(|v| !v.is_null())
        .or_else(|| body.get("reasoning_effort"));
    let parsed = parse_reasoning_effort(raw_effort);
    if parsed.is_invalid() {
        return Err(InvalidGeminiReasoningEffortError);
    }
    if let Some(effort) = parsed.effort() {
        let mapped = map_reasoning(ProviderId::Antigravity, Some(effort));
        let thinking = mapped.get("thinkingConfig");
        let disabled = thinking.is_some_and(|c| c.get("thinkingBudget").is_some());
        let mut config = thinking.and_then(Value::as_object).cloned().unwrap_or_default();
        config.insert("includeThoughts".into(), json!(!disabled));
        return Ok(GeminiThinking {
            mode: if disabled { GeminiThinkingMode::Disabled } else { GeminiThinkingMode::Enabled },
            thinking_config: Value::Object(config),
        });
    }

    // Nothing asked for: let the model decide, but ask to see the thoughts so a
    // client that renders thinking blocks gets them.
    Ok(GeminiThinking {
        mode: GeminiThinkingMode::Default,
        thinking_config: json!({ "includeThoughts": true }),
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnthropicToGeminiResult {
    pub request: Value,
    pub thinking_mode: GeminiThinkingMode,
}

/// Anthropic Messages body → the inner Gemini `request`. The adapter wraps it
/// in the antigravity envelope.
pub fn anthropic_to_gemini_request(
    body: &Value,
) -> Result<AnthropicToGeminiResult, InvalidGeminiReasoningEffortError> {
    let mut contents: Vec<Value> = Vec::new();
    // tool_use id → name, so a later `tool_result` can be named for Gemini.
    let mut call_names: HashMap<String, String> = HashMap::new();

    let empty = Vec::new();
    let messages = body.get("messages").and_then(Value::as_array).unwrap_or(&empty);
    for message in messages {
        if !message.is_object() {
            continue;
        }
        let role = if message.get("role").and_then(Value::as_str) == Some("assistant") {
            "model"
        } else {
            "user"
        };
        let mut raw_parts = blocks_to_parts(message.get("content"));
        let mut dropped = vec![false; raw_parts.len()];
        for index in 0..raw_parts.len() {
            let is_call = raw_parts[index]
                .get("functionCall")
                .and_then(|c| c.get("name"))
                .and_then(Value::as_str)
                .is_some_and(|n| !n.is_empty());
            if is_call {
                // Gemini requires a function-call signature to remain on the
                // functionCall part. Anthropic has no corresponding field on tool_use,
                // so the response emits it on the adjacent thinking block — the textual
                // one when thinking text was streaming, a signature-only one otherwise.
                // Move it back: a signature-only block is pure transport and is
                // dropped, a textual one stays as an unsigned thought part. Replaying
                // an unsigned functionCall makes Google reject the turn as missing
                // thought_signature in the functionCall part.
                // The carrier is the nearest signed thought part before this call,
                // not necessarily the adjacent one: the response flushes thinking
                // before the text that preceded the call, so a think-then-say-then-
                // call turn replays as [thought, thought(sig), text, tool_use].
                // Never reach past an earlier call — its signature is its own.
                let mut carrier: Option<usize> = None;
                if raw_parts[index].get("thoughtSignature").is_none() {
                    for back in (0..index).rev() {
                        let candidate = &raw_parts[back];
                        if candidate.get("functionCall").is_some_and(truthy) {
                            break;
                        }
                        let signed = candidate
                            .get("thought")
                            .is_some_and(truthy)
                            && candidate
                                .get("thoughtSignature")
                                .is_some_and(truthy);
                        if signed {
                            carrier = Some(back);
                            break;
                        }
                    }
                }
                if let Some(at) = carrier {
                    let signature = raw_parts[at]
                        .get("thoughtSignature")
                        .cloned()
                        .unwrap_or(Value::Null);
                    if let Some(part) = raw_parts[index].as_object_mut() {
                        part.insert("thoughtSignature".into(), signature);
                    }
                    let carrier_empty =
                        raw_parts[at].get("text").and_then(Value::as_str) == Some("");
                    if carrier_empty {
                        dropped[at] = true;
                    } else if let Some(part) = raw_parts[at].as_object_mut() {
                        part.shift_remove("thoughtSignature");
                    }
                }
                let call = &raw_parts[index]["functionCall"];
                if let (Some(id), Some(name)) = (
                    call.get("id").and_then(Value::as_str).filter(|i| !i.is_empty()),
                    call.get("name").and_then(Value::as_str),
                ) {
                    call_names.insert(id.to_string(), name.to_string());
                }
            }
            let needs_name = raw_parts[index]
                .get("functionResponse")
                .is_some_and(|r| !truthy(r.get("name").unwrap_or(&Value::Null)));
            if needs_name {
                let id = raw_parts[index]["functionResponse"]
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let name = call_names.get(&id).cloned().unwrap_or_else(|| "tool".into());
                if let Some(response) = raw_parts[index]["functionResponse"].as_object_mut() {
                    response.insert("name".into(), json!(name));
                }
            }
        }
        let parts: Vec<Value> = raw_parts
            .drain(..)
            .zip(dropped)
            .filter(|(_, drop)| !drop)
            .map(|(part, _)| part)
            .collect();
        if parts.is_empty() {
            continue;
        }
        let existing = contents
            .last_mut()
            .filter(|last| last.get("role").and_then(Value::as_str) == Some(role))
            .and_then(|last| last.get_mut("parts"))
            .and_then(Value::as_array_mut);
        match existing {
            Some(existing) => existing.extend(parts),
            None => contents.push(json!({ "role": role, "parts": parts })),
        }
    }
    open_with_user_turn(&mut contents);

    let mut generation_config = Map::new();
    if let Some(t) = body.get("temperature").filter(|v| v.is_number()) {
        generation_config.insert("temperature".into(), t.clone());
    }
    if let Some(p) = body.get("top_p").filter(|v| v.is_number()) {
        generation_config.insert("topP".into(), p.clone());
    }
    if let Some(m) = body.get("max_tokens").filter(|v| v.is_number()) {
        generation_config.insert("maxOutputTokens".into(), m.clone());
    }
    if let Some(stop) = body
        .get("stop_sequences")
        .and_then(Value::as_array)
        .filter(|s| !s.is_empty())
    {
        let kept: Vec<Value> = stop.iter().filter(|s| s.is_string()).cloned().collect();
        generation_config.insert("stopSequences".into(), Value::Array(kept));
    }
    let thinking = resolve_gemini_thinking(body)?;
    generation_config.insert("thinkingConfig".into(), thinking.thinking_config.clone());

    let system_parts = system_to_parts(body.get("system"));
    // Claude behind Antigravity rejects a union its Gemini sibling accepts, so
    // the schema dialect follows the model family (gemini_wire.rs).
    let dialect = schema_dialect_for(body.get("model").and_then(Value::as_str).unwrap_or(""));
    let tools = map_anthropic_tools(body.get("tools"), dialect);
    let tool_config = tools
        .as_ref()
        .and_then(|_| map_anthropic_tool_choice(body.get("tool_choice")));

    let mut request = Map::new();
    request.insert("contents".into(), json!(contents));
    if !system_parts.is_empty() {
        request.insert(
            "systemInstruction".into(),
            json!({ "role": "user", "parts": system_parts }),
        );
    }
    if let Some(tools) = tools {
        request.insert("tools".into(), tools);
    }
    if let Some(tool_config) = tool_config {
        request.insert("toolConfig".into(), tool_config);
    }
    request.insert("generationConfig".into(), Value::Object(generation_config));

    Ok(AnthropicToGeminiResult { request: Value::Object(request), thinking_mode: thinking.mode })
}

// ── Response: Gemini → Anthropic ───────────────────────────────────────────

/// Only the fields this frame actually reported. Gemini repeats
/// `promptTokenCount` on later frames without always repeating the output
/// counts, so a caller merging frame-by-frame must not receive a defaulted
/// `output_tokens: 0` that would overwrite a real number it already had.
fn usage_to_anthropic(resp: Option<&Value>) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(usage) = normalize_gemini_usage(resp.and_then(|r| r.get("usageMetadata"))) else {
        return out;
    };
    let cache_read = usage.cached_tokens.unwrap_or(0);
    if let Some(prompt) = usage.prompt_tokens {
        // Anthropic's `input_tokens` excludes cache reads; Gemini's
        // `promptTokenCount` includes them, so the cached half is subtracted out.
        // The two are reported as a **pair**, `cache_read_input_tokens` included
        // when it is zero: a merging caller that took a later frame's
        // `promptTokenCount` while keeping an earlier frame's cache number would
        // count the cached tokens twice.
        out.insert("input_tokens".into(), json!((prompt - cache_read).max(0)));
        out.insert("cache_read_input_tokens".into(), json!(cache_read));
    }
    if usage.completion_tokens.is_some() || usage.reasoning_tokens.is_some() {
        out.insert(
            "output_tokens".into(),
            json!(usage.completion_tokens.unwrap_or(0) + usage.reasoning_tokens.unwrap_or(0)),
        );
    }
    out
}

fn tool_use_block(part: &Value, index: usize) -> Option<Value> {
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
        .unwrap_or_else(|| format!("toolu_{index}_{}", random_hex(16)));
    let input = call.get("args").cloned().unwrap_or(Value::Null);
    let input = if input.is_null() { json!({}) } else { input };
    Some(json!({ "type": "tool_use", "id": id, "name": name, "input": input }))
}

/// Non-stream `generateContent` body → an Anthropic `message`.
pub fn gemini_response_to_anthropic(
    json_body: &Value,
    model: &str,
    thinking_mode: GeminiThinkingMode,
) -> Value {
    let resp = unwrap_antigravity_response(json_body);
    let emit_thinking = thinking_mode != GeminiThinkingMode::Disabled;
    let mut content: Vec<Value> = Vec::new();
    let mut saw_tool_call = false;
    let mut text = String::new();
    let mut thinking = String::new();
    let mut thinking_signature = String::new();

    // A signature with no accumulated text still makes a block — dropping it
    // would strip `thinking.signature` and break the client's next replay.
    macro_rules! flush_thinking {
        () => {
            if !thinking.is_empty() || !thinking_signature.is_empty() {
                let mut block = Map::new();
                block.insert("type".into(), json!("thinking"));
                block.insert("thinking".into(), json!(thinking));
                if !thinking_signature.is_empty() {
                    block.insert("signature".into(), json!(thinking_signature));
                }
                content.push(Value::Object(block));
                thinking.clear();
                thinking_signature.clear();
            }
        };
    }
    macro_rules! flush_text {
        () => {
            if !text.is_empty() {
                content.push(json!({ "type": "text", "text": text }));
                text.clear();
            }
        };
    }

    for part in gemini_parts(resp) {
        let signature = part
            .get("thoughtSignature")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        if part.get("functionCall").is_some_and(truthy) {
            // Gemini can sign the functionCall part itself. Anthropic tool_use has
            // no signature field, so it rides on the adjacent thinking block (a
            // signature-only one when nothing was accumulated) — the replay path
            // already restores signed thinking blocks as signed thought parts.
            if emit_thinking {
                if let Some(sig) = signature {
                    thinking_signature = sig.to_string();
                }
            }
            flush_thinking!();
            flush_text!();
            if let Some(block) = tool_use_block(part, content.len()) {
                content.push(block);
                saw_tool_call = true;
            }
            continue;
        }
        if let Some(inline) = inline_data_of(part) {
            flush_thinking!();
            flush_text!();
            content.push(json!({
                "type": "image",
                "source": { "type": "base64", "media_type": inline.mime_type, "data": inline.data }
            }));
            continue;
        }
        let part_text = part.get("text").and_then(Value::as_str).unwrap_or("");
        if part.get("thought").is_some_and(truthy) {
            if !emit_thinking {
                continue;
            }
            // The signature can ride on a thought part with no visible text — it
            // must be captured before the text guard or it never reaches the block.
            if part_text.is_empty() && signature.is_none() {
                continue;
            }
            flush_text!();
            thinking.push_str(part_text);
            if let Some(sig) = signature {
                thinking_signature = sig.to_string();
            }
            continue;
        }
        if part_text.is_empty() {
            continue;
        }
        // Gemini signs plain text parts too (think-then-answer, no tool call).
        // The capture must precede the flush or the signature never reaches a
        // block: it rides the adjacent thinking block, a signature-only one when
        // nothing was accumulated, exactly like a functionCall signature.
        if emit_thinking {
            if let Some(sig) = signature {
                thinking_signature = sig.to_string();
            }
        }
        flush_thinking!();
        text.push_str(part_text);
    }
    flush_thinking!();
    flush_text!();

    // A safety-blocked prompt is a valid response with no candidates and a
    // `promptFeedback.blockReason` — surface it as a refusal, never as a
    // successful empty `end_turn` the client cannot tell apart from real output.
    let blocked = has_no_candidates(resp) && block_reason(resp).is_some();
    let stop_reason = if blocked {
        "refusal"
    } else {
        anthropic_stop_reason(first_finish_reason(resp), saw_tool_call)
    };

    let mut usage = Map::new();
    usage.insert("input_tokens".into(), json!(0));
    usage.insert("output_tokens".into(), json!(0));
    for (k, v) in usage_to_anthropic(resp) {
        usage.insert(k, v);
    }

    json!({
        "id": format!("msg_{}", random_hex(24)),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": Value::Null,
        "usage": Value::Object(usage)
    })
}

// ── Streaming ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Only text and thinking blocks stay open across parts; a tool_use or image
/// block opens, delivers and stops within one frame.
enum BlockKind {
    Text,
    Thinking,
}

struct AnthropicStream {
    lines: SseDataLines,
    msg_id: String,
    model: String,
    emit_thinking: bool,
    out: VecDeque<Bytes>,
    /// The TypeScript controller was closed: nothing more is emitted and the
    /// stream ends once what is queued has been handed downstream.
    done: bool,
    next_index: usize,
    open: Option<(BlockKind, usize)>,
    pending_signature: String,
    saw_tool_call: bool,
    finish_reason: Option<String>,
    /// A candidate reported a `finishReason` — Gemini's terminal frame. A clean
    /// EOF without one is a truncated stream, not a completion.
    saw_terminal: bool,
    block_reason: String,
    /// Merged field-wise across frames — see `usage_to_anthropic`.
    usage: Map<String, Value>,
    /// A frame has reported `promptTokenCount`, so `input_tokens` is real.
    saw_input_tokens: bool,
    message_started: bool,
}

impl AnthropicStream {
    fn emit_raw(&mut self, event: &str, data: Value) {
        if self.done {
            return;
        }
        let text = format!(
            "event: {event}\ndata: {}\n\n",
            serde_json::to_string(&data).unwrap_or_default()
        );
        self.out.push_back(Bytes::from(text));
    }

    /// Every event except `error` is preceded by `message_start`, which is held
    /// back until a frame reported `promptTokenCount`. If content is ready and no
    /// count ever arrived, the turn fails instead of shipping a zero: a wrong
    /// context size is worse than a visible error, and there is no honest number
    /// to substitute (docs/api.md § Streaming).
    fn emit(&mut self, event: &str, data: Value) {
        if self.done {
            return;
        }
        if event != "error" && !self.message_started {
            if !self.saw_input_tokens {
                self.emit_raw(
                    "error",
                    json!({
                        "type": "error",
                        "error": {
                            "type": "api_error",
                            "message": "upstream reported no prompt token count before its first content frame"
                        }
                    }),
                );
                self.done = true;
                return;
            }
            self.message_started = true;
            let mut usage = self.usage.clone();
            usage.insert("output_tokens".into(), json!(0));
            let start = json!({
                "type": "message_start",
                "message": {
                    "id": self.msg_id,
                    "type": "message",
                    "role": "assistant",
                    "model": self.model,
                    "content": [],
                    "stop_reason": Value::Null,
                    "stop_sequence": Value::Null,
                    "usage": Value::Object(usage)
                }
            });
            self.emit_raw("message_start", start);
        }
        self.emit_raw(event, data);
    }

    fn close_block(&mut self) {
        let Some((kind, index)) = self.open else {
            return;
        };
        if kind == BlockKind::Thinking && !self.pending_signature.is_empty() {
            let signature = std::mem::take(&mut self.pending_signature);
            self.emit(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": { "type": "signature_delta", "signature": signature }
                }),
            );
        }
        self.emit(
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": index }),
        );
        self.open = None;
    }

    fn open_block(&mut self, kind: BlockKind, block: Value) {
        if self.open.map(|(k, _)| k) == Some(kind) {
            return;
        }
        self.close_block();
        let index = self.next_index;
        self.next_index += 1;
        self.open = Some((kind, index));
        self.emit(
            "content_block_start",
            json!({ "type": "content_block_start", "index": index, "content_block": block }),
        );
    }

    /// A clean EOF is not a completion: without a terminal Gemini frame the
    /// stream was truncated (no error is raised for a quiet network close), so
    /// end the turn with the documented error event, never a fabricated
    /// `end_turn`. A blocked prompt is the one no-terminal shape that *is* a real
    /// answer — Gemini reports it via promptFeedback, no candidate.
    fn terminal(&mut self) {
        self.close_block();
        if !self.saw_terminal && self.block_reason.is_empty() {
            self.emit(
                "error",
                json!({
                    "type": "error",
                    "error": { "type": "api_error", "message": "upstream stream ended before completion" }
                }),
            );
            self.done = true;
            return;
        }
        let stop_reason = if !self.saw_terminal && !self.block_reason.is_empty() {
            "refusal".to_string()
        } else {
            anthropic_stop_reason(self.finish_reason.as_deref(), self.saw_tool_call).to_string()
        };
        // The whole usage object, not just `output_tokens`: Gemini reports input
        // counts on the same frames, and `message_start` went out before any of
        // them arrived, so this is the client's only chance to learn them (the
        // repo's Anthropic usage sniffer merges field-wise).
        let usage = Value::Object(self.usage.clone());
        self.emit(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": stop_reason, "stop_sequence": Value::Null },
                "usage": usage
            }),
        );
        self.emit("message_stop", json!({ "type": "message_stop" }));
        self.done = true;
    }

    fn frame(&mut self, data: &str) {
        let Ok(json_value) = serde_json::from_str::<Value>(data) else {
            return;
        };
        let resp = unwrap_antigravity_response(&json_value);
        // Before anything is emitted from this frame: `message_start` is gated on a
        // real `input_tokens`, and Gemini carries the count on the same frame as the
        // first content part, so reading it after the parts loop would fail the turn
        // on a frame that did report it.
        if resp.and_then(|r| r.get("usageMetadata")).is_some() {
            let reported = usage_to_anthropic(resp);
            // `usage` is seeded with a zero, so the flag has to key off what *this
            // frame* carried, not off the merged object.
            if reported.contains_key("input_tokens") {
                self.saw_input_tokens = true;
            }
            for (k, v) in reported {
                self.usage.insert(k, v);
            }
        }
        if let Some(reason) = block_reason(resp) {
            self.block_reason = reason.to_string();
        }

        for part in gemini_parts(resp).to_vec() {
            let signature = part
                .get("thoughtSignature")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            if part.get("functionCall").is_some_and(truthy) {
                // A signature on the functionCall part itself rides on the adjacent
                // thinking block, whose closing signature_delta goes out right before
                // the tool_use block opens.
                if self.emit_thinking {
                    if let Some(sig) = signature {
                        self.open_block(BlockKind::Thinking, json!({ "type": "thinking", "thinking": "" }));
                        self.pending_signature = sig;
                    }
                }
                self.close_block();
                let Some(block) = tool_use_block(&part, self.next_index) else {
                    continue;
                };
                self.saw_tool_call = true;
                let index = self.next_index;
                self.next_index += 1;
                let mut content_block = block.as_object().cloned().unwrap_or_default();
                let input = content_block
                    .insert("input".into(), json!({}))
                    .unwrap_or(Value::Null);
                self.emit(
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": Value::Object(content_block)
                    }),
                );
                // Gemini delivers a complete `args` object in one frame, so the whole
                // JSON goes out as a single partial_json delta.
                let partial = serde_json::to_string(&if input.is_null() { json!({}) } else { input })
                    .unwrap_or_else(|_| "{}".into());
                self.emit(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": { "type": "input_json_delta", "partial_json": partial }
                    }),
                );
                self.emit(
                    "content_block_stop",
                    json!({ "type": "content_block_stop", "index": index }),
                );
                continue;
            }
            if let Some(inline) = inline_data_of(&part) {
                self.close_block();
                let index = self.next_index;
                self.next_index += 1;
                self.emit(
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {
                            "type": "image",
                            "source": { "type": "base64", "media_type": inline.mime_type, "data": inline.data }
                        }
                    }),
                );
                self.emit(
                    "content_block_stop",
                    json!({ "type": "content_block_stop", "index": index }),
                );
                continue;
            }
            let part_text = part.get("text").and_then(Value::as_str).unwrap_or("");
            if part.get("thought").is_some_and(truthy) {
                if !self.emit_thinking {
                    continue;
                }
                // A signature-only thought part (no text) is a valid shape — the block
                // still opens so close_block emits its signature_delta.
                if part_text.is_empty() && signature.is_none() {
                    continue;
                }
                self.open_block(BlockKind::Thinking, json!({ "type": "thinking", "thinking": "" }));
                if !part_text.is_empty() {
                    let index = self.open.map(|(_, i)| i).unwrap_or(0);
                    self.emit(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": { "type": "thinking_delta", "thinking": part_text }
                        }),
                    );
                }
                if let Some(sig) = signature {
                    self.pending_signature = sig;
                }
                continue;
            }
            if part_text.is_empty() {
                continue;
            }
            // A signature on a plain text part rides on the adjacent thinking block
            // (opened empty when nothing was streaming), whose closing
            // signature_delta goes out right before the text block opens.
            if self.emit_thinking {
                if let Some(sig) = signature {
                    self.open_block(BlockKind::Thinking, json!({ "type": "thinking", "thinking": "" }));
                    self.pending_signature = sig;
                }
            }
            self.open_block(BlockKind::Text, json!({ "type": "text", "text": "" }));
            let index = self.open.map(|(_, i)| i).unwrap_or(0);
            self.emit(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": { "type": "text_delta", "text": part_text }
                }),
            );
        }

        if let Some(reason) = first_finish_reason(resp).filter(|r| !r.is_empty()) {
            self.saw_terminal = true;
            self.finish_reason = Some(reason.to_string());
        }
    }

    async fn step(&mut self) -> Option<Bytes> {
        loop {
            if let Some(bytes) = self.out.pop_front() {
                return Some(bytes);
            }
            if self.done {
                return None;
            }
            match self.lines.next_data().await {
                None => self.terminal(),
                Some(Ok(data)) if data == "[DONE]" => self.terminal(),
                Some(Ok(data)) => self.frame(&data),
                // Mid-stream failure ends the turn as an Anthropic `error` event,
                // not a fabricated message_stop (docs/api.md § Errors).
                Some(Err(e)) => {
                    self.emit(
                        "error",
                        json!({ "type": "error", "error": { "type": "api_error", "message": e.to_string() } }),
                    );
                    self.done = true;
                }
            }
        }
    }
}

/// `streamGenerateContent?alt=sse` → Anthropic Messages SSE. Content blocks
/// open and close as the part type changes; a thinking block emits its
/// `signature_delta` right before it closes, so a client that echoes the
/// signature keeps the next turn's thinking valid.
///
/// `message_start` is emitted from the pump, not up front, once a frame has
/// carried `usageMetadata`. Anthropic clients read the context size off its
/// `usage.input_tokens`, and Gemini reports counts on its stream frames, not
/// before them — emitting the event up front can only put a zero there
/// (docs/api.md § Streaming).
///
/// Pull-driven, like the OpenAI converter; dropping the returned stream drops
/// the upstream body, which is what the TypeScript `cancel()` hook did.
pub fn gemini_sse_to_anthropic_stream(
    body: ByteStream,
    model: &str,
    thinking_mode: GeminiThinkingMode,
) -> ByteStream {
    let mut usage = Map::new();
    usage.insert("input_tokens".into(), json!(0));
    usage.insert("output_tokens".into(), json!(0));
    let state = AnthropicStream {
        lines: SseDataLines::new(body),
        msg_id: format!("msg_{}", random_hex(24)),
        model: model.to_string(),
        emit_thinking: thinking_mode != GeminiThinkingMode::Disabled,
        out: VecDeque::new(),
        done: false,
        next_index: 0,
        open: None,
        pending_signature: String::new(),
        saw_tool_call: false,
        finish_reason: None,
        saw_terminal: false,
        block_reason: String::new(),
        usage,
        saw_input_tokens: false,
        message_started: false,
    };
    Box::pin(futures::stream::unfold(state, |mut state| async move {
        state.step().await.map(|bytes| (Ok(bytes), state))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    fn request(body: Value) -> Value {
        anthropic_to_gemini_request(&body).expect("valid request").request
    }

    fn body_from(chunks: Vec<Vec<u8>>) -> ByteStream {
        Box::pin(futures::stream::iter(
            chunks
                .into_iter()
                .map(|c| Ok::<Bytes, std::io::Error>(Bytes::from(c)))
                .collect::<Vec<_>>(),
        ))
    }

    /// Frames as one SSE body split at awkward boundaries (5-byte chunks), so
    /// frames and JSON tokens are cut mid-way.
    fn sse(frames: &[Value]) -> ByteStream {
        let text: String = frames
            .iter()
            .map(|f| format!("data: {}\n\n", serde_json::to_string(f).unwrap()))
            .collect();
        body_from(text.into_bytes().chunks(5).map(<[u8]>::to_vec).collect())
    }

    async fn read_sse(mut stream: ByteStream) -> String {
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.unwrap());
        }
        String::from_utf8(out).unwrap()
    }

    /// `[(event, data)]` in emission order.
    fn events(raw: &str) -> Vec<(String, Value)> {
        raw.split("\n\n")
            .filter(|block| !block.trim().is_empty())
            .map(|block| {
                let mut event = String::new();
                let mut data = String::from("{}");
                for line in block.lines() {
                    if let Some(rest) = line.strip_prefix("event:") {
                        event = rest.trim().to_string();
                    } else if let Some(rest) = line.strip_prefix("data:") {
                        data = rest.trim().to_string();
                    }
                }
                (event, serde_json::from_str(&data).unwrap())
            })
            .collect()
    }

    fn names(seq: &[(String, Value)]) -> Vec<&str> {
        seq.iter().map(|(e, _)| e.as_str()).collect()
    }

    // ── resolveGeminiThinking ──────────────────────────────────────────────

    #[test]
    fn disables_thinking_outright() {
        let out = resolve_gemini_thinking(&json!({ "thinking": { "type": "disabled" } })).unwrap();
        assert_eq!(out.mode, GeminiThinkingMode::Disabled);
        assert_eq!(
            out.thinking_config,
            json!({ "thinkingBudget": 0, "includeThoughts": false })
        );
    }

    #[test]
    fn maps_an_explicit_budget_verbatim() {
        let out =
            resolve_gemini_thinking(&json!({ "thinking": { "type": "enabled", "budget_tokens": 4096 } }))
                .unwrap();
        assert_eq!(out.mode, GeminiThinkingMode::Enabled);
        assert_eq!(
            out.thinking_config,
            json!({ "thinkingBudget": 4096, "includeThoughts": true })
        );
    }

    #[test]
    fn maps_output_config_effort_to_a_thinking_level() {
        let out = resolve_gemini_thinking(&json!({ "output_config": { "effort": "low" } })).unwrap();
        assert_eq!(out.mode, GeminiThinkingMode::Enabled);
        assert_eq!(
            out.thinking_config,
            json!({ "thinkingLevel": "low", "includeThoughts": true })
        );
    }

    #[test]
    fn clamps_an_above_ceiling_effort_down_to_high() {
        for token in ["xhigh", "max"] {
            let out =
                resolve_gemini_thinking(&json!({ "output_config": { "effort": token } })).unwrap();
            assert_eq!(out.thinking_config["thinkingLevel"], "high");
        }
    }

    #[test]
    fn effort_none_disables_thinking_through_the_ladder() {
        let out = resolve_gemini_thinking(&json!({ "reasoning_effort": "none" })).unwrap();
        assert_eq!(out.mode, GeminiThinkingMode::Disabled);
        assert_eq!(
            out.thinking_config,
            json!({ "thinkingBudget": 0, "includeThoughts": false })
        );
    }

    #[test]
    fn asks_for_thoughts_by_default() {
        let out = resolve_gemini_thinking(&json!({})).unwrap();
        assert_eq!(out.mode, GeminiThinkingMode::Default);
        assert_eq!(out.thinking_config, json!({ "includeThoughts": true }));
    }

    #[test]
    fn rejects_a_garbage_effort() {
        assert_eq!(
            resolve_gemini_thinking(&json!({ "reasoning_effort": "turbo" })),
            Err(InvalidGeminiReasoningEffortError)
        );
    }

    // ── anthropicToGeminiRequest ───────────────────────────────────────────

    #[test]
    fn moves_system_to_system_instruction_and_maps_roles() {
        let out = request(json!({
            "system": "be terse",
            "messages": [
                { "role": "user", "content": "hi" },
                { "role": "assistant", "content": [{ "type": "text", "text": "hello" }] }
            ],
            "max_tokens": 64
        }));
        assert_eq!(
            out["systemInstruction"],
            json!({ "role": "user", "parts": [{ "text": "be terse" }] })
        );
        assert_eq!(
            out["contents"],
            json!([
                { "role": "user", "parts": [{ "text": "hi" }] },
                { "role": "model", "parts": [{ "text": "hello" }] }
            ])
        );
        assert_eq!(out["generationConfig"]["maxOutputTokens"], 64);
    }

    #[test]
    fn opens_an_assistant_first_history_with_a_user_turn() {
        let out = request(json!({
            "system": "boot",
            "messages": [
                { "role": "assistant", "content": [{ "type": "tool_use", "id": "toolu_boot", "name": "read_file", "input": { "path": "a.md" } }] },
                { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "toolu_boot", "content": "# a" }] },
                { "role": "user", "content": "hi" }
            ]
        }));
        assert_eq!(
            out["contents"][0],
            json!({ "role": "user", "parts": [{ "text": "(conversation start)" }] })
        );
        assert_eq!(
            out["contents"][1],
            json!({ "role": "model", "parts": [{ "functionCall": { "id": "toolu_boot", "name": "read_file", "args": { "path": "a.md" } } }] })
        );
        assert_eq!(out["contents"][2]["role"], "user");
        assert_eq!(out["contents"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn names_a_tool_result_from_the_tool_use_it_answers() {
        let out = request(json!({
            "messages": [
                { "role": "user", "content": "weather?" },
                { "role": "assistant", "content": [{ "type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": { "city": "Taipei" } }] },
                { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "toolu_1", "content": "30C" }] }
            ]
        }));
        assert_eq!(
            out["contents"][1]["parts"],
            json!([{ "functionCall": { "id": "toolu_1", "name": "get_weather", "args": { "city": "Taipei" } } }])
        );
        assert_eq!(
            out["contents"][2]["parts"],
            json!([{ "functionResponse": { "id": "toolu_1", "name": "get_weather", "response": { "result": "30C" } } }])
        );
    }

    #[test]
    fn keeps_base64_images_nested_in_a_tool_result() {
        let out = request(json!({
            "messages": [{ "role": "user", "content": [{
                "type": "tool_result",
                "tool_use_id": "t1",
                "content": [
                    { "type": "text", "text": "screenshot taken" },
                    { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAAB" } }
                ]
            }] }]
        }));
        // Gemini's functionResponse has no image field; the image rides along in
        // the same user turn instead of being silently discarded.
        assert_eq!(
            out["contents"][0]["parts"],
            json!([
                { "functionResponse": { "id": "t1", "name": "tool", "response": { "result": "screenshot taken" } } },
                { "inlineData": { "mimeType": "image/png", "data": "AAAB" } }
            ])
        );
    }

    #[test]
    fn replays_a_standalone_signature_only_thinking_block() {
        let out = request(json!({
            "messages": [
                { "role": "user", "content": "hi" },
                { "role": "assistant", "content": [{ "type": "thinking", "thinking": "", "signature": "sig-1" }] }
            ]
        }));
        assert_eq!(
            out["contents"][1]["parts"],
            json!([{ "text": "", "thought": true, "thoughtSignature": "sig-1" }])
        );
    }

    #[test]
    fn restores_a_tool_use_signature_to_its_function_call_part() {
        let out = request(json!({
            "messages": [
                { "role": "user", "content": "search" },
                { "role": "assistant", "content": [
                    { "type": "thinking", "thinking": "", "signature": "sig-fc" },
                    { "type": "tool_use", "id": "toolu_1", "name": "search", "input": { "q": "x" } }
                ] },
                { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "toolu_1", "content": "ok" }] }
            ]
        }));
        assert_eq!(
            out["contents"][1]["parts"],
            json!([{
                "functionCall": { "id": "toolu_1", "name": "search", "args": { "q": "x" } },
                "thoughtSignature": "sig-fc"
            }])
        );
    }

    #[test]
    fn moves_a_textual_thinking_signature_to_a_following_tool_call() {
        let out = request(json!({
            "messages": [{ "role": "assistant", "content": [
                { "type": "thinking", "thinking": "reasoning", "signature": "sig-thought" },
                { "type": "tool_use", "id": "toolu_1", "name": "search", "input": {} }
            ] }]
        }));
        // An assistant-first fixture: the converter opens it with a user turn.
        assert_eq!(
            out["contents"][1]["parts"],
            json!([
                { "text": "reasoning", "thought": true },
                { "functionCall": { "id": "toolu_1", "name": "search", "args": {} }, "thoughtSignature": "sig-thought" }
            ])
        );
    }

    #[test]
    fn carries_a_signature_past_the_text_that_preceded_the_tool_call() {
        // The response side flushes thinking before the pre-call text, so this
        // is exactly what a think-then-say-then-call turn replays as.
        let out = request(json!({
            "messages": [
                { "role": "user", "content": "send it" },
                { "role": "assistant", "content": [
                    { "type": "thinking", "thinking": "plan" },
                    { "type": "thinking", "thinking": "", "signature": "sig-call" },
                    { "type": "text", "text": "Sending now." },
                    { "type": "tool_use", "id": "toolu_1", "name": "send_message", "input": {} }
                ] }
            ]
        }));
        assert_eq!(
            out["contents"][1]["parts"],
            json!([
                { "text": "plan", "thought": true },
                { "text": "Sending now." },
                { "functionCall": { "id": "toolu_1", "name": "send_message", "args": {} }, "thoughtSignature": "sig-call" }
            ])
        );
    }

    #[test]
    fn does_not_borrow_an_earlier_calls_signature_across_that_call() {
        let out = request(json!({
            "messages": [
                { "role": "user", "content": "run both" },
                { "role": "assistant", "content": [
                    { "type": "thinking", "thinking": "", "signature": "sig-first" },
                    { "type": "tool_use", "id": "toolu_1", "name": "a", "input": {} },
                    { "type": "text", "text": "and" },
                    { "type": "tool_use", "id": "toolu_2", "name": "b", "input": {} }
                ] }
            ]
        }));
        assert_eq!(
            out["contents"][1]["parts"],
            json!([
                { "functionCall": { "id": "toolu_1", "name": "a", "args": {} }, "thoughtSignature": "sig-first" },
                { "text": "and" },
                { "functionCall": { "id": "toolu_2", "name": "b", "args": {} } }
            ])
        );
    }

    #[test]
    fn signs_parallel_tool_calls_after_streamed_thinking() {
        // The shape a real Claude Code turn replays: the first call's signature
        // rides the textual thinking block, later calls get a signature-only one.
        let out = request(json!({
            "messages": [
                { "role": "user", "content": "run both" },
                { "role": "assistant", "content": [
                    { "type": "thinking", "thinking": "reasoning", "signature": "sig-1" },
                    { "type": "tool_use", "id": "toolu_1", "name": "Bash", "input": { "command": "ls" } },
                    { "type": "thinking", "thinking": "", "signature": "sig-2" },
                    { "type": "tool_use", "id": "toolu_2", "name": "Bash", "input": { "command": "pwd" } }
                ] },
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "toolu_1", "content": "a" },
                    { "type": "tool_result", "tool_use_id": "toolu_2", "content": "b" }
                ] }
            ]
        }));
        assert_eq!(
            out["contents"][1]["parts"],
            json!([
                { "text": "reasoning", "thought": true },
                { "functionCall": { "id": "toolu_1", "name": "Bash", "args": { "command": "ls" } }, "thoughtSignature": "sig-1" },
                { "functionCall": { "id": "toolu_2", "name": "Bash", "args": { "command": "pwd" } }, "thoughtSignature": "sig-2" }
            ])
        );
    }

    #[test]
    fn marks_an_errored_tool_result_as_an_error() {
        let out = request(json!({
            "messages": [{ "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "t1", "is_error": true, "content": "boom" }
            ] }]
        }));
        assert_eq!(
            out["contents"][0]["parts"][0]["functionResponse"]["response"],
            json!({ "error": "boom" })
        );
    }

    #[test]
    fn round_trips_a_thinking_block_with_its_signature() {
        let out = request(json!({
            "messages": [{ "role": "assistant", "content": [
                { "type": "thinking", "thinking": "hmm", "signature": "sig-abc" },
                { "type": "text", "text": "done" }
            ] }]
        }));
        // An assistant-first fixture: the converter opens it with a user turn.
        assert_eq!(
            out["contents"][1]["parts"],
            json!([
                { "text": "hmm", "thought": true, "thoughtSignature": "sig-abc" },
                { "text": "done" }
            ])
        );
    }

    #[test]
    fn turns_a_base64_image_block_into_an_inline_part() {
        let out = request(json!({
            "messages": [{ "role": "user", "content": [
                { "type": "image", "source": { "type": "base64", "media_type": "image/jpeg", "data": "QUJD" } }
            ] }]
        }));
        assert_eq!(
            out["contents"][0]["parts"],
            json!([{ "inlineData": { "mimeType": "image/jpeg", "data": "QUJD" } }])
        );
    }

    #[test]
    fn maps_tools_tool_choice_and_stop_sequences() {
        let out = request(json!({
            "messages": [{ "role": "user", "content": "x" }],
            "stop_sequences": ["STOP"],
            "tools": [
                { "name": "search", "description": "look up", "input_schema": { "type": "object", "$schema": "http://json-schema.org/draft-07/schema#" } },
                { "type": "web_search_20250305", "name": "web_search" }
            ],
            "tool_choice": { "type": "tool", "name": "search" }
        }));
        assert_eq!(
            out["tools"],
            json!([{ "functionDeclarations": [
                { "name": "search", "description": "look up", "parameters": { "type": "object" } }
            ] }])
        );
        assert_eq!(
            out["toolConfig"],
            json!({ "functionCallingConfig": { "mode": "ANY", "allowedFunctionNames": ["search"] } })
        );
        assert_eq!(out["generationConfig"]["stopSequences"], json!(["STOP"]));
    }

    // ── geminiResponseToAnthropic ──────────────────────────────────────────

    #[test]
    fn builds_thinking_text_and_tool_use_blocks_with_usage() {
        let out = gemini_response_to_anthropic(
            &json!({ "response": {
                "candidates": [{ "content": { "parts": [
                    { "text": "reasoning", "thought": true, "thoughtSignature": "sig" },
                    { "text": "answer" },
                    { "functionCall": { "id": "fc1", "name": "search", "args": { "q": "x" } } }
                ] }, "finishReason": "STOP" }],
                "usageMetadata": { "promptTokenCount": 12, "cachedContentTokenCount": 2, "candidatesTokenCount": 3, "thoughtsTokenCount": 5 }
            } }),
            "antigravity/gemini-3-flash",
            GeminiThinkingMode::Default,
        );
        assert_eq!(
            out["content"],
            json!([
                { "type": "thinking", "thinking": "reasoning", "signature": "sig" },
                { "type": "text", "text": "answer" },
                { "type": "tool_use", "id": "fc1", "name": "search", "input": { "q": "x" } }
            ])
        );
        assert_eq!(out["stop_reason"], "tool_use");
        assert_eq!(
            out["usage"],
            // Gemini's promptTokenCount includes the cached half; Anthropic's does not
            json!({ "input_tokens": 10, "output_tokens": 8, "cache_read_input_tokens": 2 })
        );
    }

    #[test]
    fn drops_thought_parts_when_thinking_is_disabled() {
        let out = gemini_response_to_anthropic(
            &json!({ "response": { "candidates": [{ "content": { "parts": [
                { "text": "hidden", "thought": true },
                { "text": "shown" }
            ] } }] } }),
            "m",
            GeminiThinkingMode::Disabled,
        );
        assert_eq!(out["content"], json!([{ "type": "text", "text": "shown" }]));
    }

    #[test]
    fn keeps_a_signature_on_a_text_less_thought_part() {
        let out = gemini_response_to_anthropic(
            &json!({ "response": { "candidates": [{ "content": { "parts": [
                { "text": "reasoning", "thought": true },
                { "text": "", "thought": true, "thoughtSignature": "sig-7" },
                { "text": "answer" }
            ] }, "finishReason": "STOP" }] } }),
            "m",
            GeminiThinkingMode::Default,
        );
        assert_eq!(
            out["content"],
            json!([
                { "type": "thinking", "thinking": "reasoning", "signature": "sig-7" },
                { "type": "text", "text": "answer" }
            ])
        );
    }

    #[test]
    fn carries_a_function_call_signature_on_the_adjacent_thinking_block() {
        let out = gemini_response_to_anthropic(
            &json!({ "response": { "candidates": [{ "content": { "parts": [
                { "functionCall": { "id": "t1", "name": "search", "args": {} }, "thoughtSignature": "sig-fc" }
            ] }, "finishReason": "STOP" }] } }),
            "m",
            GeminiThinkingMode::Default,
        );
        // Anthropic tool_use has no signature field, so the signature rides on a
        // signature-only thinking block right before it.
        assert_eq!(
            out["content"],
            json!([
                { "type": "thinking", "thinking": "", "signature": "sig-fc" },
                { "type": "tool_use", "id": "t1", "name": "search", "input": {} }
            ])
        );
    }

    #[test]
    fn carries_a_text_part_signature_on_the_adjacent_thinking_block() {
        let out = gemini_response_to_anthropic(
            &json!({ "response": { "candidates": [{ "content": { "parts": [
                { "text": "reasoning", "thought": true },
                { "text": "answer", "thoughtSignature": "sig-t" }
            ] }, "finishReason": "STOP" }] } }),
            "m",
            GeminiThinkingMode::Default,
        );
        assert_eq!(
            out["content"],
            json!([
                { "type": "thinking", "thinking": "reasoning", "signature": "sig-t" },
                { "type": "text", "text": "answer" }
            ])
        );
    }

    #[test]
    fn opens_a_signature_only_thinking_block_for_a_signed_text_part() {
        let out = gemini_response_to_anthropic(
            &json!({ "response": { "candidates": [{
                "content": { "parts": [{ "text": "answer", "thoughtSignature": "sig-t" }] },
                "finishReason": "STOP"
            }] } }),
            "m",
            GeminiThinkingMode::Default,
        );
        assert_eq!(
            out["content"],
            json!([
                { "type": "thinking", "thinking": "", "signature": "sig-t" },
                { "type": "text", "text": "answer" }
            ])
        );
    }

    #[test]
    fn drops_a_text_part_signature_when_thinking_is_disabled() {
        let out = gemini_response_to_anthropic(
            &json!({ "response": { "candidates": [{
                "content": { "parts": [{ "text": "answer", "thoughtSignature": "sig-t" }] },
                "finishReason": "STOP"
            }] } }),
            "m",
            GeminiThinkingMode::Disabled,
        );
        assert_eq!(out["content"], json!([{ "type": "text", "text": "answer" }]));
    }

    #[test]
    fn maps_a_safety_finish_to_a_refusal() {
        let out = gemini_response_to_anthropic(
            &json!({ "response": { "candidates": [{ "content": { "parts": [] }, "finishReason": "SAFETY" }] } }),
            "m",
            GeminiThinkingMode::Default,
        );
        assert_eq!(out["stop_reason"], "refusal");
    }

    #[test]
    fn maps_a_candidate_less_safety_block_to_a_refusal() {
        let out = gemini_response_to_anthropic(
            &json!({ "response": { "promptFeedback": { "blockReason": "SAFETY" } } }),
            "m",
            GeminiThinkingMode::Default,
        );
        assert_eq!(out["stop_reason"], "refusal");
        assert_eq!(out["content"], json!([]));
    }

    #[test]
    fn maps_max_tokens_to_the_anthropic_stop_reason() {
        let out = gemini_response_to_anthropic(
            &json!({ "response": { "candidates": [{ "content": { "parts": [{ "text": "…" }] }, "finishReason": "MAX_TOKENS" }] } }),
            "m",
            GeminiThinkingMode::Default,
        );
        assert_eq!(out["stop_reason"], "max_tokens");
    }

    // ── geminiSseToAnthropicStream ─────────────────────────────────────────

    #[tokio::test]
    async fn emits_a_well_formed_messages_stream_for_thinking_then_text() {
        let raw = read_sse(gemini_sse_to_anthropic_stream(
            sse(&[
                json!({ "response": {
                    "candidates": [{ "content": { "parts": [{ "text": "think", "thought": true, "thoughtSignature": "sig" }] } }],
                    "usageMetadata": { "promptTokenCount": 9 }
                } }),
                json!({ "response": { "candidates": [{ "content": { "parts": [{ "text": "Hel" }] } }] } }),
                json!({ "response": { "candidates": [{ "content": { "parts": [{ "text": "lo" }] } }] } }),
                json!({ "response": {
                    "candidates": [{ "finishReason": "STOP" }],
                    "usageMetadata": { "promptTokenCount": 9, "candidatesTokenCount": 2 }
                } }),
            ]),
            "antigravity/gemini-3-flash",
            GeminiThinkingMode::Default,
        ))
        .await;
        let seq = events(&raw);
        assert_eq!(
            names(&seq),
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_delta", // signature_delta closing the thinking block
                "content_block_stop",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(seq[3].1["delta"], json!({ "type": "signature_delta", "signature": "sig" }));
        assert_eq!(seq[5].1["content_block"], json!({ "type": "text", "text": "" }));
        let message_delta = &seq[seq.len() - 2].1;
        assert_eq!(message_delta["delta"]["stop_reason"], "end_turn");
        assert_eq!(message_delta["usage"]["input_tokens"], 9);
        assert_eq!(message_delta["usage"]["output_tokens"], 2);
    }

    #[tokio::test]
    async fn holds_message_start_until_a_frame_reports_the_prompt_token_count() {
        let raw = read_sse(gemini_sse_to_anthropic_stream(
            sse(&[
                json!({ "response": {
                    "candidates": [{ "content": { "parts": [{ "text": "Hi" }] } }],
                    "usageMetadata": { "promptTokenCount": 11, "cachedContentTokenCount": 4 }
                } }),
                json!({ "response": {
                    "candidates": [{ "finishReason": "STOP" }],
                    "usageMetadata": { "promptTokenCount": 11, "cachedContentTokenCount": 4, "candidatesTokenCount": 6 }
                } }),
            ]),
            "m",
            GeminiThinkingMode::Default,
        ))
        .await;
        let seq = events(&raw);
        // input_tokens excludes the cached half; output is not known yet.
        assert_eq!(
            seq[0].1["message"]["usage"],
            json!({ "input_tokens": 7, "output_tokens": 0, "cache_read_input_tokens": 4 })
        );
        let usage = &seq[seq.len() - 2].1["usage"];
        assert_eq!(usage["input_tokens"], 7);
        assert_eq!(usage["cache_read_input_tokens"], 4);
        assert_eq!(usage["output_tokens"], 6);
    }

    #[tokio::test]
    async fn fails_the_turn_when_no_frame_ever_reported_a_count() {
        let raw = read_sse(gemini_sse_to_anthropic_stream(
            sse(&[json!({ "response": { "candidates": [{ "content": { "parts": [{ "text": "Hi" }] } }] } })]),
            "m",
            GeminiThinkingMode::Default,
        ))
        .await;
        let seq = events(&raw);
        assert_eq!(names(&seq), vec!["error"]);
        assert_eq!(seq[0].1["error"]["type"], "api_error");
    }

    #[tokio::test]
    async fn does_not_double_count_cache_reads() {
        let raw = read_sse(gemini_sse_to_anthropic_stream(
            sse(&[
                json!({ "response": {
                    "candidates": [{ "content": { "parts": [{ "text": "Hi" }] } }],
                    "usageMetadata": { "promptTokenCount": 10, "cachedContentTokenCount": 4 }
                } }),
                json!({ "response": {
                    "candidates": [{ "finishReason": "STOP" }],
                    "usageMetadata": { "promptTokenCount": 10, "candidatesTokenCount": 3 }
                } }),
            ]),
            "m",
            GeminiThinkingMode::Default,
        ))
        .await;
        // The second frame reports the input side without a cache number, so its
        // pair wins whole: 10 uncached, not 10 + a stale 4.
        let seq = events(&raw);
        let usage = &seq[seq.len() - 2].1["usage"];
        assert_eq!(usage["input_tokens"], 10);
        assert_eq!(usage["cache_read_input_tokens"], 0);
        assert_eq!(usage["output_tokens"], 3);
    }

    #[tokio::test]
    async fn keeps_an_output_count_a_later_count_less_frame_would_erase() {
        let raw = read_sse(gemini_sse_to_anthropic_stream(
            sse(&[
                json!({ "response": {
                    "candidates": [{ "content": { "parts": [{ "text": "Hi" }] } }],
                    "usageMetadata": { "promptTokenCount": 5, "candidatesTokenCount": 12 }
                } }),
                json!({ "response": {
                    "candidates": [{ "finishReason": "STOP" }],
                    "usageMetadata": { "promptTokenCount": 5, "totalTokenCount": 17 }
                } }),
            ]),
            "m",
            GeminiThinkingMode::Default,
        ))
        .await;
        let seq = events(&raw);
        let usage = &seq[seq.len() - 2].1["usage"];
        assert_eq!(usage["input_tokens"], 5);
        assert_eq!(usage["output_tokens"], 12);
    }

    #[tokio::test]
    async fn streams_a_tool_call_as_a_complete_input_json_delta() {
        let raw = read_sse(gemini_sse_to_anthropic_stream(
            sse(&[
                json!({ "response": {
                    "candidates": [{ "content": { "parts": [{ "functionCall": { "id": "t1", "name": "search", "args": { "q": "x" } } }] } }],
                    "usageMetadata": { "promptTokenCount": 4 }
                } }),
                json!({ "response": { "candidates": [{ "finishReason": "STOP" }] } }),
            ]),
            "m",
            GeminiThinkingMode::Default,
        ))
        .await;
        let seq = events(&raw);
        let start = seq.iter().find(|(e, _)| e == "content_block_start").unwrap();
        assert_eq!(start.1["content_block"]["type"], "tool_use");
        assert_eq!(start.1["content_block"]["id"], "t1");
        assert_eq!(start.1["content_block"]["name"], "search");
        assert_eq!(start.1["content_block"]["input"], json!({}));
        let delta = seq.iter().find(|(e, _)| e == "content_block_delta").unwrap();
        assert_eq!(
            delta.1["delta"],
            json!({ "type": "input_json_delta", "partial_json": "{\"q\":\"x\"}" })
        );
        assert_eq!(seq[seq.len() - 2].1["delta"]["stop_reason"], "tool_use");
    }

    #[tokio::test]
    async fn emits_the_signature_delta_for_a_text_less_thought_part() {
        let raw = read_sse(gemini_sse_to_anthropic_stream(
            sse(&[
                json!({ "response": {
                    "candidates": [{ "content": { "parts": [{ "text": "think", "thought": true }] } }],
                    "usageMetadata": { "promptTokenCount": 4 }
                } }),
                json!({ "response": { "candidates": [{ "content": { "parts": [{ "text": "", "thought": true, "thoughtSignature": "sig-3" }] } }] } }),
                json!({ "response": { "candidates": [{ "finishReason": "STOP" }] } }),
            ]),
            "m",
            GeminiThinkingMode::Default,
        ))
        .await;
        let seq = events(&raw);
        let signature = seq
            .iter()
            .find(|(e, d)| e == "content_block_delta" && d["delta"]["type"] == "signature_delta")
            .unwrap();
        assert_eq!(signature.1["delta"]["signature"], "sig-3");
    }

    #[tokio::test]
    async fn closes_the_thinking_block_with_a_text_part_signature_before_the_text_opens() {
        let raw = read_sse(gemini_sse_to_anthropic_stream(
            sse(&[
                json!({ "response": {
                    "candidates": [{ "content": { "parts": [{ "text": "reasoning", "thought": true }] } }],
                    "usageMetadata": { "promptTokenCount": 4 }
                } }),
                json!({ "response": { "candidates": [{ "content": { "parts": [{ "text": "answer", "thoughtSignature": "sig-t" }] } }] } }),
                json!({ "response": { "candidates": [{ "finishReason": "STOP" }] } }),
            ]),
            "m",
            GeminiThinkingMode::Default,
        ))
        .await;
        let seq = events(&raw);
        let signature_at = seq
            .iter()
            .position(|(e, d)| e == "content_block_delta" && d["delta"]["type"] == "signature_delta")
            .expect("a signature_delta is emitted");
        assert_eq!(seq[signature_at].1["delta"]["signature"], "sig-t");
        let text_start_at = seq
            .iter()
            .position(|(e, d)| e == "content_block_start" && d["content_block"]["type"] == "text")
            .expect("a text block opens");
        assert!(signature_at < text_start_at);
    }

    #[tokio::test]
    async fn finishes_a_candidate_less_safety_block_as_a_refusal() {
        let raw = read_sse(gemini_sse_to_anthropic_stream(
            sse(&[json!({ "response": {
                "promptFeedback": { "blockReason": "SAFETY" },
                "usageMetadata": { "promptTokenCount": 4 }
            } })]),
            "m",
            GeminiThinkingMode::Default,
        ))
        .await;
        let seq = events(&raw);
        assert_eq!(seq.last().unwrap().0, "message_stop");
        let delta = seq.iter().find(|(e, _)| e == "message_delta").unwrap();
        assert_eq!(delta.1["delta"]["stop_reason"], "refusal");
    }

    #[tokio::test]
    async fn ends_an_unterminated_stream_with_an_error_event() {
        let raw = read_sse(gemini_sse_to_anthropic_stream(
            sse(&[]),
            "m",
            GeminiThinkingMode::Default,
        ))
        .await;
        let seq = events(&raw);
        assert_eq!(names(&seq), vec!["error"]);
        assert_eq!(seq[0].1["error"]["type"], "api_error");
    }

    #[tokio::test]
    async fn ends_the_turn_on_a_mid_stream_upstream_error() {
        let body: ByteStream = Box::pin(futures::stream::iter(vec![
            Ok(Bytes::from_static(
                b"data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"a\"}]}}],\"usageMetadata\":{\"promptTokenCount\":3}}}\n\n",
            )),
            Err(std::io::Error::other("connection reset")),
        ]));
        let raw =
            read_sse(gemini_sse_to_anthropic_stream(body, "m", GeminiThinkingMode::Default)).await;
        let seq = events(&raw);
        assert_eq!(seq.last().unwrap().0, "error");
        assert_eq!(seq.last().unwrap().1["error"]["message"], "connection reset");
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
                let frame = json!({ "response": {
                    "candidates": [{ "content": { "parts": [{ "text": format!("chunk-{n}") }] } }],
                    "usageMetadata": { "promptTokenCount": 3 }
                } });
                let text = format!("data: {}\n\n", serde_json::to_string(&frame).unwrap());
                Some((Ok(Bytes::from(text)), ()))
            }
        }));
        let mut converted =
            gemini_sse_to_anthropic_stream(body, "m", GeminiThinkingMode::Default);
        let _ = converted.next().await; // message_start
        let _ = converted.next().await; // first converted event
        tokio::task::yield_now().await;
        assert!(served.load(Ordering::SeqCst) < 6);
    }
}
