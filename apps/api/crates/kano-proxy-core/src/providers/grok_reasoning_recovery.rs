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

/// Remove opaque replay state from a Responses request body, or `None` when it carries none.
/// - reasoning: drop `encrypted_content`; drop the item entirely if nothing readable remains
/// - compaction: drop the whole item (ciphertext is the only payload)
pub fn strip_responses_opaque_state(body: &Value) -> Option<Value> {
    let input = body.get("input").and_then(Value::as_array)?;

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
        return None;
    }
    let mut out = body.as_object().cloned().unwrap_or_default();
    out.insert("input".to_string(), Value::Array(next));
    Some(Value::Object(out))
}

pub fn affinity_present(a: &AffinityIds) -> bool {
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
        let body = strip_responses_opaque_state(&json!({
            "model": "grok-4.5",
            "input": [
                { "type": "reasoning", "summary": [], "content": null, "encrypted_content": "abc" },
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hi" }] }
            ]
        }))
        .expect("changed");
        assert_eq!(body["model"], "grok-4.5");
        assert_eq!(
            body["input"],
            json!([{ "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hi" }] }])
        );
    }

    #[test]
    fn leaves_a_body_without_opaque_state_alone() {
        let plain = json!({ "input": [{ "type": "reasoning", "summary": [{ "type": "summary_text", "text": "plan" }] }] });
        assert_eq!(strip_responses_opaque_state(&plain), None);
        assert_eq!(strip_responses_opaque_state(&json!({ "input": [] })), None);
        assert_eq!(strip_responses_opaque_state(&json!({ "model": "m" })), None);
    }

    #[test]
    fn keeps_reasoning_items_that_still_have_readable_summary_text() {
        let body = strip_responses_opaque_state(&json!({
            "input": [
                { "type": "reasoning", "summary": [{ "type": "summary_text", "text": "plan" }], "encrypted_content": "abc" }
            ]
        }))
        .expect("changed");
        assert_eq!(
            body["input"],
            json!([{ "type": "reasoning", "summary": [{ "type": "summary_text", "text": "plan" }] }])
        );
    }

    #[test]
    fn drops_compaction_items_entirely() {
        let body = strip_responses_opaque_state(&json!({
            "input": [
                { "type": "compaction", "encrypted_content": "opaque" },
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "after" }] }
            ]
        }))
        .expect("changed");
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "message");
    }

    #[test]
    fn detects_any_sticky_affinity_header() {
        let conv = AffinityIds { conv_id: Some("c".into()), ..Default::default() };
        let session = AffinityIds { session_id: Some("s".into()), ..Default::default() };
        let empty = AffinityIds::default();
        let blank = AffinityIds { conv_id: Some("  ".into()), ..Default::default() };
        assert!(affinity_present(&conv));
        assert!(affinity_present(&session));
        assert!(!affinity_present(&empty));
        assert!(!affinity_present(&blank));
    }
}
