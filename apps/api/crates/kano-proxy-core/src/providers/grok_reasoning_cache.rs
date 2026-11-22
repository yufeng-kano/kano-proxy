//! Grok reasoning replay cache (docs/providers.md § Grok).
//!
//! Session-scoped replay cache for Grok Responses encrypted reasoning.
//!
//! Claude Code / CC Switch may omit `thinking.signature` on later turns; xAI still
//! needs the prior turn's `encrypted_content` in Responses `input`.
//!
//! Isolation: SHA-256(`api_key_id` \0 `model` \0 `session`) — never shared across
//! callers or models. Entries live in [`crate::cache::Cache`] (`AppState::cache()`) for 1h.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cache::Cache;
use crate::providers::types::AffinityIds;

use super::grok_encrypted_content::is_valid_grok_encrypted_content;

const GROK_REASONING_REPLAY_TTL: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrokReasoningReplayEntry {
    /// Opaque xAI `encrypted_content` (also exposed as Anthropic `thinking.signature`).
    pub encrypted_content: String,
    /// SHA-256 hex of trailing assistant plaintext — match before reinject.
    pub assistant_text_hash: String,
}

/// Client `x-grok-conv-id`, else `x-grok-session-id`. Never invented.
pub fn grok_reasoning_replay_session_key(affinity: &AffinityIds) -> Option<String> {
    if let Some(conv) = affinity.conv_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return Some(conv.to_string());
    }
    affinity.session_id.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

pub fn hash_assistant_text(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// `None` unless all three scopes are known: an entry is never shared by a missing one.
fn scoped_cache_key(api_key_id: &str, model: &str, session_key: &str) -> Option<String> {
    if api_key_id.is_empty() || model.is_empty() || session_key.is_empty() {
        return None;
    }
    let material = format!("{api_key_id}\0{model}\0{session_key}");
    Some(format!("grok-reasoning-replay:v2:{}", hex::encode(Sha256::digest(material.as_bytes()))))
}

pub async fn read_grok_reasoning_replay(
    cache: &Cache,
    api_key_id: &str,
    model: &str,
    session_key: &str,
) -> Option<GrokReasoningReplayEntry> {
    let key = scoped_cache_key(api_key_id, model, session_key)?;
    let entry: GrokReasoningReplayEntry = cache.get_json(&key).await?;
    if !is_valid_grok_encrypted_content(&entry.encrypted_content) || entry.assistant_text_hash.is_empty() {
        return None;
    }
    Some(entry)
}

pub async fn write_grok_reasoning_replay(
    cache: &Cache,
    api_key_id: &str,
    model: &str,
    session_key: &str,
    entry: &GrokReasoningReplayEntry,
) {
    let Some(key) = scoped_cache_key(api_key_id, model, session_key) else { return };
    if !is_valid_grok_encrypted_content(&entry.encrypted_content) || entry.assistant_text_hash.is_empty() {
        return;
    }
    cache.put_json(&key, entry, GROK_REASONING_REPLAY_TTL).await;
}

pub async fn delete_grok_reasoning_replay(cache: &Cache, api_key_id: &str, model: &str, session_key: &str) {
    if let Some(key) = scoped_cache_key(api_key_id, model, session_key) {
        cache.delete(&key).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::grok_encrypted_content::test_fixtures::fake_grok_encrypted_content;

    fn affinity(conv: Option<&str>, session: Option<&str>) -> AffinityIds {
        AffinityIds {
            conv_id: conv.map(str::to_string),
            session_id: session.map(str::to_string),
            turn_idx: None,
        }
    }

    #[test]
    fn session_key_prefers_conv_id_then_session_id_and_never_invents_one() {
        let key = |conv, session| grok_reasoning_replay_session_key(&affinity(conv, session));
        assert_eq!(key(Some("conv"), Some("sess")).as_deref(), Some("conv"));
        assert_eq!(key(Some(" "), Some("sess")).as_deref(), Some("sess"));
        assert_eq!(key(None, Some("sess")).as_deref(), Some("sess"));
        assert_eq!(key(None, None), None);
    }

    #[tokio::test]
    async fn round_trips_an_entry_scoped_by_api_key_model_and_session() {
        let cache = Cache::new();
        let enc = fake_grok_encrypted_content(31);
        let hash = hash_assistant_text("hello");
        let entry = GrokReasoningReplayEntry { encrypted_content: enc.clone(), assistant_text_hash: hash.clone() };
        write_grok_reasoning_replay(&cache, "key1", "grok-4.5", "sessA", &entry).await;

        assert_eq!(read_grok_reasoning_replay(&cache, "key1", "grok-4.5", "sessA").await, Some(entry));
        assert_eq!(read_grok_reasoning_replay(&cache, "key2", "grok-4.5", "sessA").await, None);
        assert_eq!(read_grok_reasoning_replay(&cache, "key1", "grok-4.20", "sessA").await, None);
        assert_eq!(read_grok_reasoning_replay(&cache, "key1", "grok-4.5", "sessB").await, None);
    }

    #[tokio::test]
    async fn refuses_to_store_foreign_encrypted_content() {
        let cache = Cache::new();
        let entry = GrokReasoningReplayEntry {
            encrypted_content: "gAAAAABnot-grok".to_string(),
            assistant_text_hash: hash_assistant_text("x"),
        };
        write_grok_reasoning_replay(&cache, "key1", "grok-4.5", "sessA", &entry).await;
        assert_eq!(read_grok_reasoning_replay(&cache, "key1", "grok-4.5", "sessA").await, None);
    }

    #[tokio::test]
    async fn delete_removes_the_entry() {
        let cache = Cache::new();
        let entry = GrokReasoningReplayEntry {
            encrypted_content: fake_grok_encrypted_content(5),
            assistant_text_hash: hash_assistant_text("hi"),
        };
        write_grok_reasoning_replay(&cache, "key1", "grok-4.5", "sessA", &entry).await;
        delete_grok_reasoning_replay(&cache, "key1", "grok-4.5", "sessA").await;
        assert_eq!(read_grok_reasoning_replay(&cache, "key1", "grok-4.5", "sessA").await, None);
    }
}
