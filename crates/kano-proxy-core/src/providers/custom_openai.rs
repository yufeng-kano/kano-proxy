//! Port of apps/api/src/providers/custom_openai.ts (docs/providers.md § Custom providers).
//!
//! BYO OpenAI-compatible endpoint. Near-passthrough: the outgoing body is the client's own
//! OpenAI Chat Completions body (or its Anthropic→OpenAI conversion) with only `model`
//! rewritten to the bare upstream id — `temperature`, `reasoning_effort`, `response_format`,
//! etc. all ride along unmodified on the first send. Always also sets
//! `stream_options.include_usage: true` (stream and non-stream; TabbyAPI-class upstreams omit
//! `usage` without it). Client-supplied `stream_options` other keys are kept. A recognized
//! unsupported-effort HTTP 400 may rewrite only `reasoning_effort` and POST once more on the
//! same account. This deliberately diverges from the built-in adapters, which strip
//! `temperature` and clamp `reasoning_effort` to a provider ceiling. No `messages()` — the
//! `/anthropic` surface reaches this adapter through the existing Anthropic→OpenAI conversion
//! path, same as grok/codex. `count_tokens()` exists only when the row has a
//! `count_tokens_url`.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::response::Response;
use bytes::Bytes;
use http::{HeaderMap, StatusCode};
use rand::RngCore;
use serde_json::{Map, Value};

use super::custom_openai_reasoning::remap_unsupported_effort_body;
use super::types::{
    AdapterError, AudioForm, AudioInput, CallExtras, ChatCompletionRequest, DynAdapter, ListedModels, ProviderAdapter,
    UpstreamModel,
};
use crate::db::custom_providers::CustomProviderRow;
use crate::pool::acquire::AcquiredAccount;
use crate::upstream::transport::{UpstreamRequest, UpstreamResponse, UpstreamTransport};
use crate::AppState;

pub const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct CustomOpenAiAdapter {
    id: String,
    base: String,
    count_tokens_url: Option<String>,
    /// Injected transport for CLI providers (docs/cli.md): same adapter semantics, but the
    /// request goes down the agent tunnel instead of the network. `None` uses
    /// `AppState::transport()`, the `fetch` default of the TypeScript constructor.
    transport: Option<Arc<dyn UpstreamTransport>>,
    /// CLI providers drop the live catalog — it is agent-reported, never pulled
    /// (`delete adapter.listModels` in apps/api/src/providers/cli.ts).
    list_models: bool,
}

impl CustomOpenAiAdapter {
    pub fn new(row: CustomProviderRow) -> Self {
        Self {
            id: row.slug,
            base: row.base_url,
            count_tokens_url: row.count_tokens_url,
            transport: None,
            list_models: true,
        }
    }

    pub fn with_transport(mut self, transport: Arc<dyn UpstreamTransport>) -> Self {
        self.transport = Some(transport);
        self
    }

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
}

/// `createCustomOpenAIAdapter(row)`.
pub fn custom_openai_adapter(row: CustomProviderRow) -> DynAdapter {
    CustomOpenAiAdapter::new(row).into_dyn()
}

/// `createCustomOpenAIAdapter(row, agentFetch)`.
pub fn custom_openai_adapter_with_transport(
    row: CustomProviderRow,
    transport: Arc<dyn UpstreamTransport>,
) -> DynAdapter {
    CustomOpenAiAdapter::new(row).with_transport(transport).into_dyn()
}

#[async_trait]
impl ProviderAdapter for CustomOpenAiAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    /// The client body is forwarded near-verbatim, so an `input_audio` part reaches the
    /// endpoint untouched (docs/api.md § Audio input).
    fn audio_input(&self) -> Option<AudioInput> {
        Some(AudioInput::Passthrough)
    }

    async fn chat_completions(
        &self,
        cx: &AppState,
        account: &AcquiredAccount,
        req: &ChatCompletionRequest,
        extras: &CallExtras,
    ) -> Result<Response, AdapterError> {
        let url = format!("{}/chat/completions", self.base);
        let upstream_body = outgoing_body(req);
        let res = self
            .transport(cx)
            .send(post_json(&url, &account.credential.access_token, &upstream_body, extras))
            .await?;

        if res.status.is_success() || res.status != StatusCode::BAD_REQUEST || is_event_stream(&res) {
            return Ok(into_response(res));
        }

        let status = res.status;
        let headers = res.headers.clone();
        let body = res.bytes().await.map_err(|e| AdapterError::Other(e.into()))?;
        let text = String::from_utf8_lossy(&body).into_owned();
        let Some(remapped) = remap_unsupported_effort_body(&upstream_body, &text) else {
            return Ok(rebuild(status, headers, body));
        };
        let retry = self
            .transport(cx)
            .send(post_json(&url, &account.credential.access_token, &remapped, extras))
            .await?;
        Ok(into_response(retry))
    }

    fn has_audio_transcriptions(&self) -> bool {
        true
    }

    async fn audio_transcriptions(
        &self,
        cx: &AppState,
        account: &AcquiredAccount,
        form: &AudioForm,
        _raw_model: &str,
        upstream_model: &str,
        extras: &CallExtras,
    ) -> Result<Response, AdapterError> {
        let url = format!("{}/audio/transcriptions", self.base);
        let (content_type, body) = encode_multipart(form, upstream_model);
        let mut request = UpstreamRequest::post(url)
            .header("authorization", &format!("Bearer {}", account.credential.access_token))
            .header("content-type", &content_type)
            .body(body);
        if let Some(timeout) = extras.first_byte_timeout {
            request = request.timeout(timeout);
        }
        Ok(into_response(self.transport(cx).send(request).await?))
    }

    fn has_list_models(&self) -> bool {
        self.list_models
    }

    async fn list_models(&self, cx: &AppState, account: &AcquiredAccount) -> ListedModels {
        let request = UpstreamRequest::get(format!("{}/models", self.base))
            .header("authorization", &format!("Bearer {}", account.credential.access_token));
        let res = match self.transport(cx).send(request).await {
            Ok(res) => res,
            Err(e) => return ListedModels { models: Vec::new(), error: Some(e.to_string()) },
        };
        if !res.status.is_success() {
            return ListedModels { models: Vec::new(), error: Some(format!("models {}", res.status.as_u16())) };
        }
        match res.json_value().await {
            Ok(json) => ListedModels { models: openai_model_list(&json), error: None },
            Err(e) => ListedModels { models: Vec::new(), error: Some(e.to_string()) },
        }
    }

    fn has_count_tokens(&self) -> bool {
        self.count_tokens_url.is_some()
    }

    /// The operator's pointer to a real Anthropic-shaped count_tokens endpoint — posted to
    /// verbatim, nothing appended. Same key as chat_completions, sent both ways (Bearer +
    /// x-api-key) since the target speaks Anthropic while the key was entered as an OpenAI
    /// key. No effort-remap retry here — that recovery is chat_completions()-only.
    async fn count_tokens(
        &self,
        cx: &AppState,
        account: &AcquiredAccount,
        body: &Value,
        headers: &HeaderMap,
        extras: &CallExtras,
    ) -> Result<Response, AdapterError> {
        let Some(url) = self.count_tokens_url.as_deref() else {
            return Err(AdapterError::Unsupported("count_tokens"));
        };
        let token = &account.credential.access_token;
        let raw = object_or_empty(body);
        let mut request = UpstreamRequest::post(url)
            .header("authorization", &format!("Bearer {token}"))
            .header("x-api-key", token)
            .json(&Value::Object(raw))
            .header("anthropic-version", header_or(headers, "anthropic-version", DEFAULT_ANTHROPIC_VERSION));
        if let Some(beta) = non_empty_header(headers, "anthropic-beta") {
            request = request.header("anthropic-beta", beta);
        }
        if let Some(timeout) = extras.first_byte_timeout {
            request = request.timeout(timeout);
        }
        Ok(into_response(self.transport(cx).send(request).await?))
    }
}

/// `{...req.rawBody, model, stream_options}` — `serde_json`'s `preserve_order` keeps the
/// JavaScript spread's key order, an existing key staying in place.
fn outgoing_body(req: &ChatCompletionRequest) -> Map<String, Value> {
    let mut body = req.raw_body.clone();
    body.insert("model".into(), Value::String(req.upstream_model.clone()));
    // TabbyAPI reports usage only when this flag is set, for stream and non-stream alike.
    // Merge so a client stream_options object is not wholesale overwritten; force
    // include_usage even if they sent false.
    let mut stream_options = match body.get("stream_options") {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    };
    stream_options.insert("include_usage".into(), Value::Bool(true));
    body.insert("stream_options".into(), Value::Object(stream_options));
    body
}

pub(crate) fn object_or_empty(body: &Value) -> Map<String, Value> {
    match body {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    }
}

fn post_json(url: &str, token: &str, body: &Map<String, Value>, extras: &CallExtras) -> UpstreamRequest {
    let mut request = UpstreamRequest::post(url)
        .json(&Value::Object(body.clone()))
        .header("authorization", &format!("Bearer {token}"));
    if let Some(timeout) = extras.first_byte_timeout {
        request = request.timeout(timeout);
    }
    request
}

pub(crate) fn header_or<'a>(headers: &'a HeaderMap, name: &str, fallback: &'a str) -> &'a str {
    non_empty_header(headers, name).unwrap_or(fallback)
}

pub(crate) fn non_empty_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok()).filter(|v| !v.is_empty())
}

fn is_event_stream(res: &UpstreamResponse) -> bool {
    res.content_type().unwrap_or("").contains("text/event-stream")
}

/// `{ data: [{ id, display_name? }] }` → the adapter's model list, non-string/empty ids
/// dropped.
pub(crate) fn model_list(json: &Value, with_display_name: bool) -> Vec<UpstreamModel> {
    json.get("data")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|m| {
                    let id = m.get("id").and_then(Value::as_str).filter(|id| !id.is_empty())?;
                    let display_name = if with_display_name {
                        m.get("display_name").and_then(Value::as_str).map(str::to_string)
                    } else {
                        None
                    };
                    Some(UpstreamModel { id: id.to_string(), display_name })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn openai_model_list(json: &Value) -> Vec<UpstreamModel> {
    model_list(json, false)
}

/// The upstream response as the client's response: status, every header, and the body still
/// streaming (docs/api.md § Streaming — never buffered whole).
pub(crate) fn into_response(res: UpstreamResponse) -> Response {
    let mut builder = Response::builder().status(res.status);
    if let Some(headers) = builder.headers_mut() {
        *headers = res.headers;
    }
    builder.body(Body::from_stream(res.body)).expect("response builds")
}

/// `new Response(text, { status, headers })` — the rebuild that keeps every upstream header,
/// so a CLI tunnel's `x-agent-fault` marker survives (docs/cli.md).
pub(crate) fn rebuild(status: StatusCode, headers: HeaderMap, body: Bytes) -> Response {
    let mut builder = Response::builder().status(status);
    if let Some(h) = builder.headers_mut() {
        *h = headers;
    }
    builder.body(Body::from(body)).expect("response builds")
}

/// One multipart/form-data body carrying the client's fields with `model` rewritten to the
/// bare upstream id, plus the audio file itself under its original filename.
fn encode_multipart(form: &AudioForm, upstream_model: &str) -> (String, Bytes) {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    let boundary = format!("----kano-proxy-{}", hex::encode(bytes));
    let mut out: Vec<u8> = Vec::new();
    let mut wrote_model = false;
    for (key, value) in &form.fields {
        let value = if key == "model" {
            wrote_model = true;
            upstream_model
        } else {
            value.as_str()
        };
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        out.extend_from_slice(format!("content-disposition: form-data; name=\"{}\"\r\n\r\n", escape(key)).as_bytes());
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    out.extend_from_slice(
        format!(
            "content-disposition: form-data; name=\"file\"; filename=\"{}\"\r\ncontent-type: {}\r\n\r\n",
            escape(&form.file_name),
            form.file_content_type
        )
        .as_bytes(),
    );
    out.extend_from_slice(&form.file);
    out.extend_from_slice(b"\r\n");
    if !wrote_model {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        out.extend_from_slice(b"content-disposition: form-data; name=\"model\"\r\n\r\n");
        out.extend_from_slice(upstream_model.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), Bytes::from(out))
}

/// `FormData` field names and filenames are quoted-string values; a literal quote or newline
/// would break the framing, so they are escaped as `encodeURIComponent`-style percent bytes.
fn escape(value: &str) -> String {
    value.replace('\\', "%5C").replace('"', "%22").replace(['\r', '\n'], "%0A")
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::db::accounts::AccountRow;
    use crate::pool::StoredCredential;

    pub fn custom_row(format: &str, base_url: &str) -> CustomProviderRow {
        CustomProviderRow {
            id: "cprov_1".into(),
            user_id: "user_1".into(),
            slug: if format == "openai" { "my-endpoint".into() } else { "my-claude".into() },
            name: "My Endpoint".into(),
            format: format.into(),
            base_url: base_url.into(),
            count_tokens_url: None,
            models_mode: "auto".into(),
            manual_models_json: None,
            sort_order: 0,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
        }
    }

    pub fn account(token: &str) -> AcquiredAccount {
        AcquiredAccount {
            row: AccountRow {
                id: "acc_1".into(),
                user_id: "user_1".into(),
                provider: "my-endpoint".into(),
                external_account_id: None,
                label: None,
                custom_label: None,
                priority: 1,
                encrypted_payload: String::new(),
                account_meta_json: None,
                usage_snapshot_json: None,
                usage_fetched_at: None,
                usage_fetching_at: None,
                bench_until: None,
                bench_reason: None,
                refreshing_at: None,
                edge_strikes: 0,
                edge_strike_at: None,
                created_at: "2026-01-01T00:00:00.000Z".into(),
                updated_at: "2026-01-01T00:00:00.000Z".into(),
            },
            credential: StoredCredential { access_token: token.into(), ..Default::default() },
        }
    }

    /// An [`AppState`] whose only upstream is `transport`; its pool is lazy and never dialed,
    /// so an adapter test needs no database.
    pub fn adapter_state(transport: Arc<dyn UpstreamTransport>) -> AppState {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused@127.0.0.1/unused")
            .expect("a lazy pool never dials");
        AppState::builder(crate::db::test_support::test_config(), pool).transport(transport).build()
    }

    /// An [`AppState`] with a lazy pool, `transport`, and the given tunnel registry.
    pub fn tunnel_state(tunnels: crate::tunnel::registry::TunnelRegistry) -> AppState {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused@127.0.0.1/unused")
            .expect("a lazy pool never dials");
        AppState::builder(crate::db::test_support::test_config(), pool).tunnels(tunnels).build()
    }

    pub fn chat_request(raw_body: Value, upstream_model: &str) -> ChatCompletionRequest {
        let model = raw_body.get("model").and_then(Value::as_str).unwrap_or_default().to_string();
        ChatCompletionRequest {
            model: model.clone(),
            raw_model: model,
            upstream_model: upstream_model.into(),
            messages: raw_body.get("messages").and_then(Value::as_array).cloned().unwrap_or_default(),
            stream: raw_body.get("stream").and_then(Value::as_bool),
            raw_body: object_or_empty(&raw_body),
            ..Default::default()
        }
    }

    pub async fn body_text(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body reads");
        String::from_utf8_lossy(&bytes).into_owned()
    }

    pub async fn body_json(response: Response) -> Value {
        serde_json::from_str(&body_text(response).await).expect("json body")
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::upstream::transport::{MockTransport, TransportError};
    use serde_json::json;

    const BASE: &str = "https://upstream.example.com/v1";
    const KEY: &str = "sk-test-upstream-key";

    const TABBY_400: &str =
        "TemplateError: Unexpected reasoning effort high. Supported types are xhigh (default), medium, and low.";

    fn adapter() -> CustomOpenAiAdapter {
        CustomOpenAiAdapter::new(custom_row("openai", BASE))
    }

    fn adapter_with_count_tokens() -> CustomOpenAiAdapter {
        let mut row = custom_row("openai", BASE);
        row.count_tokens_url = Some("https://count.example.com/anthropic/count_tokens".into());
        CustomOpenAiAdapter::new(row)
    }

    fn tabby_detail() -> Value {
        json!({ "detail": TABBY_400 })
    }

    #[tokio::test]
    async fn has_no_messages_count_tokens_usage_or_refresh() {
        let adapter = adapter();
        assert!(!adapter.has_messages());
        assert!(!adapter.has_count_tokens());
        assert!(!adapter.has_fetch_usage());
        assert_eq!(adapter.id(), "my-endpoint");
        // The default refresh_if_needed returns the account unchanged (no OAuth flow).
        let transport = MockTransport::new();
        let state = adapter_state(transport);
        let same = adapter.refresh_if_needed(&state, account(KEY)).await.expect("no refresh");
        assert_eq!(same.credential.access_token, KEY);
    }

    #[tokio::test]
    async fn posts_to_base_chat_completions_with_a_bearer_header() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "ok": true }));
        let state = adapter_state(transport.clone());
        adapter()
            .chat_completions(
                &state,
                &account(KEY),
                &chat_request(json!({ "model": "my-endpoint/gpt-4o", "messages": [{ "role": "user", "content": "hi" }] }), "gpt-4o"),
                &CallExtras::default(),
            )
            .await
            .expect("chat completions");

        let sent = &transport.requests()[0];
        assert_eq!(sent.url, "https://upstream.example.com/v1/chat/completions");
        assert_eq!(sent.method, http::Method::POST);
        assert_eq!(sent.header("authorization"), Some("Bearer sk-test-upstream-key"));
        assert_eq!(sent.header("content-type"), Some("application/json"));
    }

    #[tokio::test]
    async fn rewrites_model_to_the_bare_upstream_id() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({}));
        let state = adapter_state(transport.clone());
        adapter()
            .chat_completions(
                &state,
                &account(KEY),
                &chat_request(json!({ "model": "my-endpoint/gpt-4o", "messages": [] }), "gpt-4o"),
                &CallExtras::default(),
            )
            .await
            .expect("chat completions");
        assert_eq!(transport.requests()[0].json()["model"], "gpt-4o");
    }

    #[tokio::test]
    async fn forwards_the_client_body_verbatim_including_temperature_and_reasoning_effort() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({}));
        let state = adapter_state(transport.clone());
        let client_body = json!({
            "model": "my-endpoint/gpt-4o",
            "messages": [{ "role": "user", "content": "hi" }],
            "temperature": 0.7,
            "reasoning_effort": "high",
            "response_format": { "type": "json_object" },
            "top_p": 0.9,
            "seed": 42,
        });
        adapter()
            .chat_completions(&state, &account(KEY), &chat_request(client_body, "gpt-4o"), &CallExtras::default())
            .await
            .expect("chat completions");

        assert_eq!(
            transport.requests()[0].json(),
            json!({
                "model": "gpt-4o",
                "messages": [{ "role": "user", "content": "hi" }],
                "temperature": 0.7,
                "reasoning_effort": "high",
                "response_format": { "type": "json_object" },
                "top_p": 0.9,
                "seed": 42,
                "stream_options": { "include_usage": true },
            })
        );
    }

    #[tokio::test]
    async fn always_sets_stream_options_include_usage_on_non_stream() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({}));
        let state = adapter_state(transport.clone());
        adapter()
            .chat_completions(
                &state,
                &account(KEY),
                &chat_request(json!({ "model": "my-endpoint/gpt-4o", "messages": [] }), "gpt-4o"),
                &CallExtras::default(),
            )
            .await
            .expect("chat completions");
        assert_eq!(transport.requests()[0].json()["stream_options"], json!({ "include_usage": true }));
    }

    #[tokio::test]
    async fn always_sets_stream_options_include_usage_on_stream() {
        let transport = MockTransport::new();
        transport.respond_sse(StatusCode::OK, "data: [DONE]\n\n");
        let state = adapter_state(transport.clone());
        adapter()
            .chat_completions(
                &state,
                &account(KEY),
                &chat_request(json!({ "model": "my-endpoint/gpt-4o", "messages": [], "stream": true }), "gpt-4o"),
                &CallExtras::default(),
            )
            .await
            .expect("chat completions");
        let body = transport.requests()[0].json();
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"], json!({ "include_usage": true }));
    }

    #[tokio::test]
    async fn merges_client_stream_options_and_forces_include_usage_true() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({}));
        let state = adapter_state(transport.clone());
        adapter()
            .chat_completions(
                &state,
                &account(KEY),
                &chat_request(
                    json!({
                        "model": "my-endpoint/gpt-4o",
                        "messages": [],
                        "stream_options": { "include_usage": false, "some_other_flag": true },
                    }),
                    "gpt-4o",
                ),
                &CallExtras::default(),
            )
            .await
            .expect("chat completions");
        assert_eq!(
            transport.requests()[0].json()["stream_options"],
            json!({ "include_usage": true, "some_other_flag": true })
        );
    }

    #[tokio::test]
    async fn pipes_an_sse_response_through_untouched() {
        let transport = MockTransport::new();
        transport.respond_sse(StatusCode::OK, "data: {\"delta\":\"hi\"}\n\n");
        let state = adapter_state(transport.clone());
        let res = adapter()
            .chat_completions(
                &state,
                &account(KEY),
                &chat_request(json!({ "model": "my-endpoint/gpt-4o", "messages": [], "stream": true }), "gpt-4o"),
                &CallExtras::default(),
            )
            .await
            .expect("chat completions");

        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers().get("content-type").unwrap(), "text/event-stream");
        assert!(body_text(res).await.contains("\"delta\":\"hi\""));
        assert_eq!(transport.pending(), 0);
    }

    #[tokio::test]
    async fn remaps_a_tabby_unsupported_effort_400_and_retries_once_on_the_same_account() {
        let transport = MockTransport::new();
        transport.expect(move |_| Ok(UpstreamResponse::json(StatusCode::BAD_REQUEST, &tabby_detail())));
        transport.respond_json(StatusCode::OK, json!({ "ok": true }));
        let state = adapter_state(transport.clone());
        let client_body = json!({
            "model": "my-endpoint/qwen",
            "messages": [{ "role": "user", "content": "hi" }],
            "temperature": 0.7,
            "reasoning_effort": "high",
            "response_format": { "type": "json_object" },
        });
        let res = adapter()
            .chat_completions(&state, &account(KEY), &chat_request(client_body, "qwen"), &CallExtras::default())
            .await
            .expect("chat completions");

        let requests = transport.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].url, "https://upstream.example.com/v1/chat/completions");
        assert_eq!(requests[1].url, requests[0].url);
        assert_eq!(requests[0].header("authorization"), Some("Bearer sk-test-upstream-key"));
        assert_eq!(requests[1].header("authorization"), requests[0].header("authorization"));

        let first = requests[0].json();
        assert_eq!(
            first,
            json!({
                "model": "qwen",
                "messages": [{ "role": "user", "content": "hi" }],
                "temperature": 0.7,
                "reasoning_effort": "high",
                "response_format": { "type": "json_object" },
                "stream_options": { "include_usage": true },
            })
        );
        let mut expected = first.clone();
        expected["reasoning_effort"] = json!("xhigh");
        assert_eq!(requests[1].json(), expected);
        assert_eq!(body_json(res).await, json!({ "ok": true }));
    }

    #[tokio::test]
    async fn keeps_merged_stream_options_on_the_remapped_retry_post() {
        let transport = MockTransport::new();
        transport.expect(move |_| Ok(UpstreamResponse::json(StatusCode::BAD_REQUEST, &tabby_detail())));
        transport.respond_json(StatusCode::OK, json!({}));
        let state = adapter_state(transport.clone());
        adapter()
            .chat_completions(
                &state,
                &account(KEY),
                &chat_request(
                    json!({
                        "model": "my-endpoint/qwen",
                        "messages": [],
                        "reasoning_effort": "high",
                        "stream_options": { "include_usage": false, "some_other_flag": true },
                    }),
                    "qwen",
                ),
                &CallExtras::default(),
            )
            .await
            .expect("chat completions");

        let requests = transport.requests();
        assert_eq!(requests.len(), 2);
        let merged = json!({ "include_usage": true, "some_other_flag": true });
        assert_eq!(requests[0].json()["stream_options"], merged);
        assert_eq!(requests[1].json()["stream_options"], merged);
        assert_eq!(requests[1].json()["reasoning_effort"], "xhigh");
    }

    #[tokio::test]
    async fn returns_an_unrecognized_400_unchanged_after_one_send() {
        let original = r#"{"error":"context length exceeded"}"#;
        let transport = MockTransport::new();
        transport.expect(move |_| {
            let mut headers = HeaderMap::new();
            headers.insert("content-type", http::HeaderValue::from_static("application/json"));
            headers.insert("x-upstream", http::HeaderValue::from_static("yes"));
            Ok(UpstreamResponse::from_bytes(StatusCode::BAD_REQUEST, headers, original))
        });
        let state = adapter_state(transport.clone());
        let res = adapter()
            .chat_completions(
                &state,
                &account(KEY),
                &chat_request(json!({ "model": "my-endpoint/gpt-4o", "messages": [], "reasoning_effort": "high" }), "gpt-4o"),
                &CallExtras::default(),
            )
            .await
            .expect("chat completions");

        assert_eq!(transport.requests().len(), 1);
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(res.headers().get("content-type").unwrap(), "application/json");
        assert_eq!(res.headers().get("x-upstream").unwrap(), "yes");
        assert_eq!(body_text(res).await, original);
    }

    #[tokio::test]
    async fn returns_a_retry_non_2xx_instead_of_the_original_400() {
        let transport = MockTransport::new();
        transport.expect(move |_| Ok(UpstreamResponse::json(StatusCode::BAD_REQUEST, &tabby_detail())));
        transport.expect(|_| {
            Ok(UpstreamResponse::from_bytes(
                StatusCode::UNPROCESSABLE_ENTITY,
                HeaderMap::new(),
                r#"{"error":"still rejected"}"#,
            ))
        });
        let state = adapter_state(transport.clone());
        let res = adapter()
            .chat_completions(
                &state,
                &account(KEY),
                &chat_request(json!({ "model": "my-endpoint/qwen", "messages": [], "reasoning_effort": "high" }), "qwen"),
                &CallExtras::default(),
            )
            .await
            .expect("chat completions");

        assert_eq!(transport.requests().len(), 2);
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body_text(res).await, r#"{"error":"still rejected"}"#);
    }

    #[tokio::test]
    async fn does_not_retry_when_the_parser_does_not_recognize_the_400() {
        let transport = MockTransport::new();
        transport.expect(|_| {
            Ok(UpstreamResponse::from_bytes(StatusCode::BAD_REQUEST, HeaderMap::new(), r#"{"error":"nope"}"#))
        });
        let state = adapter_state(transport.clone());
        let res = adapter()
            .chat_completions(
                &state,
                &account(KEY),
                &chat_request(json!({ "model": "my-endpoint/gpt-4o", "messages": [], "reasoning_effort": "high" }), "gpt-4o"),
                &CallExtras::default(),
            )
            .await
            .expect("chat completions");

        assert_eq!(transport.requests().len(), 1);
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_text(res).await, r#"{"error":"nope"}"#);
    }

    #[tokio::test]
    async fn forwards_the_dispatch_deadline_on_both_posts() {
        let transport = MockTransport::new();
        transport.expect(move |_| Ok(UpstreamResponse::json(StatusCode::BAD_REQUEST, &tabby_detail())));
        transport.respond_json(StatusCode::OK, json!({}));
        let state = adapter_state(transport.clone());
        let timeout = std::time::Duration::from_millis(1234);
        adapter()
            .chat_completions(
                &state,
                &account(KEY),
                &chat_request(json!({ "model": "my-endpoint/qwen", "messages": [], "reasoning_effort": "high" }), "qwen"),
                &CallExtras { first_byte_timeout: Some(timeout), ..Default::default() },
            )
            .await
            .expect("chat completions");
        // `UpstreamRequest.first_byte_timeout` is the Rust seam for the TypeScript
        // `extras.signal`: both POSTs carry the dispatch deadline.
        assert_eq!(transport.requests().len(), 2);
    }

    #[tokio::test]
    async fn list_models_gets_base_models_with_a_bearer_header() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "data": [{ "id": "gpt-4o" }, { "id": "gpt-4o-mini" }] }));
        let state = adapter_state(transport.clone());
        let result = adapter().list_models(&state, &account(KEY)).await;

        let sent = &transport.requests()[0];
        assert_eq!(sent.url, "https://upstream.example.com/v1/models");
        assert_eq!(sent.method, http::Method::GET);
        assert_eq!(sent.header("authorization"), Some("Bearer sk-test-upstream-key"));
        assert_eq!(
            result.models,
            vec![
                UpstreamModel { id: "gpt-4o".into(), display_name: None },
                UpstreamModel { id: "gpt-4o-mini".into(), display_name: None },
            ]
        );
        assert_eq!(result.error, None);
    }

    #[tokio::test]
    async fn list_models_reports_an_error_string_on_non_2xx() {
        let transport = MockTransport::new();
        transport.expect(|_| Ok(UpstreamResponse::from_bytes(StatusCode::UNAUTHORIZED, HeaderMap::new(), "nope")));
        let state = adapter_state(transport);
        let result = adapter().list_models(&state, &account(KEY)).await;
        assert!(result.models.is_empty());
        assert_eq!(result.error.as_deref(), Some("models 401"));
    }

    #[tokio::test]
    async fn list_models_catches_a_transport_failure_instead_of_propagating() {
        let transport = MockTransport::new();
        transport.expect(|_| Err(TransportError::Connect("network down".into())));
        let state = adapter_state(transport);
        let result = adapter().list_models(&state, &account(KEY)).await;
        assert!(result.models.is_empty());
        assert!(result.error.unwrap().contains("network down"));
    }

    #[tokio::test]
    async fn audio_transcriptions_forwards_the_form_with_the_model_rewritten() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "text": "Transcription result test" }));
        let state = adapter_state(transport.clone());
        let form = AudioForm {
            fields: vec![
                ("model".into(), "openrouter/openai/whisper-large-v3-turbo".into()),
                ("language".into(), "en".into()),
                ("response_format".into(), "json".into()),
            ],
            file_name: "speech.wav".into(),
            file_content_type: "audio/wav".into(),
            file: Bytes::from_static(b"audio-bytes"),
        };
        let res = adapter()
            .audio_transcriptions(
                &state,
                &account(KEY),
                &form,
                "openrouter/openai/whisper-large-v3-turbo",
                "openai/whisper-large-v3-turbo",
                &CallExtras::default(),
            )
            .await
            .expect("audio transcriptions");

        let sent = &transport.requests()[0];
        assert_eq!(sent.url, "https://upstream.example.com/v1/audio/transcriptions");
        assert_eq!(sent.header("authorization"), Some("Bearer sk-test-upstream-key"));
        let content_type = sent.header("content-type").expect("content type");
        assert!(content_type.starts_with("multipart/form-data; boundary="));
        let body = String::from_utf8_lossy(sent.body.as_deref().expect("a body")).into_owned();
        assert!(body.contains("name=\"model\"\r\n\r\nopenai/whisper-large-v3-turbo\r\n"));
        assert!(!body.contains("openrouter/openai/whisper-large-v3-turbo"));
        assert!(body.contains("name=\"language\"\r\n\r\nen\r\n"));
        assert!(body.contains("name=\"response_format\"\r\n\r\njson\r\n"));
        assert!(body.contains("name=\"file\"; filename=\"speech.wav\"\r\ncontent-type: audio/wav\r\n\r\naudio-bytes\r\n"));
        assert_eq!(body_json(res).await, json!({ "text": "Transcription result test" }));
    }

    #[tokio::test]
    async fn audio_transcriptions_appends_the_model_when_the_client_sent_none() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "text": "" }));
        let state = adapter_state(transport.clone());
        let form = AudioForm {
            fields: vec![("response_format".into(), "srt".into())],
            file_name: "test.mp3".into(),
            file_content_type: "audio/mp3".into(),
            file: Bytes::from_static(b"audio-bytes"),
        };
        adapter()
            .audio_transcriptions(&state, &account(KEY), &form, "groq/whisper-large-v3", "whisper-large-v3", &CallExtras::default())
            .await
            .expect("audio transcriptions");
        let body = String::from_utf8_lossy(transport.requests()[0].body.as_deref().expect("a body")).into_owned();
        assert!(body.contains("name=\"model\"\r\n\r\nwhisper-large-v3\r\n"));
    }

    #[tokio::test]
    async fn audio_transcriptions_passes_a_plain_text_response_through() {
        let transport = MockTransport::new();
        transport.expect(|_| {
            let mut headers = HeaderMap::new();
            headers.insert("content-type", http::HeaderValue::from_static("text/plain; charset=utf-8"));
            Ok(UpstreamResponse::from_bytes(
                StatusCode::OK,
                headers,
                "1\n00:00:00,000 --> 00:00:02,000\nHello world\n",
            ))
        });
        let state = adapter_state(transport);
        let form = AudioForm {
            fields: vec![("model".into(), "groq/whisper-large-v3".into())],
            file_name: "test.mp3".into(),
            file_content_type: "audio/mp3".into(),
            file: Bytes::from_static(b"audio-bytes"),
        };
        let res = adapter()
            .audio_transcriptions(&state, &account(KEY), &form, "groq/whisper-large-v3", "whisper-large-v3", &CallExtras::default())
            .await
            .expect("audio transcriptions");
        assert_eq!(res.headers().get("content-type").unwrap(), "text/plain; charset=utf-8");
        assert!(body_text(res).await.contains("00:00:00,000 --> 00:00:02,000"));
    }

    // -----------------------------------------------------------------------
    // countTokens

    #[tokio::test]
    async fn has_no_count_tokens_without_a_count_tokens_url() {
        assert!(!adapter().has_count_tokens());
        assert!(adapter_with_count_tokens().has_count_tokens());
    }

    #[tokio::test]
    async fn count_tokens_posts_to_the_exact_stored_url_verbatim_with_both_auth_headers() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "input_tokens": 12 }));
        let state = adapter_state(transport.clone());
        let body = json!({ "model": "gpt-4o", "messages": [{ "role": "user", "content": "hi" }] });
        let res = adapter_with_count_tokens()
            .count_tokens(&state, &account(KEY), &body, &HeaderMap::new(), &CallExtras::default())
            .await
            .expect("count tokens");

        let sent = &transport.requests()[0];
        assert_eq!(sent.url, "https://count.example.com/anthropic/count_tokens");
        assert_eq!(sent.method, http::Method::POST);
        assert_eq!(sent.header("authorization"), Some("Bearer sk-test-upstream-key"));
        assert_eq!(sent.header("x-api-key"), Some("sk-test-upstream-key"));
        assert_eq!(sent.header("content-type"), Some("application/json"));
        assert_eq!(sent.header("anthropic-version"), Some("2023-06-01"));
        assert_eq!(sent.header("anthropic-beta"), None);
        assert_eq!(sent.json(), body);
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await, json!({ "input_tokens": 12 }));
    }

    #[tokio::test]
    async fn count_tokens_forwards_a_client_anthropic_version_instead_of_the_default() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({}));
        let state = adapter_state(transport.clone());
        let mut headers = HeaderMap::new();
        headers.insert("anthropic-version", http::HeaderValue::from_static("2024-10-01"));
        adapter_with_count_tokens()
            .count_tokens(&state, &account(KEY), &json!({}), &headers, &CallExtras::default())
            .await
            .expect("count tokens");
        assert_eq!(transport.requests()[0].header("anthropic-version"), Some("2024-10-01"));
    }

    #[tokio::test]
    async fn count_tokens_forwards_anthropic_beta_verbatim() {
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({}));
        let state = adapter_state(transport.clone());
        let mut headers = HeaderMap::new();
        headers.insert("anthropic-beta", http::HeaderValue::from_static("some-beta-2025-01-01"));
        adapter_with_count_tokens()
            .count_tokens(&state, &account(KEY), &json!({}), &headers, &CallExtras::default())
            .await
            .expect("count tokens");
        assert_eq!(transport.requests()[0].header("anthropic-beta"), Some("some-beta-2025-01-01"));
    }

    #[tokio::test]
    async fn count_tokens_returns_the_upstream_response_untouched() {
        let transport = MockTransport::new();
        transport.expect(|_| {
            let mut headers = HeaderMap::new();
            headers.insert("x-upstream", http::HeaderValue::from_static("yes"));
            Ok(UpstreamResponse::from_bytes(StatusCode::OK, headers, r#"{"input_tokens":42}"#))
        });
        let state = adapter_state(transport);
        let res = adapter_with_count_tokens()
            .count_tokens(&state, &account(KEY), &json!({}), &HeaderMap::new(), &CallExtras::default())
            .await
            .expect("count tokens");
        assert_eq!(res.headers().get("x-upstream").unwrap(), "yes");
        assert_eq!(body_json(res).await, json!({ "input_tokens": 42 }));
    }
}
