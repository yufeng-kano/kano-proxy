//! Provider adapter contract (apps/api/src/providers/types.ts, docs/providers.md). Adapters
//! return HTTP responses whose bodies stream; `Response` is the axum response type.

use std::sync::Arc;

use async_trait::async_trait;
use axum::response::Response;
use http::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::pool::acquire::AcquiredAccount;
use crate::utils::reasoning::ReasoningEffort;
use crate::AppState;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageWindow {
    pub label: String,
    /// Percent used, 0–100 (not a 0–1 fraction).
    pub utilization: Option<f64>,
    pub resets_at: Option<String>,
    /// Replaces the percent caption when set — an edition's own "used / ceiling" reading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

/// Computed at read time from priority order plus router facts, never stored
/// (docs/admin-ui.md § Providers page).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    Active,
    ActiveNoFable,
    ActiveFable,
    Standby,
    Limited,
    Benched,
    Unusable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountUsage {
    pub windows: Vec<UsageWindow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountUsageView {
    pub id: String,
    pub priority: i32,
    pub status: AccountStatus,
    pub label: Option<String>,
    pub custom_label: Option<String>,
    pub account: Option<Map<String, Value>>,
    pub usage: Option<AccountUsage>,
    pub error: Option<String>,
    pub stale: bool,
}

/// Opaque client-supplied affinity ids forwarded verbatim upstream; never generated here.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AffinityIds {
    pub conv_id: Option<String>,
    pub session_id: Option<String>,
    pub turn_idx: Option<String>,
}

/// The normalized chat request every adapter receives (fields as in the TypeScript type).
#[derive(Debug, Clone, Default)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub raw_model: String,
    pub upstream_model: String,
    pub messages: Vec<Value>,
    pub stream: Option<bool>,
    pub max_tokens: Option<u64>,
    pub tools: Option<Value>,
    pub tool_choice: Option<Value>,
    pub response_format: Option<Value>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub stop: Option<Vec<String>>,
    pub prompt_cache_key: Option<String>,
    pub affinity: Option<AffinityIds>,
    /// Set only by `POST /openai/v1/responses` when every resolved candidate is codex.
    pub responses_body: Option<Map<String, Value>>,
    /// The OpenAI Chat Completions-shaped body this request came from.
    pub raw_body: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpstreamModel {
    pub id: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Default)]
pub struct ListedModels {
    pub models: Vec<UpstreamModel>,
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub struct FetchedUsage {
    pub windows: Vec<UsageWindow>,
    pub account: Map<String, Value>,
    pub stale: bool,
    pub error: Option<String>,
    /// Usage endpoint blocked (e.g. chatgpt bot wall); account may still be usable.
    pub edge_blocked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioInput {
    Convert,
    Passthrough,
}

/// Per-call context the TypeScript `extras` carried: `apiKeyId` scopes per-caller cache
/// state; deadlines come from the dispatch-scoped `first_byte_timeout`. Background work
/// that `waitUntil` kept alive is simply `tokio::spawn`ed.
#[derive(Debug, Clone, Default)]
pub struct CallExtras {
    pub api_key_id: Option<String>,
    pub first_byte_timeout: Option<std::time::Duration>,
}

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("this adapter does not implement {0}")]
    Unsupported(&'static str),
    #[error(transparent)]
    Transport(#[from] crate::upstream::transport::TransportError),
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

/// A multipart audio transcription request as received from the client.
#[derive(Debug, Clone)]
pub struct AudioForm {
    pub fields: Vec<(String, String)>,
    pub file_name: String,
    pub file_content_type: String,
    pub file: bytes::Bytes,
}

#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    /// Builtin `ProviderId`, or a custom provider's slug for BYO adapters.
    fn id(&self) -> &str;

    /// How an OpenAI `input_audio` part reaches this upstream (docs/api.md § Audio input);
    /// `None` means the OpenAI route answers `400 unsupported_modality`.
    fn audio_input(&self) -> Option<AudioInput> {
        None
    }

    async fn chat_completions(
        &self,
        cx: &AppState,
        account: &AcquiredAccount,
        req: &ChatCompletionRequest,
        extras: &CallExtras,
    ) -> Result<Response, AdapterError>;

    fn has_messages(&self) -> bool {
        false
    }
    /// Anthropic Messages entry (claude-code / custom-anthropic passthrough, grok conversion).
    async fn messages(
        &self,
        _cx: &AppState,
        _account: &AcquiredAccount,
        _body: &Value,
        _headers: &HeaderMap,
        _extras: &CallExtras,
    ) -> Result<Response, AdapterError> {
        Err(AdapterError::Unsupported("messages"))
    }

    fn has_audio_transcriptions(&self) -> bool {
        false
    }
    async fn audio_transcriptions(
        &self,
        _cx: &AppState,
        _account: &AcquiredAccount,
        _form: &AudioForm,
        _raw_model: &str,
        _upstream_model: &str,
        _extras: &CallExtras,
    ) -> Result<Response, AdapterError> {
        Err(AdapterError::Unsupported("audio_transcriptions"))
    }

    fn has_count_tokens(&self) -> bool {
        false
    }
    /// Native Anthropic count_tokens (same providers as `messages`). Never streams.
    async fn count_tokens(
        &self,
        _cx: &AppState,
        _account: &AcquiredAccount,
        _body: &Value,
        _headers: &HeaderMap,
        _extras: &CallExtras,
    ) -> Result<Response, AdapterError> {
        Err(AdapterError::Unsupported("count_tokens"))
    }

    /// Whether an account with these stored profile facts can serve `upstream_model`.
    /// Must fail open on missing facts.
    fn supports_model(&self, _meta: Option<&Map<String, Value>>, _upstream_model: &str) -> bool {
        true
    }

    fn has_list_models(&self) -> bool {
        false
    }
    async fn list_models(&self, _cx: &AppState, _account: &AcquiredAccount) -> ListedModels {
        ListedModels::default()
    }

    fn has_fetch_usage(&self) -> bool {
        false
    }
    async fn fetch_usage(&self, _cx: &AppState, _account: &AcquiredAccount) -> FetchedUsage {
        FetchedUsage::default()
    }

    /// Refresh the credential when it is about to expire; the default returns it unchanged.
    async fn refresh_if_needed(&self, _cx: &AppState, account: AcquiredAccount) -> Result<AcquiredAccount, AdapterError> {
        Ok(account)
    }
}

pub type DynAdapter = Arc<dyn ProviderAdapter>;
