//! Port of apps/api/src/proxy/dispatch_audio.ts (docs/api.md § Audio).
//!
//! `/openai/v1/audio/transcriptions` dispatch: the shared candidate walk via the non-stream
//! transport, with audio's own delivery — the response is passed through with every upstream
//! header and status, usage is tapped from a bounded prefix of a JSON body, and a non-2xx
//! pass-through is logged as `upstream_error`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::response::Response;
use bytes::Bytes;
use futures::StreamExt;
use http::header;

use crate::db::custom_providers::CustomProviderRow;
use crate::logging::request_log::{log_request, LogEntry};
use crate::logging::usage_capture::{from_openai_usage, NormalizedUsage, UsageSniffer};
use crate::pool::extension::{borrower_safe_headers, settle_lease, LeaseOutcome};
use crate::providers::types::{AdapterError, AudioForm, CallExtras, DynAdapter};
use crate::proxy::backpressure::backpressured_byte_stream;
use crate::proxy::dispatch::{
    body_stream, canonical_model_id, dispatch_non_stream_with, is_event_stream, lease_outcome,
    passthrough_stream_headers, stream_close_error_code, Delivery, NonStreamDelivery, TransportOpts,
    DEFAULT_IDLE_TIMEOUT,
};
use crate::proxy::dispatch_walk::{CandidateCaller, CandidateSource};
use crate::proxy::sse::{
    stream_with_keepalive, StreamCloseReason, StreamKeepaliveOpts, DEFAULT_KEEPALIVE_INTERVAL,
};
use crate::proxy::wire::Wire;
use crate::proxy::wire_openai::OpenAiWire;
use crate::routing::types::RoutingCandidate;
use crate::upstream::transport::ByteStream;
use crate::AppState;

/// At most this much of a JSON body is buffered to read `usage` from once it ends; the bytes
/// themselves still pass through untouched.
const MAX_USAGE_TAP_BYTES: usize = 256 * 1024;

struct AudioCaller {
    form: AudioForm,
    raw_model: String,
}

#[async_trait]
impl CandidateCaller for AudioCaller {
    fn supports(&self, candidate: &RoutingCandidate) -> bool {
        candidate.adapter.has_audio_transcriptions()
    }
    async fn call(
        &self,
        cx: &AppState,
        candidate: &RoutingCandidate,
        acquired: &crate::pool::AcquiredAccount,
        extras: &CallExtras,
    ) -> Result<Response, AdapterError> {
        candidate
            .adapter
            .audio_transcriptions(cx, acquired, &self.form, &self.raw_model, &candidate.upstream_model, extras)
            .await
    }
}

/// Options for [`dispatch_audio_transcriptions`].
pub struct AudioDispatchOptions {
    pub user_id: String,
    pub api_key_id: Option<String>,
    pub provider: String,
    pub adapter: Option<DynAdapter>,
    pub form: AudioForm,
    pub raw_model: String,
    pub upstream_model: String,
    pub idle_timeout: Option<Duration>,
    pub group_name: Option<String>,
    pub pinned_account_id: Option<String>,
    pub candidates: Option<Vec<RoutingCandidate>>,
    pub strategy: Option<String>,
    pub is_builtin: Option<bool>,
    pub custom_provider: Option<CustomProviderRow>,
}

impl Default for AudioDispatchOptions {
    fn default() -> Self {
        Self {
            user_id: String::new(),
            api_key_id: None,
            provider: String::new(),
            adapter: None,
            form: AudioForm {
                fields: Vec::new(),
                file_name: String::new(),
                file_content_type: String::new(),
                file: Bytes::new(),
            },
            raw_model: String::new(),
            upstream_model: String::new(),
            idle_timeout: None,
            group_name: None,
            pinned_account_id: None,
            candidates: None,
            strategy: None,
            is_builtin: None,
            custom_provider: None,
        }
    }
}

pub async fn dispatch_audio_transcriptions(cx: &AppState, opts: AudioDispatchOptions) -> Response {
    let idle_timeout = opts.idle_timeout.unwrap_or(DEFAULT_IDLE_TIMEOUT);
    let transport = Arc::new(TransportOpts {
        user_id: opts.user_id.clone(),
        api_key_id: opts.api_key_id.clone(),
        group_name: opts.group_name.clone(),
        idle_timeout: Some(idle_timeout),
        requested_provider: opts.provider.clone(),
        requested_model: canonical_model_id(&opts.provider, &opts.upstream_model),
        source: CandidateSource {
            user_id: opts.user_id.clone(),
            api_key_id: opts.api_key_id.clone(),
            provider: opts.provider.clone(),
            adapter: opts.adapter.clone(),
            pinned_account_id: opts.pinned_account_id.clone(),
            candidates: opts.candidates.clone(),
            strategy: opts.strategy.clone(),
            is_builtin: opts.is_builtin,
            custom_provider: opts.custom_provider.clone(),
        },
        upstream_model: opts.upstream_model.clone(),
        caller: Arc::new(AudioCaller { form: opts.form.clone(), raw_model: opts.raw_model.clone() }),
        wire: Arc::new(OpenAiWire),
        capture_usage: true,
    });
    dispatch_non_stream_with(cx, transport, &AudioDelivery { idle_timeout }).await
}

struct AudioDelivery {
    idle_timeout: Duration,
}

#[async_trait]
impl NonStreamDelivery for AudioDelivery {
    async fn deliver(&self, cx: &AppState, t: &Arc<TransportOpts>, delivery: Delivery) -> Response {
        let Delivery { candidate, response, latency_ms, lease, started_at_ms } = delivery;
        let status = response.status();
        let error_code: Option<String> =
            if status.is_success() { None } else { Some("upstream_error".to_string()) };
        let event_stream = is_event_stream(&response);
        let is_json = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("application/json"));
        let shared = candidate.share.is_some();

        let entry = LogEntry {
            started_at_ms: Some(started_at_ms),
            user_id: t.user_id.clone(),
            api_key_id: t.api_key_id.clone(),
            group_name: t.group_name.clone(),
            provider: candidate.provider.clone(),
            model: canonical_model_id(&candidate.provider, &candidate.upstream_model),
            account_id: Some(candidate.account.id.clone()),
            status_code: status.as_u16() as i32,
            latency_ms,
            upstream_status: Some(status.as_u16() as i32),
            ..Default::default()
        };

        // Whether any response byte reached the client — the same rule the other transports use.
        let saw_output = Arc::new(AtomicBool::new(false));
        let (parts, body) = response.into_parts();

        // An upstream error status passed through carries no useful output whatever its body
        // says; otherwise transcription text that already reached the client is consumed, even
        // if the connection then broke. The settlement and the row must outlive the client
        // connection, exactly as `waitUntil` kept them alive.
        let log = {
            let cx = cx.clone();
            let saw_output = saw_output.clone();
            let lease = Arc::new(Mutex::new(lease));
            let upstream_status = status.as_u16();
            Arc::new(move |usage: Option<NormalizedUsage>, code: Option<String>| {
                let usage = usage.unwrap_or_default();
                let mut entry = entry.clone();
                entry.error_code = code;
                entry.prompt_tokens = usage.prompt_tokens;
                entry.completion_tokens = usage.completion_tokens;
                entry.cache_read_input_tokens = usage.cache_read_input_tokens;
                entry.cache_creation_input_tokens = usage.cache_creation_input_tokens;
                let outcome = if upstream_status >= 400 {
                    LeaseOutcome::Released
                } else {
                    lease_outcome(entry.error_code.as_deref(), saw_output.load(Ordering::SeqCst))
                };
                let lease = lease.lock().unwrap_or_else(|e| e.into_inner()).take();
                let cx = cx.clone();
                tokio::spawn(async move {
                    settle_lease(lease.as_deref(), outcome).await;
                    log_request(&cx, entry).await;
                });
            })
        };

        if event_stream {
            let sniffer: Arc<Mutex<Box<dyn UsageSniffer>>> = Arc::new(Mutex::new(OpenAiWire.create_usage_sniffer()));
            let tap_sniffer = sniffer.clone();
            let tap_saw_output = saw_output.clone();
            let close_code = error_code.clone();
            let close_log = log.clone();
            let stream = stream_with_keepalive(
                Box::pin(body_stream(body)),
                DEFAULT_KEEPALIVE_INTERVAL,
                StreamKeepaliveOpts {
                    tap: Some(Arc::new(move |chunk: &Bytes| {
                        tap_saw_output.store(true, Ordering::SeqCst);
                        tap_sniffer.lock().unwrap_or_else(|e| e.into_inner()).feed(chunk);
                    })),
                    on_close: Some(Box::new(move |reason: StreamCloseReason| {
                        let (usage, complete) = {
                            let s = sniffer.lock().unwrap_or_else(|e| e.into_inner());
                            (s.finish(), s.complete())
                        };
                        let code = close_code.clone().or_else(|| stream_close_error_code(reason, complete));
                        close_log(usage, code);
                    })),
                    idle_timeout: Some(self.idle_timeout),
                    stall_frame: Some(OpenAiWire.stall_frame()),
                    error_frame: None,
                },
            );
            let mut res = Response::builder().status(parts.status).body(Body::from_stream(stream)).expect("builds");
            *res.headers_mut() = passthrough_stream_headers(&parts.headers, shared);
            return res;
        }

        // Audio passes every upstream header through; a borrowed row drops the owner's
        // rate-limit/identity ones first (docs/cloud-edition.md § "Pool extension").
        let headers = if shared { borrower_safe_headers(&parts.headers) } else { parts.headers.clone() };
        let tap_saw_output = saw_output.clone();
        let tapped = stream_with_usage_tap(
            Box::pin(body_stream(body)),
            is_json,
            move |usage, reason| {
                let code = match reason {
                    StreamCloseReason::Cancel => Some("client_abort".to_string()),
                    StreamCloseReason::Error => Some("upstream_error".to_string()),
                    _ => error_code.clone(),
                };
                log(usage, code);
            },
            move || tap_saw_output.store(true, Ordering::SeqCst),
        );
        let mut res = Response::builder().status(parts.status).body(Body::from_stream(tapped)).expect("builds");
        *res.headers_mut() = headers;
        res
    }
}

/// Fires `on_finish` exactly once — including when the client drops the stream mid-body, which
/// aborts the pump and drops this with it (`cancel` in the TypeScript reader).
struct FinishGuard<F: FnOnce(Option<NormalizedUsage>, StreamCloseReason) + Send + 'static> {
    on_finish: Option<F>,
    buffered: Vec<u8>,
    reason: StreamCloseReason,
    is_json: bool,
}

impl<F: FnOnce(Option<NormalizedUsage>, StreamCloseReason) + Send + 'static> Drop for FinishGuard<F> {
    fn drop(&mut self) {
        let Some(on_finish) = self.on_finish.take() else { return };
        let mut usage = None;
        if self.is_json && !self.buffered.is_empty() {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&self.buffered) {
                usage = Some(from_openai_usage(value.get("usage")));
            }
        }
        on_finish(usage, self.reason);
    }
}

/// Passes the body through untouched while buffering at most 256 KiB of a JSON body, to read
/// `usage` from once it ends.
fn stream_with_usage_tap<F, C>(mut upstream: ByteStream, is_json: bool, on_finish: F, on_chunk: C) -> ByteStream
where
    F: FnOnce(Option<NormalizedUsage>, StreamCloseReason) + Send + 'static,
    C: Fn() + Send + 'static,
{
    backpressured_byte_stream(move |emitter| async move {
        let mut guard =
            FinishGuard { on_finish: Some(on_finish), buffered: Vec::new(), reason: StreamCloseReason::Cancel, is_json };
        loop {
            match upstream.next().await {
                None => {
                    guard.reason = StreamCloseReason::Done;
                    break;
                }
                Some(Err(error)) => {
                    guard.reason = StreamCloseReason::Error;
                    tracing::debug!(%error, "audio body failed mid-flight");
                    break;
                }
                Some(Ok(chunk)) => {
                    if emitter.enqueue(chunk.clone()).await.is_err() {
                        guard.reason = StreamCloseReason::Cancel;
                        break;
                    }
                    on_chunk();
                    if is_json && guard.buffered.len() < MAX_USAGE_TAP_BYTES {
                        guard.buffered.extend_from_slice(&chunk);
                    }
                }
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{insert_user, skip_without_db, test_pool, test_state};
    use crate::pool::AcquiredAccount;
    use crate::providers::types::ProviderAdapter;
    use crate::proxy::dispatch::tests::{body_text, logs, seed_accounts, wait_for_logs};
    use crate::upstream::MockTransport;
    use http::{HeaderValue, StatusCode};
    use serde_json::{json, Value};

    /// Answers one canned transcription response and records the model it was called with.
    struct AudioAdapter {
        status: StatusCode,
        body: String,
        content_type: &'static str,
        seen_model: Arc<Mutex<Option<String>>>,
    }

    #[async_trait]
    impl ProviderAdapter for AudioAdapter {
        fn id(&self) -> &str {
            "custom-audio"
        }
        async fn chat_completions(
            &self,
            _: &AppState,
            _: &AcquiredAccount,
            _: &crate::providers::types::ChatCompletionRequest,
            _: &CallExtras,
        ) -> Result<Response, AdapterError> {
            Err(AdapterError::Unsupported("chat_completions"))
        }
        fn has_audio_transcriptions(&self) -> bool {
            true
        }
        async fn audio_transcriptions(
            &self,
            _: &AppState,
            _: &AcquiredAccount,
            _: &AudioForm,
            _: &str,
            upstream_model: &str,
            _: &CallExtras,
        ) -> Result<Response, AdapterError> {
            *self.seen_model.lock().unwrap() = Some(upstream_model.to_string());
            let mut res = Response::builder()
                .status(self.status)
                .header(header::CONTENT_TYPE, self.content_type)
                .body(Body::from(self.body.clone()))
                .expect("builds");
            res.headers_mut().insert("x-request-id", HeaderValue::from_static("abc"));
            res
                .headers_mut()
                .insert("anthropic-ratelimit-tokens-remaining", HeaderValue::from_static("5"));
            Ok(res)
        }
    }

    fn form() -> AudioForm {
        AudioForm {
            fields: vec![("response_format".into(), "json".into())],
            file_name: "clip.wav".into(),
            file_content_type: "audio/wav".into(),
            file: Bytes::from_static(b"RIFF"),
        }
    }

    async fn run(status: StatusCode, body: Value, content_type: &'static str) -> Option<(AppState, Response, String)> {
        let pool = test_pool().await?;
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), &format!("audio-{}@example.com", crate::ids::new_id(""))).await;
        seed_accounts(state.pool(), &user.id, "custom-audio", &["acc_audio"]).await;
        let seen_model = Arc::new(Mutex::new(None));
        let adapter: DynAdapter = Arc::new(AudioAdapter {
            status,
            body: body.to_string(),
            content_type,
            seen_model: seen_model.clone(),
        });
        let res = dispatch_audio_transcriptions(
            &state,
            AudioDispatchOptions {
                user_id: user.id.clone(),
                provider: "custom-audio".into(),
                adapter: Some(adapter),
                is_builtin: Some(false),
                form: form(),
                raw_model: "custom-audio/whisper-1".into(),
                upstream_model: "whisper-1".into(),
                ..Default::default()
            },
        )
        .await;
        let model = seen_model.lock().unwrap().clone().expect("the adapter was called");
        Some((state, res, model))
    }

    #[tokio::test]
    async fn a_successful_transcription_passes_every_upstream_header_through_and_logs_its_usage() {
        let Some((state, res, model)) = run(
            StatusCode::OK,
            json!({ "text": "hello", "usage": { "prompt_tokens": 9, "completion_tokens": 3 } }),
            "application/json",
        )
        .await
        else {
            return skip_without_db();
        };
        assert_eq!(model, "whisper-1", "the candidate's own upstream id reaches the adapter");
        assert_eq!(res.status(), StatusCode::OK);
        // Audio passes every upstream header through, unlike the chat transports.
        assert_eq!(res.headers().get("x-request-id").unwrap(), "abc");
        let text = body_text(res).await;
        assert!(text.contains("hello"), "{text}");
        let rows = wait_for_logs(&state, 1).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provider, "custom-audio");
        assert_eq!(rows[0].model, "custom-audio/whisper-1");
        assert_eq!(rows[0].status_code, 200);
        assert_eq!(rows[0].error_code, None);
        assert_eq!(rows[0].prompt_tokens, Some(9));
        assert_eq!(rows[0].completion_tokens, Some(3));
    }

    #[tokio::test]
    async fn a_non_2xx_pass_through_is_logged_as_upstream_error_with_its_real_status() {
        let Some((state, res, _)) = run(
            StatusCode::BAD_REQUEST,
            json!({ "error": { "message": "bad audio" } }),
            "application/json",
        )
        .await
        else {
            return skip_without_db();
        };
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(body_text(res).await.contains("bad audio"));
        let rows = wait_for_logs(&state, 1).await;
        assert_eq!(rows[0].status_code, 400);
        assert_eq!(rows[0].upstream_status, Some(400));
        assert_eq!(rows[0].error_code.as_deref(), Some("upstream_error"));
        // A non-JSON-usage error body reports no tokens rather than zeroes.
        assert_eq!(rows[0].prompt_tokens, None);
    }

    #[tokio::test]
    async fn a_non_json_body_is_relayed_without_a_usage_read() {
        let Some((state, res, _)) = run(StatusCode::OK, json!("plain transcript"), "text/plain").await else {
            return skip_without_db();
        };
        assert_eq!(res.status(), StatusCode::OK);
        assert!(body_text(res).await.contains("plain transcript"));
        let rows = wait_for_logs(&state, 1).await;
        assert_eq!(rows[0].error_code, None);
        assert_eq!(rows[0].prompt_tokens, None);
    }

    #[tokio::test]
    async fn an_adapter_without_the_endpoint_contributes_no_attempt() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "audio-noendpoint@example.com").await;
        seed_accounts(state.pool(), &user.id, "custom-audio", &["acc_audio"]).await;
        // The default adapter has no `audio_transcriptions`, so the walk finds no attempt at all.
        let res = dispatch_audio_transcriptions(
            &state,
            AudioDispatchOptions {
                user_id: user.id.clone(),
                provider: "custom-audio".into(),
                adapter: Some(crate::routing::candidates::custom_provider_adapter(&CustomProviderRow {
                    id: "cprov".into(),
                    user_id: user.id.clone(),
                    slug: "custom-audio".into(),
                    name: "custom-audio".into(),
                    format: "anthropic".into(),
                    base_url: "https://upstream.example.com".into(),
                    count_tokens_url: None,
                    models_mode: "manual".into(),
                    manual_models_json: None,
                    sort_order: 0,
                    created_at: "2026-01-01T00:00:00.000Z".into(),
                    updated_at: "2026-01-01T00:00:00.000Z".into(),
                })),
                is_builtin: Some(false),
                form: form(),
                raw_model: "custom-audio/whisper-1".into(),
                upstream_model: "whisper-1".into(),
                ..Default::default()
            },
        )
        .await;
        // Every candidate skipped: the pool ran dry without a single attempt.
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(logs(&state).await[0].error_code.as_deref(), Some("upstream_unavailable"));
    }
}
