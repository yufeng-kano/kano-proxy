//! Port of apps/api/src/providers/codex_reasoning_cache.ts (docs/providers.md § Codex).
//!
//! Session-scoped cache for Codex Responses reasoning replay: a bounded history of
//! completed assistant turns, each identified by the hashes of the normalized visible
//! Responses input prefix before and after it. The Worker's KV namespace becomes
//! [`crate::cache::Cache`] (`AppState::cache()`), with the same `v2` key shape, the same 1h
//! TTL and the same 256 KiB serialized cap.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::cache::Cache;
use crate::providers::types::AffinityIds;

pub const CODEX_REASONING_REPLAY_TTL_SECONDS: u64 = 3600;
const CODEX_REASONING_REPLAY_MAX_BYTES: usize = 256 * 1024;

/// One opaque Responses output item (`reasoning` / `function_call` / `custom_tool_call`).
/// Kept as a JSON object so upstream fields ride through untouched.
pub type CodexReasoningReplayItem = Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodexReasoningReplayTurn {
    /// Hashes of the visible input before and after this assistant turn.
    pub start_hash: String,
    pub end_hash: String,
    pub visible_count: usize,
    pub items: Vec<CodexReasoningReplayItem>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CodexReasoningReplayEntry {
    pub turns: Vec<CodexReasoningReplayTurn>,
}

/// Client affinity ids first, then the `prompt_cache_key` (client-sent on `/openai/v1`,
/// `metadata.user_id`-derived on `/anthropic`). Never invented.
pub fn codex_reasoning_replay_session_key(
    affinity: Option<&AffinityIds>,
    prompt_cache_key: Option<&str>,
) -> Option<String> {
    let trimmed = |v: Option<&String>| v.map(|s| s.trim()).filter(|s| !s.is_empty()).map(str::to_string);
    if let Some(affinity) = affinity {
        if let Some(conv) = trimmed(affinity.conv_id.as_ref()) {
            return Some(conv);
        }
        if let Some(session) = trimmed(affinity.session_id.as_ref()) {
            return Some(session);
        }
    }
    prompt_cache_key.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

pub fn hash_assistant_text(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

fn scoped_cache_key(api_key_id: &str, model: &str, session_key: &str) -> String {
    let material = format!("{api_key_id}\0{model}\0{session_key}");
    format!("codex-reasoning-replay:v2:{}", hex::encode(Sha256::digest(material.as_bytes())))
}

fn is_replay_item(value: &Value) -> bool {
    value.as_object().map(|o| o.get("type").and_then(Value::as_str).is_some()).unwrap_or(false)
}

/// The TypeScript `isReplayEntry` guard, applied on both read and write.
fn is_replay_entry(entry: &CodexReasoningReplayEntry) -> bool {
    entry.turns.iter().all(|turn| {
        !turn.start_hash.is_empty()
            && !turn.end_hash.is_empty()
            && turn.visible_count > 0
            && turn.items.iter().all(is_replay_item)
    })
}

pub async fn read_codex_reasoning_replay(
    cache: &Cache,
    api_key_id: &str,
    model: &str,
    session_key: Option<&str>,
) -> Option<CodexReasoningReplayEntry> {
    let session_key = session_key.filter(|s| !s.is_empty())?;
    if api_key_id.is_empty() || model.is_empty() {
        return None;
    }
    let raw = cache.get(&scoped_cache_key(api_key_id, model, session_key)).await?;
    let entry: CodexReasoningReplayEntry = serde_json::from_slice(&raw).ok()?;
    if !is_replay_entry(&entry) {
        return None;
    }
    Some(entry)
}

pub async fn write_codex_reasoning_replay(
    cache: &Cache,
    api_key_id: &str,
    model: &str,
    session_key: Option<&str>,
    entry: &CodexReasoningReplayEntry,
) {
    let Some(session_key) = session_key.filter(|s| !s.is_empty()) else { return };
    if api_key_id.is_empty() || model.is_empty() || !is_replay_entry(entry) {
        return;
    }
    let Ok(serialized) = serde_json::to_vec(entry) else { return };
    if serialized.len() > CODEX_REASONING_REPLAY_MAX_BYTES {
        return;
    }
    cache
        .put(
            &scoped_cache_key(api_key_id, model, session_key),
            bytes::Bytes::from(serialized),
            Duration::from_secs(CODEX_REASONING_REPLAY_TTL_SECONDS),
        )
        .await;
}

pub async fn delete_codex_reasoning_replay(
    cache: &Cache,
    api_key_id: &str,
    model: &str,
    session_key: Option<&str>,
) {
    let Some(session_key) = session_key.filter(|s| !s.is_empty()) else { return };
    if api_key_id.is_empty() || model.is_empty() {
        return;
    }
    cache.delete(&scoped_cache_key(api_key_id, model, session_key)).await;
}

/// Exported for tests — same material as the private scoped key.
pub fn codex_reasoning_replay_cache_key_for_test(api_key_id: &str, model: &str, session_key: &str) -> String {
    scoped_cache_key(api_key_id, model, session_key)
}

/// Preserve older replayed prefixes when the history reaches its byte budget: a turn that
/// would push the entry past the cap is dropped, never an earlier prefix.
pub fn append_codex_replay_turn(
    history: &CodexReasoningReplayEntry,
    turn: Option<CodexReasoningReplayTurn>,
) -> CodexReasoningReplayEntry {
    let Some(turn) = turn else { return history.clone() };
    if history
        .turns
        .iter()
        .any(|old| old.start_hash == turn.start_hash && old.end_hash == turn.end_hash)
    {
        return history.clone();
    }
    let mut next = history.clone();
    next.turns.push(turn);
    match serde_json::to_vec(&next) {
        Ok(bytes) if bytes.len() <= CODEX_REASONING_REPLAY_MAX_BYTES => next,
        _ => history.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn affinity(conv: Option<&str>, session: Option<&str>) -> AffinityIds {
        AffinityIds {
            conv_id: conv.map(str::to_string),
            session_id: session.map(str::to_string),
            turn_idx: None,
        }
    }

    fn reasoning() -> Value {
        json!({ "type": "reasoning", "encrypted_content": "opaque-codex-content", "summary": [] })
    }
    fn function_call() -> Value {
        json!({ "type": "function_call", "call_id": "call_1", "name": "Read", "arguments": "{\"path\":\"/tmp/a\"}" })
    }
    fn custom_tool_call() -> Value {
        json!({ "type": "custom_tool_call", "call_id": "call_2", "name": "custom", "input": "value" })
    }

    fn turn(items: Vec<Value>, start: &str, end: &str, visible: usize) -> CodexReasoningReplayTurn {
        CodexReasoningReplayTurn {
            start_hash: start.into(),
            end_hash: end.into(),
            visible_count: visible,
            items,
        }
    }

    #[test]
    fn prefers_conv_id_over_session_id() {
        let a = affinity(Some("conv"), Some("sess"));
        assert_eq!(codex_reasoning_replay_session_key(Some(&a), None).as_deref(), Some("conv"));
    }

    #[test]
    fn falls_back_to_session_id_and_returns_none_without_either() {
        let a = affinity(None, Some("sess"));
        assert_eq!(codex_reasoning_replay_session_key(Some(&a), None).as_deref(), Some("sess"));
        assert_eq!(codex_reasoning_replay_session_key(None, None), None);
        assert_eq!(codex_reasoning_replay_session_key(Some(&affinity(None, None)), None), None);
    }

    #[test]
    fn falls_back_to_the_prompt_cache_key_after_both_affinity_ids() {
        assert_eq!(
            codex_reasoning_replay_session_key(Some(&affinity(None, Some("sess"))), Some("pck")).as_deref(),
            Some("sess")
        );
        assert_eq!(
            codex_reasoning_replay_session_key(Some(&affinity(None, None)), Some(" pck ")).as_deref(),
            Some("pck")
        );
        assert_eq!(codex_reasoning_replay_session_key(None, Some("pck")).as_deref(), Some("pck"));
        assert_eq!(codex_reasoning_replay_session_key(None, Some("   ")), None);
    }

    #[tokio::test]
    async fn round_trips_the_ordered_item_array_and_fingerprint() {
        let cache = Cache::new();
        let entry = CodexReasoningReplayEntry {
            turns: vec![turn(
                vec![reasoning(), function_call(), custom_tool_call()],
                "before",
                &hash_assistant_text("answer"),
                2,
            )],
        };
        write_codex_reasoning_replay(&cache, "key1", "gpt-5.2", Some("sessA"), &entry).await;
        assert_eq!(read_codex_reasoning_replay(&cache, "key1", "gpt-5.2", Some("sessA")).await, Some(entry));
    }

    #[tokio::test]
    async fn isolates_by_api_key_model_and_session() {
        let cache = Cache::new();
        let entry = CodexReasoningReplayEntry { turns: vec![turn(vec![reasoning()], "before", "after", 1)] };
        write_codex_reasoning_replay(&cache, "key1", "gpt-5.2", Some("sessA"), &entry).await;
        assert_eq!(read_codex_reasoning_replay(&cache, "key2", "gpt-5.2", Some("sessA")).await, None);
        assert_eq!(read_codex_reasoning_replay(&cache, "key1", "gpt-5.1", Some("sessA")).await, None);
        assert_eq!(read_codex_reasoning_replay(&cache, "key1", "gpt-5.2", Some("sessB")).await, None);
        let a = codex_reasoning_replay_cache_key_for_test("key1", "gpt-5.2", "sessA");
        let b = codex_reasoning_replay_cache_key_for_test("key2", "gpt-5.2", "sessA");
        assert_ne!(a, b);
        assert!(a.starts_with("codex-reasoning-replay:v2:"));
    }

    #[tokio::test]
    async fn treats_a_missing_session_key_as_a_no_op() {
        let cache = Cache::new();
        let entry = CodexReasoningReplayEntry { turns: vec![turn(vec![reasoning()], "before", "after", 1)] };
        write_codex_reasoning_replay(&cache, "key", "model", None, &entry).await;
        assert_eq!(read_codex_reasoning_replay(&cache, "key", "model", None).await, None);
        // Nothing was stored under any key derived from this call.
        assert_eq!(read_codex_reasoning_replay(&cache, "key", "model", Some("sess")).await, None);
    }

    #[tokio::test]
    async fn returns_none_for_corrupt_json_and_never_panics() {
        let cache = Cache::new();
        let key = codex_reasoning_replay_cache_key_for_test("key", "model", "sess");
        cache.put_text(&key, "not json", Duration::from_secs(60)).await;
        assert_eq!(read_codex_reasoning_replay(&cache, "key", "model", Some("sess")).await, None);
        cache
            .put_text(&key, r#"{"items":[{"nope":true}],"assistant_text_hash":"hash"}"#, Duration::from_secs(60))
            .await;
        assert_eq!(read_codex_reasoning_replay(&cache, "key", "model", Some("sess")).await, None);
        // A structurally-typed entry that fails the guard is rejected too.
        let bad = serde_json::to_string(&CodexReasoningReplayEntry {
            turns: vec![turn(vec![json!({ "no_type": 1 })], "a", "b", 1)],
        })
        .unwrap();
        cache.put_text(&key, &bad, Duration::from_secs(60)).await;
        assert_eq!(read_codex_reasoning_replay(&cache, "key", "model", Some("sess")).await, None);
    }

    #[tokio::test]
    async fn skips_an_oversized_entry_without_failing() {
        let cache = Cache::new();
        let item = json!({ "type": "reasoning", "encrypted_content": "x".repeat(300_000) });
        let entry = CodexReasoningReplayEntry { turns: vec![turn(vec![item], "before", "after", 1)] };
        write_codex_reasoning_replay(&cache, "key", "model", Some("sess"), &entry).await;
        assert_eq!(read_codex_reasoning_replay(&cache, "key", "model", Some("sess")).await, None);
    }

    #[tokio::test]
    async fn deletes_an_entry() {
        let cache = Cache::new();
        let entry = CodexReasoningReplayEntry { turns: vec![turn(vec![reasoning()], "before", "after", 1)] };
        write_codex_reasoning_replay(&cache, "key", "model", Some("sess"), &entry).await;
        delete_codex_reasoning_replay(&cache, "key", "model", Some("sess")).await;
        assert_eq!(read_codex_reasoning_replay(&cache, "key", "model", Some("sess")).await, None);
    }
}
