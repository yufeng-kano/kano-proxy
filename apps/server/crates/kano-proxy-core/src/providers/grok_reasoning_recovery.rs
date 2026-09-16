//! Grok reasoning recovery (docs/providers.md § Grok).
//!
//! Recover from xAI/cli-chat-proxy opaque-state decode rejections.
//!
//! Claude Code → Messages → Responses can surface either:
//!   - "Could not decode the compaction blob..."
//!   - "Could not decrypt the provided encrypted_content"
//!
//! when a prior `thinking.signature` / replay-cache entry / session sticky state is
//! foreign, truncated, or bound to a different upstream session. Shape checks cannot
//! prove decryptability — retry after stripping opaque items (and, if needed,
//! clearing affinity headers). Inspired by grok2api PR #721.

use serde_json::{Map, Value};

use crate::providers::types::AffinityIds;

const DECODE_FAILURE_MARKERS: [&str; 2] =
    ["could not decode the compaction blob", "could not decrypt the provided encrypted_content"];

pub fn is_grok_opaque_decode_failure(body_text: &str) -> bool {
    let lower = body_text.to_lowercase();
    DECODE_FAILURE_MARKERS.iter().any(|m| lower.contains(m))
}

#[derive(Debug, Clone, PartialEq)]
pub struct StripOpaqueResult {
    pub body: Value,
    /// True when at least one `reasoning.encrypted_content` or compaction item was removed.
    pub changed: bool,
}

/// Remove opaque replay state from a Responses request body.
/// - reasoning: drop `encrypted_content`; drop the item entirely if nothing readable remains
/// - compaction: drop the whole item (ciphertext is the only payload)
pub fn strip_responses_opaque_state(body: &Value) -> StripOpaqueResult {
    let input = match body.get("input").and_then(Value::as_array) {
        Some(input) if !input.is_empty() => input,
        _ => return StripOpaqueResult { body: body.clone(), changed: false },
    };

    let mut changed = false;
    let mut next: Vec<Value> = Vec::with_capacity(input.len());
    for raw in input {
        let item = match raw.as_object() {
            Some(o) => o,
            None => {
                next.push(raw.clone());
                continue;
            }
        };
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");

        if item_type == "compaction" {
            changed = true;
            continue;
        }
        if item_type != "reasoning" {
            next.push(raw.clone());
            continue;
        }

        let has_encrypted = item.get("encrypted_content").and_then(Value::as_str).is_some_and(|s| !s.is_empty());
        if !has_encrypted {
            next.push(raw.clone());
            continue;
        }

        changed = true;
        let mut cleaned = item.clone();
        cleaned.remove("encrypted_content");
        cleaned.remove("id");
        cleaned.remove("status");
        if has_readable_reasoning_content(&cleaned) {
            next.push(Value::Object(cleaned));
        }
    }

    if !changed {
        return StripOpaqueResult { body: body.clone(), changed: false };
    }
    let mut out = body.as_object().cloned().unwrap_or_default();
    out.insert("input".to_string(), Value::Array(next));
    StripOpaqueResult { body: Value::Object(out), changed: true }
}

/// Drop `prompt_cache_key` so a session-reset retry cannot reattach sticky cache.
pub fn strip_prompt_cache_key(body: &Value) -> Value {
    match body.as_object() {
        Some(obj) if obj.contains_key("prompt_cache_key") => {
            let mut next = obj.clone();
            next.remove("prompt_cache_key");
            Value::Object(next)
        }
        _ => body.clone(),
    }
}

pub fn affinity_present(affinity: Option<&AffinityIds>) -> bool {
    let a = match affinity {
        Some(a) => a,
        None => return false,
    };
    [&a.conv_id, &a.session_id, &a.turn_idx]
        .into_iter()
        .any(|v| v.as_deref().is_some_and(|s| !s.trim().is_empty()))
}

fn has_readable_reasoning_content(item: &Map<String, Value>) -> bool {
    for field in ["summary", "content"] {
        let parts = match item.get(field).and_then(Value::as_array) {
            Some(p) => p,
            None => continue,
        };
        for part in parts {
            if part.get("text").and_then(Value::as_str).is_some_and(|t| !t.trim().is_empty()) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn matches_compaction_and_encrypted_content_decode_errors() {
        assert!(is_grok_opaque_decode_failure(
            &json!({
                "code": "invalid-argument",
                "error": "Could not decode the compaction blob. Ensure it is unmodified from the compact response."
            })
            .to_string()
        ));
        assert!(is_grok_opaque_decode_failure("Could not decrypt the provided encrypted_content"));
        assert!(!is_grok_opaque_decode_failure(r#"{"error":"model not found"}"#));
    }

    #[test]
    fn removes_encrypted_content_and_drops_empty_reasoning_items() {
        let result = strip_responses_opaque_state(&json!({
            "model": "grok-4.5",
            "input": [
                { "type": "reasoning", "summary": [], "content": null, "encrypted_content": "abc" },
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hi" }] }
            ]
        }));
        assert!(result.changed);
        assert_eq!(
            result.body["input"],
            json!([{ "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hi" }] }])
        );
    }

    #[test]
    fn keeps_reasoning_items_that_still_have_readable_summary_text() {
        let result = strip_responses_opaque_state(&json!({
            "input": [
                { "type": "reasoning", "summary": [{ "type": "summary_text", "text": "plan" }], "encrypted_content": "abc" }
            ]
        }));
        assert!(result.changed);
        assert_eq!(
            result.body["input"],
            json!([{ "type": "reasoning", "summary": [{ "type": "summary_text", "text": "plan" }] }])
        );
    }

    #[test]
    fn drops_compaction_items_entirely() {
        let result = strip_responses_opaque_state(&json!({
            "input": [
                { "type": "compaction", "encrypted_content": "opaque" },
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "after" }] }
            ]
        }));
        assert!(result.changed);
        let input = result.body["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "message");
    }

    #[test]
    fn removes_prompt_cache_key_when_present() {
        assert_eq!(strip_prompt_cache_key(&json!({ "prompt_cache_key": "s", "model": "m" })), json!({ "model": "m" }));
        assert_eq!(strip_prompt_cache_key(&json!({ "model": "m" })), json!({ "model": "m" }));
    }

    #[test]
    fn detects_any_sticky_affinity_header() {
        let conv = AffinityIds { conv_id: Some("c".into()), ..Default::default() };
        let session = AffinityIds { session_id: Some("s".into()), ..Default::default() };
        let empty = AffinityIds::default();
        assert!(affinity_present(Some(&conv)));
        assert!(affinity_present(Some(&session)));
        assert!(!affinity_present(Some(&empty)));
        assert!(!affinity_present(None));
    }
}
