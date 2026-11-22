//! Codex token counting (docs/api.md § count_tokens).
//!
//! The Anthropic body is serialized to text and counted in-process with `tiktoken_rs`'s
//! `o200k_base`. Any failure returns `None`, which the route answers as the sentinel
//! `{"input_tokens": 0}`.
//! A failed count must never surface as an error status, because that sends Claude Code into
//! a parallel `max_tokens: 1` probe burst against the real upstream (measured 2026-08-22).

use serde_json::Value;

use crate::proxy::anthropic_client_tools;

/// A `content` (or `system`) value: a non-empty string, or an array of blocks.
fn push_content(out: &mut Vec<String>, content: Option<&Value>) {
    match content {
        Some(Value::String(text)) if !text.is_empty() => out.push(text.clone()),
        Some(Value::Array(blocks)) => blocks.iter().for_each(|block| push_block_text(out, block)),
        _ => {}
    }
}

fn push_block_text(out: &mut Vec<String>, block: &Value) {
    if let Some(text) = block.as_str() {
        if !text.is_empty() {
            out.push(text.to_string());
        }
        return;
    }
    let Some(b) = block.as_object() else { return };
    for key in ["text", "thinking"] {
        if let Some(value) = b.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()) {
            out.push(value.to_string());
        }
    }
    let block_type = b.get("type").and_then(Value::as_str);
    if block_type == Some("tool_use") {
        if let Some(name) = b.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            out.push(name.to_string());
        }
        if let Some(input) = b.get("input") {
            out.push(input.to_string());
        }
    }
    if block_type == Some("tool_result") {
        push_content(out, b.get("content"));
    }
}

/// The text a `count_tokens` body would put in front of the model: system, message content
/// (text / thinking / tool_use / tool_result) and tool declarations. Images and redacted
/// thinking are skipped — no honest text length exists for either. This is deliberately an
/// approximation of the upstream's own prompt framing.
fn anthropic_count_texts(body: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    push_content(&mut out, body.get("system"));
    for message in body.get("messages").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]) {
        push_content(&mut out, message.get("content"));
    }
    for tool in body.get("tools").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]) {
        let Some(tool) = tool.as_object() else { continue };
        // The bash and text editor tools count with the parameters the conversion sends.
        let tool = anthropic_client_tools::with_schema(tool);
        if let Some(name) = tool.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            out.push(name.to_string());
        }
        if let Some(description) = tool.get("description").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            out.push(description.to_string());
        }
        if let Some(schema) = tool.get("input_schema") {
            out.push(schema.to_string());
        }
    }
    out
}

/// `o200k_base` (the gpt-4o/o1/gpt-5 family encoding) over already-serialized segments.
/// `None` when the tokenizer is unavailable, so every caller degrades rather than errors.
fn count_texts_o200k(texts: &[String]) -> Option<u64> {
    /// Loaded once per process: the ranks are a multi-MB parse, and a codex stream must
    /// never pay for them, so nothing but the first count touches this.
    static O200K: once_cell::sync::Lazy<Option<tiktoken_rs::CoreBPE>> =
        once_cell::sync::Lazy::new(|| tiktoken_rs::o200k_base().ok());
    let bpe = O200K.as_ref()?;
    let mut total = 0u64;
    for text in texts {
        total += bpe.encode_ordinary(text).len() as u64;
    }
    Some(total)
}

/// The local `count_tokens` answer for an Anthropic request body, or `None` when no count
/// could be obtained (the route then answers the sentinel `{"input_tokens": 0}`).
pub fn count_anthropic_tokens(body: &Value) -> Option<u64> {
    count_texts_o200k(&anthropic_count_texts(body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_system_blocks_message_block_types_and_tool_declarations() {
        let texts = anthropic_count_texts(&json!({
            "system": [{ "type": "text", "text": "sys" }],
            "messages": [
                { "role": "user", "content": "plain" },
                { "role": "assistant", "content": [
                    { "type": "thinking", "thinking": "hmm", "signature": "sig" },
                    { "type": "text", "text": "answer" },
                    { "type": "tool_use", "id": "t1", "name": "run", "input": { "cmd": "ls" } },
                ] },
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "t1", "content": [{ "type": "text", "text": "ok" }] },
                ] },
            ],
            "tools": [{ "name": "run", "description": "runs", "input_schema": { "type": "object" } }],
        }));
        assert_eq!(
            texts,
            vec![
                "sys",
                "plain",
                "hmm",
                "answer",
                "run",
                r#"{"cmd":"ls"}"#,
                "ok",
                "run",
                "runs",
                r#"{"type":"object"}"#,
            ]
        );
    }

    #[test]
    fn counts_the_bash_tool_with_the_parameters_the_conversion_sends() {
        let texts = anthropic_count_texts(&json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "tools": [{ "type": "bash_20250124", "name": "bash" }],
        }));
        assert_eq!(texts.len(), 4, "{texts:?}");
        assert_eq!(texts[1], "bash");
        assert!(texts[3].contains(r#""restart""#), "{texts:?}");
    }

    #[test]
    fn skips_images_and_redacted_thinking_rather_than_inventing_text() {
        let texts = anthropic_count_texts(&json!({
            "messages": [{ "role": "user", "content": [
                { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAAA" } },
                { "type": "redacted_thinking", "data": "opaque" },
                { "type": "text", "text": "visible" },
            ] }],
        }));
        assert_eq!(texts, vec!["visible"]);
    }

    #[test]
    fn a_plain_string_system_and_an_empty_body_are_handled() {
        assert_eq!(anthropic_count_texts(&json!({ "system": "be terse" })), vec!["be terse"]);
        assert!(anthropic_count_texts(&json!({ "system": "" })).is_empty());
        assert!(anthropic_count_texts(&json!({})).is_empty());
    }

    #[test]
    fn counts_in_process_with_o200k_base() {
        // Pinned against the public tokenizer: "hello world" is two o200k_base tokens.
        assert_eq!(count_texts_o200k(&["hello world".to_string()]), Some(2));
        // Segments are summed, and an empty list counts zero rather than failing.
        assert_eq!(
            count_texts_o200k(&["hello world".to_string(), "hello world".to_string()]),
            Some(4)
        );
        assert_eq!(count_texts_o200k(&[]), Some(0));
    }

    #[test]
    fn a_body_with_no_countable_text_counts_zero_not_an_error() {
        assert_eq!(count_anthropic_tokens(&json!({ "messages": [] })), Some(0));
    }
}
