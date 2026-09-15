//! Builtin adapter lookup (apps/api/src/providers/index.ts `getAdapter`). Adapters register
//! here as their ports land; until then a builtin answers `AdapterError::Unsupported` so the
//! routing and dispatch ports can be exercised with test adapters.

use std::sync::Arc;

use async_trait::async_trait;
use axum::response::Response;

use super::types::{AdapterError, CallExtras, ChatCompletionRequest, DynAdapter, ProviderAdapter};
use super::ProviderId;
use crate::pool::AcquiredAccount;
use crate::AppState;

struct Pending(&'static str);

#[async_trait]
impl ProviderAdapter for Pending {
    fn id(&self) -> &str {
        self.0
    }
    async fn chat_completions(&self, _: &AppState, _: &AcquiredAccount, _: &ChatCompletionRequest, _: &CallExtras) -> Result<Response, AdapterError> {
        Err(AdapterError::Unsupported("chat_completions"))
    }
}

pub fn get_adapter(provider: ProviderId) -> DynAdapter {
    match provider {
        ProviderId::ClaudeCode => Arc::new(Pending("claude-code")),
        ProviderId::Codex => Arc::new(Pending("codex")),
        ProviderId::Grok => Arc::new(Pending("grok")),
        ProviderId::Antigravity => Arc::new(Pending("antigravity")),
    }
}
