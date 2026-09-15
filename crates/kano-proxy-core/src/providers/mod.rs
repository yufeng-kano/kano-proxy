//! Provider adapters (apps/api/src/providers, docs/providers.md).

pub mod antigravity;
pub mod antigravity_limits;
pub mod claude_code;
pub mod cli;
pub mod codex;
pub mod codex_count;
pub mod codex_models;
pub mod codex_reasoning_cache;
pub mod codex_replay_history;
pub mod codex_usage;
pub mod custom_anthropic;
pub mod custom_openai;
pub mod custom_openai_reasoning;
pub mod grok;
pub mod grok_encrypted_content;
pub mod grok_reasoning_cache;
pub mod grok_reasoning_recovery;
pub mod identity;
pub mod refresh;
pub mod registry;
pub mod types;
pub mod usage_refresh;

pub use types::*;

/// Builtin provider ids (apps/api/src/env.ts `ProviderId`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ProviderId {
    #[serde(rename = "claude-code")]
    ClaudeCode,
    #[serde(rename = "codex")]
    Codex,
    #[serde(rename = "grok")]
    Grok,
    #[serde(rename = "antigravity")]
    Antigravity,
}

pub const PROVIDERS: [ProviderId; 4] = [ProviderId::ClaudeCode, ProviderId::Codex, ProviderId::Grok, ProviderId::Antigravity];

impl ProviderId {
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderId::ClaudeCode => "claude-code",
            ProviderId::Codex => "codex",
            ProviderId::Grok => "grok",
            ProviderId::Antigravity => "antigravity",
        }
    }
    pub fn parse(s: &str) -> Option<ProviderId> {
        PROVIDERS.into_iter().find(|p| p.as_str() == s)
    }
}

impl std::fmt::Display for ProviderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub fn is_provider_id(s: &str) -> bool {
    ProviderId::parse(s).is_some()
}
