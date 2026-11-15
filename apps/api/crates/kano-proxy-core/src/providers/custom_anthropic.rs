//! BYO Anthropic-compatible provider (docs/providers.md § Custom providers).
//!
//! BYO Anthropic-compatible endpoint. `messages()` / `count_tokens()` are a native
//! passthrough (mirrors claude-code's `forwardToAnthropic`) MINUS every Claude-Code-OAuth
//! specific: no required-system prepend, no auto-added `effort-2025-11-24` beta, no fixed
//! base betas — the `anthropic-beta` header is forwarded verbatim from the client (or
//! omitted) rather than resolved. One exception to the passthrough: an endpoint that answers
//! 400 to an Anthropic-defined tool for its missing `input_schema` is sent the request once
//! more without those tools (custom_anthropic_tools.rs). `chat_completions()` builds a request via the shared
//! OpenAI↔Anthropic converters for the `/openai/v1` surface; `reasoning_effort` is dropped
//! there on purpose (the native surface gives full `thinking` control).

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::response::Response;
use http::{HeaderMap, StatusCode};
use serde_json::{Map, Value};

use super::custom_anthropic_tools::without_anthropic_defined_tools;
use super::custom_openai::{
    header_or, into_response, is_event_stream, model_list, non_empty_header, object_or_empty, rebuild,
};
use super::types::{
    AdapterError, CallExtras, ChatCompletionRequest, DynAdapter, ListedModels, ProviderAdapter,
};
use crate::db::custom_providers::CustomProviderRow;
use crate::pool::acquire::AcquiredAccount;
use crate::proxy::openai_anthropic::{
    add_conversion_cache_control, anthropic_sse_to_openai_stream, anthropic_to_openai_response,
    openai_to_anthropic_messages, AnthropicMessagesInput,
};
use crate::upstream::transport::{UpstreamRequest, UpstreamTransport};
use crate::AppState;

pub const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";

/// The beta whose presence marks a request as Claude Code to Anthropic's usage accounting
/// (docs/providers.md § Request edits). Dropped from a forwarded client list with the
/// identity switch on; kept on the claude-code adapter, whose OAuth upstream requires it.
pub const CLAUDE_CODE_BETA: &str = "claude-code-20250219";

pub struct CustomAnthropicAdapter {
    id: String,
    base: String,
    /// Injected transport for CLI providers (docs/cli.md) — see custom_openai.rs.
    transport: Option<Arc<dyn UpstreamTransport>>,
    list_models: bool,
}

impl CustomAnthropicAdapter {
    pub fn new(row: CustomProviderRow) -> Self {
        Self { id: row.slug, base: row.base_url, transport: None, list_models: true }
    }

    pub fn with_transport(mut self, transport: Arc<dyn UpstreamTransport>) -> Self {
        self.transport = Some(transport);
        self
    }

    /// CLI providers drop the live catalog — it is agent-reported, never pulled.
    pub fn without_list_models(mut self) -> Self {
        self.list_models = false;
        self
    }

    pub fn into_dyn(self) -> DynAdapter {
        Arc::new(self)
    }

    fn transport<'a>(&'a self, cx: &'a AppState) -> &'a Arc<dyn UpstreamTransport> {
        self.transport.as_ref().unwrap_or_else(|| cx.transport())
    }

    async fn forward_native(
        &self,
        cx: &AppState,
        url: String,
        account: &AcquiredAccount,
        body: &Value,
        headers: &HeaderMap,
        extras: &CallExtras,
    ) -> Result<Response, AdapterError> {
        let raw = object_or_empty(body);
        // With "Hide Claude Code identity" on, the `claude-code-20250219` entry is dropped from
        // the forwarded `anthropic-beta` (docs/providers.md § Request edits): it is one of the two
        // markers Anthropic reads to bill Claude Code usage. Other client betas stay verbatim.
        let beta = if extras.edits.strip_claude_code_line {
            non_empty_header(headers, "anthropic-beta").and_then(|beta| {
                let kept: Vec<&str> = beta.split(',').map(str::trim).filter(|b| !b.is_empty() && *b != CLAUDE_CODE_BETA).collect();
                (!kept.is_empty()).then(|| kept.join(","))
            })
        } else {
            non_empty_header(headers, "anthropic-beta").map(str::to_string)
        };
        let native_post = |body: &Map<String, Value>| {
            let mut request = UpstreamRequest::post(url.as_str())
                .header("x-api-key", &account.credential.access_token)
                .json(&Value::Object(body.clone()))
                .header("anthropic-version", header_or(headers, "anthropic-version", DEFAULT_ANTHROPIC_VERSION));
            if let Some(beta) = &beta {
                request = request.header("anthropic-beta", beta);
            }
            if let Some(timeout) = extras.first_byte_timeout {
                request = request.timeout(timeout);
            }
            request
        };
        let res = self.transport(cx).send(native_post(&raw)).await?;

        if res.status != StatusCode::BAD_REQUEST || is_event_stream(&res) {
            return Ok(into_response(res));
        }

        // The one retry: the endpoint refused an Anthropic-defined tool for having no
        // `input_schema`, so it gets the request again without those tools.
        let status = res.status;
        let res_headers = res.headers.clone();
        let refusal = res.bytes().await.map_err(|e| AdapterError::Other(e.into()))?;
        let Some(retry_body) = without_anthropic_defined_tools(&raw, &String::from_utf8_lossy(&refusal)) else {
            return Ok(rebuild(status, res_headers, refusal));
        };
        Ok(into_response(self.transport(cx).send(native_post(&retry_body)).await?))
    }
}

/// `createCustomAnthropicAdapter(row)`.
pub fn custom_anthropic_adapter(row: CustomProviderRow) -> DynAdapter {
    CustomAnthropicAdapter::new(row).into_dyn()
}

/// `createCustomAnthropicAdapter(row, agentFetch)`.
pub fn custom_anthropic_adapter_with_transport(
    row: CustomProviderRow,
    transport: Arc<dyn UpstreamTransport>,
) -> DynAdapter {
    CustomAnthropicAdapter::new(row).with_transport(transport).into_dyn()
}

#[async_trait]
impl ProviderAdapter for CustomAnthropicAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    fn has_messages(&self) -> bool {
        true
    }

    async fn messages(
        &self,
        cx: &AppState,
        account: &AcquiredAccount,
        body: &Value,
        headers: &HeaderMap,
        extras: &CallExtras,
    ) -> Result<Response, AdapterError> {
        self.forward_native(cx, format!("{}/v1/messages", self.base), account, body, headers, extras).await
    }

    fn has_count_tokens(&self) -> bool {
        true
    }

    async fn count_tokens(
        &self,
        cx: &AppState,
        account: &AcquiredAccount,
        body: &Value,
        headers: &HeaderMap,
        extras: &CallExtras,
    ) -> Result<Response, AdapterError> {
        self.forward_native(cx, format!("{}/v1/messages/count_tokens", self.base), account, body, headers, extras)
            .await
    }

    async fn chat_completions(
        &self,
        cx: &AppState,
        account: &AcquiredAccount,
        req: &ChatCompletionRequest,
        extras: &CallExtras,
    ) -> Result<Response, AdapterError> {
        let anthropic_body = openai_to_anthropic_messages(&AnthropicMessagesInput {
            model: req.upstream_model.clone(),
            messages: req.messages.clone(),
            max_tokens: req.max_tokens.unwrap_or(4096),
            stream: req.stream,
            tools: req.tools.clone(),
            tool_choice: req.tool_choice.clone(),
            response_format: req.response_format.clone(),
            stop: req.stop.clone(),
            temperature: req.temperature,
            top_p: req.top_p,
            // reasoning_effort intentionally dropped on this surface — no thinking /
            // output_config mapped from it for custom upstreams.
            thinking: None,
            output_config: None,
        });
        // Proxy-placed cache breakpoints (docs/api.md § Prompt cache); "Keep prompt cache for 1
        // hour" gives the message markers the conversation TTL too.
        let conversation = req.prompt_cache_key.is_some() || extras.edits.force_cache_ttl_1h;
        let outgoing = add_conversion_cache_control(&anthropic_body, conversation);

        let mut request = UpstreamRequest::post(format!("{}/v1/messages", self.base))
            .header("x-api-key", &account.credential.access_token)
            .json(&Value::Object(outgoing))
            .header("anthropic-version", DEFAULT_ANTHROPIC_VERSION);
        if let Some(timeout) = extras.first_byte_timeout {
            request = request.timeout(timeout);
        }
        let res = self.transport(cx).send(request).await?;

        if req.stream == Some(true) {
            if !res.status.is_success() {
                return Ok(into_response(res));
            }
            let stream = anthropic_sse_to_openai_stream(res.body, &req.raw_model);
            return Ok(Response::builder()
                .status(res.status)
                .header("content-type", "text/event-stream; charset=utf-8")
                .header("cache-control", "no-cache")
                .body(Body::from_stream(stream))
                .expect("response builds"));
        }

        let status = res.status;
        let headers = res.headers.clone();
        let bytes = res.bytes().await.map_err(|e| AdapterError::Other(e.into()))?;
        if !status.is_success() {
            // Full headers, not just content-type: a CLI provider's tunnel fault marker
            // (x-tunnel-fault / x-tunnel-upstream) must survive this rebuild or dispatch can
            // neither fail over nor bench (docs/cli.md).
            return Ok(rebuild(status, headers, bytes));
        }
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(message)) => {
                let converted = anthropic_to_openai_response(&message, &req.raw_model);
                let body = serde_json::to_vec(&Value::Object(converted)).expect("json");
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .expect("response builds"))
            }
            _ => Ok(rebuild(status, HeaderMap::new(), bytes)),
        }
    }

    fn has_list_models(&self) -> bool {
        self.list_models
    }

    async fn list_models(&self, cx: &AppState, account: &AcquiredAccount) -> ListedModels {
        let request = UpstreamRequest::get(format!("{}/v1/models", self.base))
            .header("x-api-key", &account.credential.access_token)
            .header("anthropic-version", DEFAULT_ANTHROPIC_VERSION);
        let res = match self.transport(cx).send(request).await {
            Ok(res) => res,
            Err(e) => return ListedModels { models: Vec::new(), error: Some(e.to_string()) },
        };
        if !res.status.is_success() {
            return ListedModels { models: Vec::new(), error: Some(format!("models {}", res.status.as_u16())) };
        }
        match res.json_value().await {
            Ok(json) => ListedModels { models: model_list(&json, true), error: None },
            Err(e) => ListedModels { models: Vec::new(), error: Some(e.to_string()) },
        }
    }
}

/// The `messages` field of an internal chat request, for the conversion tests.
#[cfg(test)]
fn messages(values: Value) -> Vec<Value> {
    values.as_array().cloned().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::super::custom_openai::test_support::*;
    use super::super::types::UpstreamModel;
    use super::*;
    use crate::upstream::transport::MockTransport;
    use serde_json::json;

    const BASE: &str = "https://upstream.example.com";
    const KEY: &str = "sk-ant-test-key";

    fn adapter() -> CustomAnthropicAdapter {
        CustomAnthropicAdapter::new(custom_row("anthropic", BASE))
    }

    fn chat(messages_json: Value) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "my-claude/claude-3".into(),
            raw_model: "my-claude/claude-3".into(),
            upstream_model: "claude-3".into(),
            messages: messages(messages_json),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn has_no_fetch_usage_or_audio_input() {
        let adapter = adapter();
        assert!(!adapter.has_fetch_usage());
        assert_eq!(adapter.audio_input(), None);
        assert!(!adapter.has_audio_transcriptions());
        assert_eq!(adapter.id(), "my-claude");
    }

    // -----------------------------------------------------------------------
    // messages() — native passthrough

    #[tokio::test]
    async fn posts_to_base_v1_messages_with_x_api_key_auth() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({}));
        let state = adapter_state(transport.clone());
        adapter()
            .messages(
                &state,
                &account(KEY),
                &json!({ "model": "claude-3", "messages": [] }),
                &HeaderMap::new(),
                &CallExtras::default(),
            )
            .await
            .expect("messages");

        let sent = &transport.requests()[0];
        assert_eq!(sent.url, "https://upstream.example.com/v1/messages");
        assert_eq!(sent.header("x-api-key"), Some(KEY));
        assert_eq!(sent.header("authorization"), None);
    }

    #[tokio::test]
    async fn preserves_cache_control_and_thinking_in_the_body_verbatim() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({}));
        let state = adapter_state(transport.clone());
        let client_body = json!({
            "model": "claude-3",
            "messages": [{
                "role": "user",
                "content": [{ "type": "text", "text": "hi", "cache_control": { "type": "ephemeral" } }],
            }],
            "thinking": { "type": "enabled", "budget_tokens": 2048 },
            "system": [{ "type": "text", "text": "sys", "cache_control": { "type": "ephemeral" } }],
        });
        adapter()
            .messages(&state, &account(KEY), &client_body, &HeaderMap::new(), &CallExtras::default())
            .await
            .expect("messages");
        assert_eq!(transport.requests()[0].json(), client_body);
    }

    #[tokio::test]
    async fn defaults_anthropic_version_when_the_client_sends_none() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({}));
        let state = adapter_state(transport.clone());
        adapter()
            .messages(
                &state,
                &account(KEY),
                &json!({ "model": "claude-3", "messages": [] }),
                &HeaderMap::new(),
                &CallExtras::default(),
            )
            .await
            .expect("messages");
        assert_eq!(transport.requests()[0].header("anthropic-version"), Some("2023-06-01"));
    }

    #[tokio::test]
    async fn forwards_the_clients_anthropic_version_when_present() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({}));
        let state = adapter_state(transport.clone());
        let mut headers = HeaderMap::new();
        headers.insert("anthropic-version", http::HeaderValue::from_static("2024-01-01"));
        adapter()
            .messages(&state, &account(KEY), &json!({ "model": "claude-3", "messages": [] }), &headers, &CallExtras::default())
            .await
            .expect("messages");
        assert_eq!(transport.requests()[0].header("anthropic-version"), Some("2024-01-01"));
    }

    #[tokio::test]
    async fn forwards_the_clients_anthropic_beta_verbatim_with_no_base_betas_added() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({}));
        let state = adapter_state(transport.clone());
        let mut headers = HeaderMap::new();
        headers.insert("anthropic-beta", http::HeaderValue::from_static("some-client-beta-2026"));
        adapter()
            .messages(&state, &account(KEY), &json!({ "model": "claude-3", "messages": [] }), &headers, &CallExtras::default())
            .await
            .expect("messages");
        assert_eq!(transport.requests()[0].header("anthropic-beta"), Some("some-client-beta-2026"));
    }

    #[tokio::test]
    async fn omits_anthropic_beta_entirely_when_the_client_sends_none() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({}));
        let state = adapter_state(transport.clone());
        adapter()
            .messages(
                &state,
                &account(KEY),
                &json!({ "model": "claude-3", "messages": [] }),
                &HeaderMap::new(),
                &CallExtras::default(),
            )
            .await
            .expect("messages");
        let sent = &transport.requests()[0];
        assert_eq!(sent.header("anthropic-beta"), None);
        let rendered = format!("{:?}", sent.headers);
        assert!(!rendered.contains("oauth-"));
        assert!(!rendered.contains("claude-code-"));
    }

    #[tokio::test]
    async fn the_identity_switch_drops_only_the_claude_code_beta_from_the_forwarded_list() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({}));
        let state = adapter_state(transport.clone());
        let mut headers = HeaderMap::new();
        headers.insert(
            "anthropic-beta",
            http::HeaderValue::from_static("claude-code-20250219,some-client-beta-2026"),
        );
        adapter()
            .messages(
                &state,
                &account(KEY),
                &json!({ "model": "claude-3", "messages": [] }),
                &headers,
                &CallExtras { edits: crate::providers::types::RequestEdits { strip_claude_code_line: true, ..Default::default() }, ..Default::default() },
            )
            .await
            .expect("messages");
        assert_eq!(transport.requests()[0].header("anthropic-beta"), Some("some-client-beta-2026"));

        // The only entry: the header goes entirely, not as an empty string.
        transport.respond_json(StatusCode::OK, json!({}));
        let mut headers = HeaderMap::new();
        headers.insert("anthropic-beta", http::HeaderValue::from_static("claude-code-20250219"));
        adapter()
            .messages(
                &state,
                &account(KEY),
                &json!({ "model": "claude-3", "messages": [] }),
                &headers,
                &CallExtras { edits: crate::providers::types::RequestEdits { strip_claude_code_line: true, ..Default::default() }, ..Default::default() },
            )
            .await
            .expect("messages");
        assert_eq!(transport.requests()[1].header("anthropic-beta"), None);
    }

    #[tokio::test]
    async fn does_not_prepend_a_claude_code_system_line() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({}));
        let state = adapter_state(transport.clone());
        adapter()
            .messages(
                &state,
                &account(KEY),
                &json!({ "model": "claude-3", "messages": [], "system": "be terse" }),
                &HeaderMap::new(),
                &CallExtras::default(),
            )
            .await
            .expect("messages");
        assert_eq!(transport.requests()[0].json()["system"], "be terse");
    }

    // -----------------------------------------------------------------------
    // countTokens()

    #[tokio::test]
    async fn count_tokens_posts_to_the_count_tokens_path_with_the_same_auth_construction() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "input_tokens": 12 }));
        let state = adapter_state(transport.clone());
        let mut headers = HeaderMap::new();
        headers.insert("anthropic-beta", http::HeaderValue::from_static("x-beta"));
        let res = adapter()
            .count_tokens(
                &state,
                &account(KEY),
                &json!({ "model": "claude-3", "messages": [{ "role": "user", "content": "hi" }] }),
                &headers,
                &CallExtras::default(),
            )
            .await
            .expect("count tokens");

        let sent = &transport.requests()[0];
        assert_eq!(sent.url, "https://upstream.example.com/v1/messages/count_tokens");
        assert_eq!(sent.header("x-api-key"), Some(KEY));
        assert_eq!(sent.header("anthropic-beta"), Some("x-beta"));
        assert_eq!(res.status(), StatusCode::OK);
    }

    // -----------------------------------------------------------------------
    // The one retry without Anthropic-defined tools

    /// A local Messages server refusing an Anthropic-defined tool, in the wording Claude Code showed.
    fn missing_input_schema_400(index: usize) -> crate::upstream::UpstreamResponse {
        crate::upstream::UpstreamResponse::json(
            StatusCode::BAD_REQUEST,
            &json!({ "type": "error", "error": { "type": "invalid_request_error", "message": format!(
                "1 validation error:\n    {{'type': 'missing', 'loc': 'body.tools.{index}.input_schema', 'msg': 'Field required', 'input': '<dict of 3 items>'}}"
            ) } }),
        )
    }

    fn body_with_server_tool() -> Value {
        json!({
            "model": "qwen",
            "max_tokens": 64,
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }],
            "tools": [
                { "name": "lookup", "description": "looks up", "input_schema": { "type": "object" } },
                { "type": "web_search_20250305", "name": "web_search", "max_uses": 5 },
            ],
        })
    }

    #[tokio::test]
    async fn a_refused_anthropic_defined_tool_is_dropped_on_one_retry_with_the_same_headers() {
        let transport = MockTransport::new();
        transport.expect(|_| Ok(missing_input_schema_400(1)));
        transport.respond_sse(StatusCode::OK, "event: message_stop\ndata: {}\n\n");
        let state = adapter_state(transport.clone());
        let mut headers = HeaderMap::new();
        headers.insert("anthropic-version", http::HeaderValue::from_static("2024-01-01"));
        headers.insert("anthropic-beta", http::HeaderValue::from_static("x-beta"));
        let client_body = body_with_server_tool();
        let res = adapter()
            .messages(&state, &account(KEY), &client_body, &headers, &CallExtras::default())
            .await
            .expect("messages");

        let requests = transport.requests();
        assert_eq!(requests.len(), 2);
        // The first send is still the client's body, untouched.
        assert_eq!(requests[0].json(), client_body);
        let mut expected = client_body.clone();
        expected["tools"] = json!([client_body["tools"][0].clone()]);
        assert_eq!(requests[1].json(), expected);
        assert_eq!(requests[1].url, requests[0].url);
        for name in ["x-api-key", "anthropic-version", "anthropic-beta"] {
            assert!(requests[0].header(name).is_some(), "{name}");
            assert_eq!(requests[1].header(name), requests[0].header(name), "{name}");
        }
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_text(res).await, "event: message_stop\ndata: {}\n\n");
    }

    #[tokio::test]
    async fn count_tokens_gets_the_same_retry() {
        let transport = MockTransport::new();
        transport.expect(|_| Ok(missing_input_schema_400(1)));
        transport.respond_json(StatusCode::OK, json!({ "input_tokens": 12 }));
        let state = adapter_state(transport.clone());
        let res = adapter()
            .count_tokens(&state, &account(KEY), &body_with_server_tool(), &HeaderMap::new(), &CallExtras::default())
            .await
            .expect("count tokens");

        let requests = transport.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].url, "https://upstream.example.com/v1/messages/count_tokens");
        assert_eq!(requests[1].json()["tools"].as_array().map(Vec::len), Some(1));
        assert_eq!(body_json(res).await, json!({ "input_tokens": 12 }));
    }

    #[tokio::test]
    async fn a_400_that_is_not_about_an_anthropic_defined_tool_goes_back_once_with_its_headers() {
        // Unrelated wording, and the right wording pointing at a tool that has its schema.
        let unrelated = || {
            let mut headers = HeaderMap::new();
            headers.insert("x-tunnel-upstream", http::HeaderValue::from_static("1"));
            headers.insert("content-type", http::HeaderValue::from_static("application/json"));
            crate::upstream::UpstreamResponse::from_bytes(
                StatusCode::BAD_REQUEST,
                headers,
                r#"{"error":{"message":"max_tokens: Field required"}}"#,
            )
        };
        let transport = MockTransport::new();
        transport.expect(move |_| Ok(unrelated()));
        let state = adapter_state(transport.clone());
        let res = adapter()
            .messages(&state, &account(KEY), &body_with_server_tool(), &HeaderMap::new(), &CallExtras::default())
            .await
            .expect("messages");
        assert_eq!(transport.requests().len(), 1);
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(res.headers().get("x-tunnel-upstream").unwrap(), "1");
        assert_eq!(body_json(res).await, json!({ "error": { "message": "max_tokens: Field required" } }));

        let transport = MockTransport::new();
        transport.expect(|_| Ok(missing_input_schema_400(0)));
        let state = adapter_state(transport.clone());
        let res = adapter()
            .messages(&state, &account(KEY), &body_with_server_tool(), &HeaderMap::new(), &CallExtras::default())
            .await
            .expect("messages");
        assert_eq!(transport.requests().len(), 1);
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_400_event_stream_is_returned_as_received_and_never_read_for_a_retry() {
        const REFUSAL: &str =
            "event: error\ndata: {\"message\":\"body.tools.1.input_schema: Field required\"}\n\n";
        let transport = MockTransport::new();
        transport.respond_sse(StatusCode::BAD_REQUEST, REFUSAL);
        let state = adapter_state(transport.clone());
        let res = adapter()
            .messages(&state, &account(KEY), &body_with_server_tool(), &HeaderMap::new(), &CallExtras::default())
            .await
            .expect("messages");
        assert_eq!(transport.requests().len(), 1);
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_text(res).await, REFUSAL);
    }

    #[tokio::test]
    async fn the_retrys_own_answer_goes_back_without_a_third_send() {
        let transport = MockTransport::new();
        transport.expect(|_| Ok(missing_input_schema_400(1)));
        transport.respond_json(StatusCode::BAD_REQUEST, json!({ "error": { "message": "context too long" } }));
        let state = adapter_state(transport.clone());
        let res = adapter()
            .messages(&state, &account(KEY), &body_with_server_tool(), &HeaderMap::new(), &CallExtras::default())
            .await
            .expect("messages");
        assert_eq!(transport.requests().len(), 2);
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(res).await, json!({ "error": { "message": "context too long" } }));
    }

    // -----------------------------------------------------------------------
    // chatCompletions() — /openai/v1 surface via the shared converters

    #[tokio::test]
    async fn places_the_proxy_cache_breakpoints_with_no_system_prepend() {
        let transport = MockTransport::new();
        transport
            .respond_json(StatusCode::OK, json!({ "content": [], "usage": { "input_tokens": 1, "output_tokens": 1 } }));
        let state = adapter_state(transport.clone());
        adapter()
            .chat_completions(
                &state,
                &account(KEY),
                &chat(json!([{ "role": "system", "content": "sys" }, { "role": "user", "content": "hi" }])),
                &CallExtras::default(),
            )
            .await
            .expect("chat completions");

        let sent = transport.requests()[0].json();
        assert_eq!(
            sent["system"],
            json!([{ "type": "text", "text": "sys", "cache_control": { "type": "ephemeral", "ttl": "1h" } }])
        );
        assert_eq!(
            sent["messages"][0]["content"],
            json!([{ "type": "text", "text": "hi", "cache_control": { "type": "ephemeral" } }])
        );
        assert_eq!(sent.to_string().matches("\"cache_control\"").count(), 2);
    }

    #[tokio::test]
    async fn converts_the_openai_request_into_an_anthropic_messages_body() {
        let transport = MockTransport::new();
        transport.respond_json(
            StatusCode::OK,
            json!({
                "id": "msg_1",
                "content": [{ "type": "text", "text": "hello" }],
                "stop_reason": "end_turn",
                "usage": { "input_tokens": 3, "output_tokens": 2 },
            }),
        );
        let state = adapter_state(transport.clone());
        let mut req = chat(json!([{ "role": "user", "content": "hi" }]));
        req.max_tokens = Some(512);
        req.reasoning_effort = Some(crate::utils::reasoning::ReasoningEffort::High);
        let res = adapter().chat_completions(&state, &account(KEY), &req, &CallExtras::default()).await.expect("chat");

        let sent = transport.requests()[0].json();
        assert_eq!(sent["model"], "claude-3");
        assert_eq!(sent["max_tokens"], 512);
        // reasoning_effort is dropped on this surface — no thinking/output_config synthesized.
        assert!(sent.get("thinking").is_none());
        assert!(sent.get("output_config").is_none());
        let json = body_json(res).await;
        assert_eq!(json["choices"][0]["message"]["content"], "hello");
    }

    #[tokio::test]
    async fn sends_no_anthropic_beta_header_on_this_surface() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "content": [] }));
        let state = adapter_state(transport.clone());
        adapter()
            .chat_completions(&state, &account(KEY), &chat(json!([{ "role": "user", "content": "hi" }])), &CallExtras::default())
            .await
            .expect("chat completions");
        assert_eq!(transport.requests()[0].header("anthropic-beta"), None);
    }

    #[tokio::test]
    async fn an_error_rebuild_keeps_every_upstream_header() {
        let transport = MockTransport::new();
        transport.expect(|_| {
            let mut headers = HeaderMap::new();
            headers.insert("x-tunnel-fault", http::HeaderValue::from_static("offline"));
            headers.insert("content-type", http::HeaderValue::from_static("application/json"));
            Ok(crate::upstream::UpstreamResponse::from_bytes(
                StatusCode::BAD_GATEWAY,
                headers,
                r#"{"error":{"type":"agent_fault","reason":"offline"}}"#,
            ))
        });
        let state = adapter_state(transport);
        let res = adapter()
            .chat_completions(&state, &account(KEY), &chat(json!([{ "role": "user", "content": "hi" }])), &CallExtras::default())
            .await
            .expect("chat completions");
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(res.headers().get("x-tunnel-fault").unwrap(), "offline");
    }

    #[tokio::test]
    async fn a_stream_is_converted_to_openai_sse() {
        let transport = MockTransport::new();
        transport.respond_sse(
            StatusCode::OK,
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\n\
             event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n\
             event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        );
        let state = adapter_state(transport);
        let mut req = chat(json!([{ "role": "user", "content": "hi" }]));
        req.stream = Some(true);
        let res = adapter().chat_completions(&state, &account(KEY), &req, &CallExtras::default()).await.expect("chat");
        assert_eq!(res.headers().get("content-type").unwrap(), "text/event-stream; charset=utf-8");
        let text = body_text(res).await;
        assert!(text.contains("chat.completion.chunk"));
        assert!(text.contains("hi"));
        assert!(text.contains("data: [DONE]"));
    }

    #[tokio::test]
    async fn a_stream_error_is_returned_as_received() {
        let transport = MockTransport::new();
        transport.expect(|_| {
            Ok(crate::upstream::UpstreamResponse::json(
                StatusCode::TOO_MANY_REQUESTS,
                &json!({ "error": { "type": "rate_limit_error" } }),
            ))
        });
        let state = adapter_state(transport);
        let mut req = chat(json!([{ "role": "user", "content": "hi" }]));
        req.stream = Some(true);
        let res = adapter().chat_completions(&state, &account(KEY), &req, &CallExtras::default()).await.expect("chat");
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body_json(res).await["error"]["type"], "rate_limit_error");
    }

    // -----------------------------------------------------------------------
    // listModels()

    #[tokio::test]
    async fn list_models_gets_base_v1_models_with_x_api_key_auth() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "data": [{ "id": "claude-3", "display_name": "Claude 3" }] }));
        let state = adapter_state(transport.clone());
        let result = adapter().list_models(&state, &account(KEY)).await;
        assert_eq!(transport.requests()[0].url, "https://upstream.example.com/v1/models");
        assert_eq!(transport.requests()[0].header("x-api-key"), Some(KEY));
        assert_eq!(
            result.models,
            vec![UpstreamModel { id: "claude-3".into(), display_name: Some("Claude 3".into()) }]
        );
    }
}
