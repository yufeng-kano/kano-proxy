//! Port of apps/api/src/proxy/dispatch.ts (docs/api.md § Eager streaming commit, § Errors,
//! docs/logging.md, docs/cloud-edition.md § "Pool extension").
//!
//! Chat Completions and Anthropic Messages dispatch: two transports — eager streaming commit
//! and non-stream — over the single candidate walk in [`crate::proxy::dispatch_walk`], generic
//! over a [`Wire`] for everything protocol-specific. Exactly one `request_logs` row is written
//! per client request, at the point the outcome is decided: for a stream, when the stream
//! closes; the attempt's pool-extension lease settles on that same decision.
//!
//! `waitUntil` becomes `tokio::spawn`: the settlement and the log write outlive the client
//! connection, which is what makes a cancelled stream still produce its row.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::response::Response;
use bytes::Bytes;
use http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use serde_json::{Map, Value};

use crate::app::now_ms;
use crate::db::custom_providers::CustomProviderRow;
use crate::logging::request_log::{log_request, LogEntry};
use crate::logging::usage_capture::{NormalizedUsage, UsageSniffer};
use crate::pool::extension::{borrower_safe_headers, settle_lease, AttemptLease, LeaseOutcome};
use crate::providers::types::{AdapterError, CallExtras, ChatCompletionRequest, DynAdapter};
use crate::providers::ProviderId;
use crate::proxy::dispatch_walk::{
    recompute_unavailable_until, walk_candidates, CandidateCaller, CandidateSource, WalkOpts, WalkOutcome,
    WalkProgress,
};
use crate::proxy::sse::{
    stream_with_eager_producer, stream_with_keepalive, PipeOpts, StreamCloseReason, StreamKeepaliveOpts,
    DEFAULT_KEEPALIVE_INTERVAL,
};
use crate::proxy::wire::{NonStreamResponse, Wire};
use crate::proxy::wire_anthropic::AnthropicWire;
use crate::proxy::wire_openai::OpenAiWire;
use crate::routing::types::RoutingCandidate;
use crate::utils::model::split_model_id;
use crate::AppState;

/// `request_logs.model`/`provider` always store the expanded canonical target, never a
/// model-group alias — reconstructed the same way `split_model_id` builds `raw`.
pub fn canonical_model_id(provider: &str, upstream_model: &str) -> String {
    format!("{provider}/{upstream_model}")
}

/// No real upstream chunk for this long tears the stream down (docs/api.md § Streaming).
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_millis(120_000);

/// `request_logs.error_code` for a streamed response, from how it closed and whether the
/// sniffer ever saw the upstream's documented completion signal (docs/logging.md § Streaming
/// rows). An idle-timeout close is always `upstream_stall` regardless of completeness (the
/// connection was abnormal even if, by coincidence, a full payload had already arrived);
/// anything else that reached completion is unchanged (NULL); a client cancel before completion
/// is `client_abort`; any other close before completion — a clean EOF with no completion
/// signal, or a transport error — is `incomplete_stream`.
pub fn stream_close_error_code(reason: StreamCloseReason, complete: bool) -> Option<String> {
    match reason {
        StreamCloseReason::IdleTimeout => Some("upstream_stall".to_string()),
        _ if complete => None,
        StreamCloseReason::Cancel => Some("client_abort".to_string()),
        _ => Some("incomplete_stream".to_string()),
    }
}

/// How one attempt's pool-extension lease settles, read off the very row the core is about to
/// log: an error row that produced no output is `Released`; anything else — including a stream
/// that produced output and then broke — is `Consumed`.
pub fn lease_outcome(error_code: Option<&str>, saw_output: bool) -> LeaseOutcome {
    if error_code.is_some() && !saw_output {
        LeaseOutcome::Released
    } else {
        LeaseOutcome::Consumed
    }
}

fn retry_after_seconds(until_ms: i64) -> u64 {
    let seconds = (until_ms - now_ms()).div_euclid(1000) + i64::from((until_ms - now_ms()).rem_euclid(1000) > 0);
    seconds.clamp(1, 60) as u64
}

/// `x-should-retry` on terminal errors (docs/api.md § Errors).
fn retry_marker(status: u16, error_code: Option<&str>) -> Option<&'static str> {
    match (status, error_code) {
        (503, Some("upstream_unavailable")) => Some("true"),
        (400, Some("no_upstream_account" | "invalid_model" | "loop_detected")) => Some("false"),
        (429, Some("spend_limit_exceeded")) => Some("false"),
        _ => None,
    }
}

fn json_response(status: StatusCode, body: &Value) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("response builds")
}

fn with_header(mut res: Response, name: HeaderName, value: &str) -> Response {
    if let Ok(v) = HeaderValue::from_str(value) {
        res.headers_mut().insert(name, v);
    }
    res
}

/// Shared "pool unavailable" 503 for both surfaces, with `Retry-After` attached whenever the
/// earliest bench/limit expiry across the candidates is known (docs/api.md § Errors).
fn upstream_unavailable_response(body: &Value, until_ms: Option<i64>) -> Response {
    let mut res = json_response(StatusCode::SERVICE_UNAVAILABLE, body);
    res = with_header(res, HeaderName::from_static("x-should-retry"), "true");
    match until_ms {
        Some(until) => with_header(res, header::RETRY_AFTER, &retry_after_seconds(until).to_string()),
        None => res,
    }
}

/// Best-effort message extraction from an upstream error JSON body.
fn message_from_upstream_error_body(text: &str, fallback: &str) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        if let Some(message) = value.get("error").and_then(|e| e.get("message")).and_then(Value::as_str) {
            if !message.is_empty() {
                return message.to_string();
            }
        }
        if let Some(message) = value.get("error").and_then(Value::as_str) {
            if !message.is_empty() {
                return message.to_string();
            }
        }
        if let Some(message) = value.get("message").and_then(Value::as_str) {
            if !message.is_empty() {
                return message.to_string();
            }
        }
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

/// SSE response headers for eager commit (HTTP already 200 before upstream).
fn sse_response_headers(body: Body) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(body)
        .expect("response builds")
}

pub fn is_event_stream(res: &Response) -> bool {
    res.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("text/event-stream"))
}

/// `shared` = the candidate is a borrowed row: the owner's rate-limit and identity headers are
/// dropped first (docs/cloud-edition.md § "Pool extension"). Own rows pass the default and are
/// unchanged.
pub fn passthrough_stream_headers(headers: &HeaderMap, shared: bool) -> HeaderMap {
    let src = if shared { borrower_safe_headers(headers) } else { headers.clone() };
    let mut out = HeaderMap::new();
    let content_type = src
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .unwrap_or("text/event-stream; charset=utf-8");
    out.insert(header::CONTENT_TYPE, HeaderValue::from_str(content_type).expect("content type"));
    out.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    for (name, value) in &src {
        if name.as_str().starts_with("anthropic-ratelimit-") {
            out.insert(name.clone(), value.clone());
        }
    }
    out
}

/// Reads a whole response body; a body that fails mid-read reads as empty, exactly as the
/// TypeScript `safeResponseText`.
async fn safe_response_text(body: Body) -> String {
    match axum::body::to_bytes(body, usize::MAX).await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => String::new(),
    }
}

/// An axum body as the `ByteStream` the SSE relays consume.
pub(crate) fn body_stream(body: Body) -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> + Send {
    use futures::StreamExt as _;
    body.into_data_stream().map(|r| r.map_err(std::io::Error::other))
}

fn is_relay_request_too_large(res: &Response) -> bool {
    res.status() == StatusCode::PAYLOAD_TOO_LARGE
        && res.headers().get("x-kano-relay-error").and_then(|v| v.to_str().ok()) == Some("request_too_large")
}

/// `None` (nothing captured) flattens to all-NULL `request_logs` token fields.
fn apply_usage(entry: &mut LogEntry, usage: Option<NormalizedUsage>) {
    let usage = usage.unwrap_or_default();
    entry.prompt_tokens = usage.prompt_tokens;
    entry.completion_tokens = usage.completion_tokens;
    entry.cache_read_input_tokens = usage.cache_read_input_tokens;
    entry.cache_creation_input_tokens = usage.cache_creation_input_tokens;
}

/// What a transport needs beyond the walk itself: where to log, which protocol, and how to
/// reach the upstream per candidate.
pub struct TransportOpts {
    pub user_id: String,
    pub api_key_id: Option<String>,
    /// The model-group alias this request was addressed to, if any.
    pub group_name: Option<String>,
    /// Testability hook for the streaming idle timeout; defaults to 120s.
    pub idle_timeout: Option<Duration>,
    /// Logged on rows written before any candidate was attempted (`no_upstream_account`, pool
    /// unavailable, attempt cap). `requested_model` is already canonical.
    pub requested_provider: String,
    pub requested_model: String,
    pub source: CandidateSource,
    pub upstream_model: String,
    pub caller: Arc<dyn CandidateCaller>,
    pub wire: Arc<dyn Wire>,
    /// `false` for `count_tokens`: an estimate never consumes tokens, so no usage is ever
    /// logged from it.
    pub capture_usage: bool,
}

impl TransportOpts {
    fn idle_timeout(&self) -> Duration {
        self.idle_timeout.unwrap_or(DEFAULT_IDLE_TIMEOUT)
    }

    fn base_entry(&self, started_at_ms: i64) -> LogEntry {
        LogEntry {
            started_at_ms: Some(started_at_ms),
            user_id: self.user_id.clone(),
            api_key_id: self.api_key_id.clone(),
            group_name: self.group_name.clone(),
            provider: self.requested_provider.clone(),
            model: self.requested_model.clone(),
            ..Default::default()
        }
    }

    fn candidate_entry(&self, started_at_ms: i64, candidate: &RoutingCandidate) -> LogEntry {
        LogEntry {
            provider: candidate.provider.clone(),
            model: canonical_model_id(&candidate.provider, &candidate.upstream_model),
            account_id: Some(candidate.account.id.clone()),
            ..self.base_entry(started_at_ms)
        }
    }
}

/// The lease of the one admitted attempt, settled exactly once — whichever of the stream's
/// close and the walk's return happens last.
#[derive(Default)]
struct LeaseSlot {
    lease: Option<Box<dyn AttemptLease>>,
    /// The walk has finished, so `lease` is final — `None` included.
    ready: bool,
    settled: bool,
    /// The stream closed before the walk handed its lease over.
    pending: Option<LeaseOutcome>,
}

fn settle_once(slot: &Arc<Mutex<LeaseSlot>>, outcome: LeaseOutcome) {
    let lease = {
        let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
        if slot.settled {
            return;
        }
        slot.settled = true;
        slot.lease.take()
    };
    // The settlement must outlive the client connection (`ctx.waitUntil`).
    tokio::spawn(async move {
        settle_lease(lease.as_deref(), outcome).await;
    });
}

/// Eager streaming commit: return 200 + SSE immediately, run the candidate walk inside the
/// stream (docs/api.md § Eager streaming commit). Every failure after commit is a terminal
/// frame; one log row on close.
async fn dispatch_eager(cx: &AppState, t: Arc<TransportOpts>) -> Response {
    let started = now_ms();
    let idle_timeout = t.idle_timeout();
    let sniffer: Arc<Mutex<Box<dyn UsageSniffer>>> = Arc::new(Mutex::new(t.wire.create_usage_sniffer()));
    let progress = Arc::new(Mutex::new(WalkProgress::default()));
    let forced_error_code: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    // TTFB into the pipe, or time until a terminal fail/cancel if earlier.
    let headers_latency: Arc<Mutex<Option<i64>>> = Arc::new(Mutex::new(None));
    let slot: Arc<Mutex<LeaseSlot>> = Arc::new(Mutex::new(LeaseSlot::default()));
    // Whether any upstream byte reached the client — a stream that produced output and then
    // broke still counts.
    let saw_output = Arc::new(AtomicBool::new(false));

    let on_close = {
        let cx = cx.clone();
        let t = t.clone();
        let sniffer = sniffer.clone();
        let progress = progress.clone();
        let forced_error_code = forced_error_code.clone();
        let headers_latency = headers_latency.clone();
        let slot = slot.clone();
        let saw_output = saw_output.clone();
        Box::new(move |reason: StreamCloseReason| {
            let (usage, complete) = {
                let sniffer = sniffer.lock().unwrap_or_else(|e| e.into_inner());
                (sniffer.finish(), sniffer.complete())
            };
            let error_code = forced_error_code
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .or_else(|| stream_close_error_code(reason, complete));
            let outcome = lease_outcome(error_code.as_deref(), saw_output.load(Ordering::SeqCst));
            let ready = slot.lock().unwrap_or_else(|e| e.into_inner()).ready;
            if ready {
                settle_once(&slot, outcome);
            } else {
                slot.lock().unwrap_or_else(|e| e.into_inner()).pending = Some(outcome);
            }

            let progress = progress.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let mut entry = match progress.candidate.as_ref() {
                Some(candidate) => t.candidate_entry(started, candidate),
                None => t.base_entry(started),
            };
            entry.status_code = 200;
            entry.latency_ms = headers_latency.lock().unwrap_or_else(|e| e.into_inner()).unwrap_or(now_ms() - started);
            entry.error_code = error_code;
            entry.upstream_status = progress.upstream_status;
            apply_usage(&mut entry, usage);
            // The row must outlive the client connection, exactly as `waitUntil` kept it alive.
            tokio::spawn(async move { log_request(&cx, entry).await });
        })
    };

    let error_frame = t.wire.upstream_error_frame();
    let body = {
        let cx = cx.clone();
        let t = t.clone();
        stream_with_eager_producer(
            move |ctl| async move {
                let fail = |error_code: &str, frame: Bytes| {
                    *headers_latency.lock().unwrap_or_else(|e| e.into_inner()) = Some(now_ms() - started);
                    *forced_error_code.lock().unwrap_or_else(|e| e.into_inner()) = Some(error_code.to_string());
                    frame
                };
                let outcome = walk_candidates(
                    &cx,
                    WalkOpts {
                        source: &t.source,
                        upstream_model: &t.upstream_model,
                        caller: t.caller.as_ref(),
                        cancelled: Some(ctl.cancel_signal()),
                        progress: progress.clone(),
                    },
                )
                .await;

                // Take the lease before anything else can await: every other outcome already
                // settled its own leases, so `None` is final for them too.
                let (delivered, frame) = match outcome {
                    WalkOutcome::Response { response, lease, .. } => {
                        slot.lock().unwrap_or_else(|e| e.into_inner()).lease = lease;
                        (Some(response), None)
                    }
                    WalkOutcome::Cancelled => (None, None),
                    WalkOutcome::NoAccount => {
                        (None, Some(fail("no_upstream_account", t.wire.no_account_frame(&t.requested_provider))))
                    }
                    WalkOutcome::Unavailable { .. } | WalkOutcome::Exhausted { .. } => {
                        (None, Some(fail("upstream_unavailable", t.wire.unavailable_frame())))
                    }
                    WalkOutcome::FetchError { .. } => {
                        (None, Some(fail("upstream_error", t.wire.upstream_error_frame())))
                    }
                };
                let pending = {
                    let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
                    slot.ready = true;
                    slot.pending
                };
                if let Some(pending) = pending {
                    settle_once(&slot, pending);
                }
                let Some(response) = delivered else {
                    if let Some(frame) = frame {
                        ctl.fail(frame).await;
                    }
                    return;
                };

                let ok = response.status().is_success();
                let event_stream = is_event_stream(&response);
                let relay_too_large = is_relay_request_too_large(&response);
                let (parts, body) = response.into_parts();
                if ok && event_stream {
                    *headers_latency.lock().unwrap_or_else(|e| e.into_inner()) = Some(now_ms() - started);
                    let tap_sniffer = sniffer.clone();
                    let tap_saw_output = saw_output.clone();
                    ctl.pipe_upstream(
                        Box::pin(body_stream(body)),
                        PipeOpts {
                            tap: Some(Arc::new(move |chunk: &Bytes| {
                                tap_saw_output.store(true, Ordering::SeqCst);
                                tap_sniffer.lock().unwrap_or_else(|e| e.into_inner()).feed(chunk);
                            })),
                            idle_timeout: Some(idle_timeout),
                            stall_frame: Some(t.wire.stall_frame()),
                            ..Default::default()
                        },
                    )
                    .await;
                    return;
                }

                let text = safe_response_text(body).await;
                if !parts.status.is_success() {
                    let frame = if relay_too_large {
                        fail(
                            "request_too_large",
                            t.wire
                                .request_too_large_frame(&message_from_upstream_error_body(&text, "request body too large")),
                        )
                    } else {
                        fail(
                            "upstream_error",
                            t.wire.upstream_error_frame_from_body(
                                &message_from_upstream_error_body(&text, "upstream error"),
                                &text,
                            ),
                        )
                    };
                    ctl.fail(frame).await;
                    return;
                }
                // 200 but not an event stream under stream:true — nothing useful to pipe.
                *headers_latency.lock().unwrap_or_else(|e| e.into_inner()) = Some(now_ms() - started);
                ctl.close();
            },
            DEFAULT_KEEPALIVE_INTERVAL,
            StreamKeepaliveOpts {
                error_frame: Some(error_frame),
                on_close: Some(on_close),
                ..Default::default()
            },
        )
    };

    sse_response_headers(Body::from_stream(body))
}

/// Shapes the response for a non-bench upstream response, and settles that attempt's lease and
/// log row. The default below serves Chat Completions and Messages; audio supplies its own.
#[async_trait]
pub trait NonStreamDelivery: Send + Sync {
    async fn deliver(&self, cx: &AppState, t: &Arc<TransportOpts>, delivery: Delivery) -> Response;
}

/// One non-bench attempt, handed to a [`NonStreamDelivery`].
pub struct Delivery {
    pub candidate: RoutingCandidate,
    pub response: Response,
    pub latency_ms: i64,
    /// This attempt's pool-extension lease — settle it where this delivery decides the log row.
    pub lease: Option<Box<dyn AttemptLease>>,
    pub started_at_ms: i64,
}

/// Non-stream transport: run the walk first, then mirror the outcome as a real HTTP status —
/// `400` / `503` (+ `Retry-After`) / `502` for pool and fetch failures, upstream status passed
/// through otherwise (docs/api.md § Errors).
pub async fn dispatch_non_stream(cx: &AppState, t: Arc<TransportOpts>) -> Response {
    dispatch_non_stream_with(cx, t, &DefaultNonStreamDelivery).await
}

pub async fn dispatch_non_stream_with(
    cx: &AppState,
    t: Arc<TransportOpts>,
    delivery: &dyn NonStreamDelivery,
) -> Response {
    let started = now_ms();
    let progress = Arc::new(Mutex::new(WalkProgress::default()));
    let outcome = walk_candidates(
        cx,
        WalkOpts {
            source: &t.source,
            upstream_model: &t.upstream_model,
            caller: t.caller.as_ref(),
            cancelled: None,
            progress: progress.clone(),
        },
    )
    .await;

    match outcome {
        // The non-stream transport passes no cancel signal, so the walk can never report this.
        WalkOutcome::Cancelled => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &t.wire.upstream_error_body(),
        ),
        WalkOutcome::NoAccount => {
            let mut entry = t.base_entry(started);
            entry.status_code = 400;
            entry.latency_ms = now_ms() - started;
            entry.error_code = Some("no_upstream_account".into());
            log_request(cx, entry).await;
            let res = json_response(StatusCode::BAD_REQUEST, &t.wire.no_account_body(&t.requested_provider));
            match retry_marker(400, Some("no_upstream_account")) {
                Some(v) => with_header(res, HeaderName::from_static("x-should-retry"), v),
                None => res,
            }
        }
        WalkOutcome::Unavailable { until_ms } => {
            let mut entry = t.base_entry(started);
            entry.status_code = 503;
            entry.latency_ms = now_ms() - started;
            entry.error_code = Some("upstream_unavailable".into());
            log_request(cx, entry).await;
            upstream_unavailable_response(&t.wire.unavailable_body(), until_ms)
        }
        WalkOutcome::Exhausted { tried, last_benched } => {
            // Same 503 + Retry-After as the all-unusable case, recomputed now that this walk
            // just benched more of the pool.
            let until_ms = recompute_unavailable_until(cx, &tried).await;
            let mut entry = match last_benched.as_ref() {
                Some(candidate) => t.candidate_entry(started, candidate),
                None => t.base_entry(started),
            };
            entry.status_code = 503;
            entry.latency_ms = now_ms() - started;
            entry.error_code = Some("upstream_unavailable".into());
            entry.upstream_status = progress.lock().unwrap_or_else(|e| e.into_inner()).upstream_status;
            log_request(cx, entry).await;
            upstream_unavailable_response(&t.wire.unavailable_body(), until_ms)
        }
        WalkOutcome::FetchError { candidate } => {
            let mut entry = t.candidate_entry(started, &candidate);
            entry.status_code = 502;
            entry.latency_ms = now_ms() - started;
            entry.error_code = Some("upstream_error".into());
            log_request(cx, entry).await;
            json_response(StatusCode::BAD_GATEWAY, &t.wire.upstream_error_body())
        }
        WalkOutcome::Response { candidate, response, lease } => {
            let d = Delivery { candidate, response, latency_ms: now_ms() - started, lease, started_at_ms: started };
            delivery.deliver(cx, &t, d).await
        }
    }
}

/// Default non-stream delivery. An event-stream body under a non-stream request ("legacy
/// attach", docs/logging.md) gets keepalive + idle timeout and the upstream status; anything
/// else is delivered the way the wire says — rebuilt with only `content-type`, or returned as
/// received with the body peeked for usage so every upstream header passes through.
pub struct DefaultNonStreamDelivery;

/// One non-stream attempt's settle-then-log, in that order: the lease settles on the very
/// decision that writes the row. An upstream error status passed through unchanged carries no
/// useful output either, whatever the row's `error_code` says.
async fn settle_and_log(
    cx: &AppState,
    entry: LogEntry,
    lease: Option<Box<dyn AttemptLease>>,
    upstream_status: u16,
    saw_output: bool,
) {
    let outcome = if upstream_status >= 400 {
        LeaseOutcome::Released
    } else {
        lease_outcome(entry.error_code.as_deref(), saw_output)
    };
    settle_lease(lease.as_deref(), outcome).await;
    log_request(cx, entry).await;
}

#[async_trait]
impl NonStreamDelivery for DefaultNonStreamDelivery {
    async fn deliver(&self, cx: &AppState, t: &Arc<TransportOpts>, delivery: Delivery) -> Response {
        let Delivery { candidate, response, latency_ms, lease, started_at_ms } = delivery;
        let status = response.status();
        let relay_too_large = is_relay_request_too_large(&response);
        let event_stream = is_event_stream(&response);
        let shared = candidate.share.is_some();
        let mut entry = t.candidate_entry(started_at_ms, &candidate);
        entry.status_code = status.as_u16() as i32;
        entry.latency_ms = latency_ms;
        entry.upstream_status = Some(status.as_u16() as i32);

        let (parts, body) = response.into_parts();

        if relay_too_large {
            let text = safe_response_text(body).await;
            let message = message_from_upstream_error_body(&text, "request body too large");
            let mut entry = entry;
            entry.status_code = 413;
            entry.error_code = Some("request_too_large".into());
            settle_and_log(cx, entry, lease, status.as_u16(), false).await;
            let res = json_response(StatusCode::PAYLOAD_TOO_LARGE, &t.wire.request_too_large_body(&message));
            return with_header(res, HeaderName::from_static("x-should-retry"), "false");
        }

        if event_stream {
            // "Legacy attach": an event-stream body under a non-stream request. The log row —
            // and with it the lease — lands on the stream's close.
            let sniffer: Arc<Mutex<Box<dyn UsageSniffer>>> = Arc::new(Mutex::new(t.wire.create_usage_sniffer()));
            let saw_output = Arc::new(AtomicBool::new(false));
            let tap_sniffer = sniffer.clone();
            let tap_saw_output = saw_output.clone();
            let capture_usage = t.capture_usage;
            let cx_close = cx.clone();
            let lease = Arc::new(Mutex::new(lease));
            let on_close = Box::new(move |reason: StreamCloseReason| {
                let (usage, complete) = {
                    let sniffer = sniffer.lock().unwrap_or_else(|e| e.into_inner());
                    (sniffer.finish(), sniffer.complete())
                };
                let mut entry = entry;
                entry.error_code =
                    if capture_usage { stream_close_error_code(reason, complete) } else { None };
                apply_usage(&mut entry, if capture_usage { usage } else { None });
                let lease = lease.lock().unwrap_or_else(|e| e.into_inner()).take();
                let saw_output = saw_output.load(Ordering::SeqCst);
                let upstream_status = status.as_u16();
                tokio::spawn(async move {
                    settle_and_log(&cx_close, entry, lease, upstream_status, saw_output).await;
                });
            });
            let stream = stream_with_keepalive(
                Box::pin(body_stream(body)),
                DEFAULT_KEEPALIVE_INTERVAL,
                StreamKeepaliveOpts {
                    tap: Some(Arc::new(move |chunk: &Bytes| {
                        tap_saw_output.store(true, Ordering::SeqCst);
                        tap_sniffer.lock().unwrap_or_else(|e| e.into_inner()).feed(chunk);
                    })),
                    on_close: Some(on_close),
                    idle_timeout: Some(t.idle_timeout()),
                    stall_frame: Some(t.wire.stall_frame()),
                    error_frame: None,
                },
            );
            let mut res = Response::builder().status(parts.status).body(Body::from_stream(stream)).expect("builds");
            *res.headers_mut() = passthrough_stream_headers(&parts.headers, shared);
            return res;
        }

        let text = safe_response_text(body).await;
        let usage = if t.capture_usage {
            serde_json::from_str::<Value>(&text).ok().map(|json| t.wire.parse_usage(json.get("usage")))
        } else {
            None
        };
        apply_usage(&mut entry, usage);
        settle_and_log(cx, entry, lease, status.as_u16(), true).await;

        match t.wire.non_stream_response() {
            NonStreamResponse::ContentTypeOnly => {
                let content_type = parts
                    .headers
                    .get(header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .filter(|v| !v.is_empty())
                    .unwrap_or("application/json")
                    .to_string();
                Response::builder()
                    .status(parts.status)
                    .header(header::CONTENT_TYPE, content_type)
                    .body(Body::from(text))
                    .expect("response builds")
            }
            NonStreamResponse::AsReceived => {
                // Same status and body as received; a borrowed row drops the owner's
                // rate-limit/identity headers first.
                let mut res = Response::builder().status(parts.status).body(Body::from(text)).expect("builds");
                *res.headers_mut() =
                    if shared { borrower_safe_headers(&parts.headers) } else { parts.headers.clone() };
                res
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Chat Completions
// ---------------------------------------------------------------------------------------

struct ChatCaller {
    req: ChatCompletionRequest,
}

#[async_trait]
impl CandidateCaller for ChatCaller {
    fn supports(&self, _candidate: &RoutingCandidate) -> bool {
        true
    }
    async fn call(
        &self,
        cx: &AppState,
        candidate: &RoutingCandidate,
        acquired: &crate::pool::AcquiredAccount,
        extras: &CallExtras,
    ) -> Result<Response, AdapterError> {
        let req = ChatCompletionRequest { upstream_model: candidate.upstream_model.clone(), ..self.req.clone() };
        candidate.adapter.chat_completions(cx, acquired, &req, extras).await
    }
}

/// Options for [`dispatch_chat_completions`].
#[derive(Default)]
pub struct ChatDispatchOptions {
    pub user_id: String,
    pub api_key_id: Option<String>,
    /// Builtin `ProviderId` or a custom provider's slug.
    pub provider: String,
    /// Pre-resolved adapter for custom providers; defaults to the builtin registry.
    pub adapter: Option<DynAdapter>,
    pub req: ChatCompletionRequest,
    /// Testability hook for the streaming idle timeout; defaults to 120s.
    pub idle_timeout: Option<Duration>,
    /// The model-group alias this request was addressed to, if any.
    pub group_name: Option<String>,
    /// Model-group account pinning. Ignored when `candidates` is given (already scoped).
    pub pinned_account_id: Option<String>,
    /// Pre-built cross-target candidate list — group dispatch.
    pub candidates: Option<Vec<RoutingCandidate>>,
    pub strategy: Option<String>,
    pub is_builtin: Option<bool>,
    pub custom_provider: Option<CustomProviderRow>,
}

impl ChatDispatchOptions {
    fn source(&self) -> CandidateSource {
        CandidateSource {
            user_id: self.user_id.clone(),
            api_key_id: self.api_key_id.clone(),
            provider: self.provider.clone(),
            adapter: self.adapter.clone(),
            pinned_account_id: self.pinned_account_id.clone(),
            candidates: self.candidates.clone(),
            strategy: self.strategy.clone(),
            is_builtin: self.is_builtin,
            custom_provider: self.custom_provider.clone(),
        }
    }
}

pub async fn dispatch_chat_completions(cx: &AppState, opts: ChatDispatchOptions) -> Response {
    let stream = opts.req.stream.unwrap_or(false);
    let transport = Arc::new(TransportOpts {
        user_id: opts.user_id.clone(),
        api_key_id: opts.api_key_id.clone(),
        group_name: opts.group_name.clone(),
        idle_timeout: opts.idle_timeout,
        requested_provider: opts.provider.clone(),
        requested_model: canonical_model_id(&opts.provider, &opts.req.upstream_model),
        source: opts.source(),
        upstream_model: opts.req.upstream_model.clone(),
        caller: Arc::new(ChatCaller { req: opts.req.clone() }),
        wire: Arc::new(OpenAiWire),
        capture_usage: true,
    });
    if stream {
        dispatch_eager(cx, transport).await
    } else {
        dispatch_non_stream(cx, transport).await
    }
}

// ---------------------------------------------------------------------------------------
// Anthropic Messages / count_tokens
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnthropicEndpoint {
    #[default]
    Messages,
    CountTokens,
}

struct MessagesCaller {
    body: Map<String, Value>,
    headers: HeaderMap,
    endpoint: AnthropicEndpoint,
}

#[async_trait]
impl CandidateCaller for MessagesCaller {
    fn supports(&self, candidate: &RoutingCandidate) -> bool {
        match self.endpoint {
            AnthropicEndpoint::Messages => candidate.adapter.has_messages(),
            AnthropicEndpoint::CountTokens => candidate.adapter.has_count_tokens(),
        }
    }
    async fn call(
        &self,
        cx: &AppState,
        candidate: &RoutingCandidate,
        acquired: &crate::pool::AcquiredAccount,
        extras: &CallExtras,
    ) -> Result<Response, AdapterError> {
        // `body` with `.model` rewritten to the candidate's own upstream id — cache_control and
        // everything else pass through untouched.
        let mut body = self.body.clone();
        body.insert("model".into(), Value::String(candidate.upstream_model.clone()));
        let body = Value::Object(body);
        match self.endpoint {
            AnthropicEndpoint::Messages => {
                candidate.adapter.messages(cx, acquired, &body, &self.headers, extras).await
            }
            AnthropicEndpoint::CountTokens => {
                candidate.adapter.count_tokens(cx, acquired, &body, &self.headers, extras).await
            }
        }
    }
}

/// Options for [`dispatch_anthropic_messages`].
#[derive(Default)]
pub struct AnthropicDispatchOptions {
    pub user_id: String,
    pub api_key_id: Option<String>,
    pub body: Map<String, Value>,
    pub headers: HeaderMap,
    /// Canonical `provider/upstream_model` for `request_logs.model` — never a group alias.
    pub model: String,
    /// Builtin `ProviderId` or a custom provider's slug. Defaults to claude-code.
    pub provider: Option<String>,
    pub adapter: Option<DynAdapter>,
    pub endpoint: AnthropicEndpoint,
    pub idle_timeout: Option<Duration>,
    pub group_name: Option<String>,
    pub pinned_account_id: Option<String>,
    pub candidates: Option<Vec<RoutingCandidate>>,
    pub strategy: Option<String>,
    pub is_builtin: Option<bool>,
    pub custom_provider: Option<CustomProviderRow>,
}

fn body_wants_stream(body: &Map<String, Value>) -> bool {
    body.get("stream") == Some(&Value::Bool(true))
}

/// Native Anthropic Messages (or count_tokens) passthrough — claude-code by default, or a
/// custom anthropic-format provider when `provider`/`adapter` are given. cache_control is never
/// rewritten; the body goes through the adapter method as-is aside from the `model` rewrite to
/// the acquired candidate's own upstream id. `endpoint` selects which adapter method carries
/// the request; both share the same candidate walk.
pub async fn dispatch_anthropic_messages(cx: &AppState, opts: AnthropicDispatchOptions) -> Response {
    let provider = opts.provider.clone().unwrap_or_else(|| "claude-code".to_string());
    let endpoint = opts.endpoint;

    // Fixed adapter (no candidates): keep the immediate rejection for an adapter that never
    // supports this endpoint at all, before ever touching the pool — 500, not a synthesized
    // pool-exhaustion error.
    if opts.candidates.is_none() {
        let fixed = opts
            .adapter
            .clone()
            .or_else(|| ProviderId::parse(&provider).map(crate::providers::registry::get_adapter));
        let supported = match (&fixed, endpoint) {
            (Some(adapter), AnthropicEndpoint::Messages) => adapter.has_messages(),
            (Some(adapter), AnthropicEndpoint::CountTokens) => adapter.has_count_tokens(),
            (None, _) => false,
        };
        if !supported {
            let name = match endpoint {
                AnthropicEndpoint::Messages => "messages",
                AnthropicEndpoint::CountTokens => "count_tokens",
            };
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &serde_json::json!({
                    "type": "error",
                    "error": { "type": "api_error", "message": format!("{name} not supported") }
                }),
            );
        }
    }

    let stream = endpoint == AnthropicEndpoint::Messages && body_wants_stream(&opts.body);
    let transport = Arc::new(TransportOpts {
        user_id: opts.user_id.clone(),
        api_key_id: opts.api_key_id.clone(),
        group_name: opts.group_name.clone(),
        idle_timeout: opts.idle_timeout,
        requested_provider: provider.clone(),
        requested_model: opts.model.clone(),
        source: CandidateSource {
            user_id: opts.user_id.clone(),
            api_key_id: opts.api_key_id.clone(),
            provider: provider.clone(),
            adapter: opts.adapter.clone(),
            pinned_account_id: opts.pinned_account_id.clone(),
            candidates: opts.candidates.clone(),
            strategy: opts.strategy.clone(),
            is_builtin: opts.is_builtin,
            custom_provider: opts.custom_provider.clone(),
        },
        // Bare upstream id from the canonical `provider/upstream_model` string — builds the
        // single-pool candidate when no pre-built list was handed in.
        upstream_model: split_model_id(&opts.model).map(|s| s.upstream_model).unwrap_or(opts.model.clone()),
        caller: Arc::new(MessagesCaller {
            body: opts.body.clone(),
            headers: opts.headers.clone(),
            endpoint,
        }),
        wire: Arc::new(AnthropicWire),
        capture_usage: endpoint != AnthropicEndpoint::CountTokens,
    });
    if stream {
        dispatch_eager(cx, transport).await
    } else {
        dispatch_non_stream(cx, transport).await
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::db::accounts::{get_account, AccountRow};
    use crate::db::test_support::{insert_account, insert_user, skip_without_db, test_pool, test_state};
    use crate::pool::bench::is_benched;
    use crate::pool::StoredCredential;
    use crate::providers::types::ProviderAdapter;
    use crate::proxy::dispatch_walk::MAX_ATTEMPTS;
    use crate::upstream::MockTransport;
    use futures::StreamExt;
    use serde_json::json;
    use sqlx::PgPool;
    use std::sync::atomic::AtomicUsize;

    /// The stubbed upstream answer one test adapter gives for one attempt.
    pub(crate) enum Reply {
        Json(StatusCode, Value),
        Text(StatusCode, &'static str),
        Sse(StatusCode, String),
        /// An event-stream body that sends one chunk and then goes silent forever.
        SilentSse(String),
        /// The adapter itself fails.
        Error,
        /// A response with extra headers (relay 413, agent faults, rate-limit resets).
        WithHeaders(StatusCode, Value, Vec<(&'static str, String)>),
    }

    /// A scripted adapter: one reply per call, recording which account each call used.
    pub(crate) struct ScriptedAdapter {
        pub id: String,
        pub calls: Arc<Mutex<Vec<String>>>,
        replies: Mutex<std::collections::VecDeque<Reply>>,
        /// Used once the scripted replies run out.
        default_reply: Box<dyn Fn() -> Reply + Send + Sync>,
        messages: AtomicBool,
    }

    impl ScriptedAdapter {
        pub(crate) fn new(id: &str, replies: Vec<Reply>) -> Arc<Self> {
            Arc::new(Self {
                id: id.into(),
                calls: Arc::new(Mutex::new(Vec::new())),
                replies: Mutex::new(replies.into()),
                default_reply: Box::new(|| Reply::Json(StatusCode::OK, json!({ "choices": [] }))),
                messages: AtomicBool::new(false),
            })
        }
        /// Every call answers the same way.
        pub(crate) fn always(id: &str, reply: impl Fn() -> Reply + Send + Sync + 'static) -> Arc<Self> {
            Arc::new(Self {
                id: id.into(),
                calls: Arc::new(Mutex::new(Vec::new())),
                replies: Mutex::new(Default::default()),
                default_reply: Box::new(reply),
                messages: AtomicBool::new(false),
            })
        }
        /// Marks the same adapter as serving the Anthropic Messages endpoint, keeping its
        /// scripted replies.
        pub(crate) fn for_messages(self: Arc<Self>) -> Arc<Self> {
            self.messages.store(true, Ordering::SeqCst);
            self
        }
        pub(crate) fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }

        async fn answer(&self, account_id: &str) -> Result<Response, AdapterError> {
            self.calls.lock().unwrap_or_else(|e| e.into_inner()).push(account_id.to_string());
            let reply = self
                .replies
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pop_front()
                .unwrap_or_else(|| (self.default_reply)());
            Ok(match reply {
                Reply::Json(status, value) => json_response(status, &value),
                Reply::Text(status, text) => Response::builder()
                    .status(status)
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(Body::from(text))
                    .unwrap(),
                Reply::Sse(status, text) => Response::builder()
                    .status(status)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from(text))
                    .unwrap(),
                Reply::SilentSse(initial) => {
                    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
                    tx.send(Bytes::from(initial)).unwrap();
                    std::mem::forget(tx);
                    let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx)
                        .map(Ok::<Bytes, std::io::Error>);
                    Response::builder()
                        .status(StatusCode::OK)
                        .header(header::CONTENT_TYPE, "text/event-stream")
                        .body(Body::from_stream(stream))
                        .unwrap()
                }
                Reply::Error => return Err(AdapterError::Other(anyhow::anyhow!("adapter failed"))),
                Reply::WithHeaders(status, value, headers) => {
                    let mut res = json_response(status, &value);
                    for (name, v) in headers {
                        res.headers_mut().insert(
                            HeaderName::from_static(name),
                            HeaderValue::from_str(&v).expect("header"),
                        );
                    }
                    res
                }
            })
        }
    }

    #[async_trait]
    impl ProviderAdapter for ScriptedAdapter {
        fn id(&self) -> &str {
            &self.id
        }
        async fn chat_completions(
            &self,
            _: &AppState,
            account: &crate::pool::AcquiredAccount,
            _: &ChatCompletionRequest,
            _: &CallExtras,
        ) -> Result<Response, AdapterError> {
            self.answer(&account.row.id).await
        }
        fn has_messages(&self) -> bool {
            self.messages.load(Ordering::SeqCst)
        }
        async fn messages(
            &self,
            _: &AppState,
            account: &crate::pool::AcquiredAccount,
            _: &Value,
            _: &HeaderMap,
            _: &CallExtras,
        ) -> Result<Response, AdapterError> {
            if !self.messages.load(Ordering::SeqCst) {
                return Err(AdapterError::Unsupported("messages"));
            }
            self.answer(&account.row.id).await
        }
    }

    pub(crate) async fn seed_accounts(pool: &PgPool, user_id: &str, provider: &str, ids: &[&str]) -> Vec<AccountRow> {
        let mut rows = Vec::new();
        for (i, id) in ids.iter().enumerate() {
            let row = insert_account(pool, user_id, provider, &StoredCredential { access_token: format!("tok-{id}"), ..Default::default() }).await;
            sqlx::query("UPDATE upstream_accounts SET id = $1, priority = $2 WHERE id = $3")
                .bind(id)
                .bind((ids.len() - i) as i32)
                .bind(&row.id)
                .execute(pool)
                .await
                .expect("rename account");
            rows.push(get_account(pool, user_id, id).await.unwrap().unwrap());
        }
        rows
    }

    pub(crate) async fn logs(state: &AppState) -> Vec<crate::db::request_logs::RequestLogRow> {
        sqlx::query_as::<_, crate::db::request_logs::RequestLogRow>(
            "SELECT * FROM request_logs ORDER BY created_at ASC, id ASC",
        )
        .fetch_all(state.pool())
        .await
        .expect("read logs")
    }

    /// Waits for the single detached log row a stream transport writes on close.
    pub(crate) async fn wait_for_logs(state: &AppState, count: usize) -> Vec<crate::db::request_logs::RequestLogRow> {
        for _ in 0..200 {
            let rows = logs(state).await;
            if rows.len() >= count {
                return rows;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        logs(state).await
    }

    pub(crate) async fn body_text(res: Response) -> String {
        safe_response_text(res.into_body()).await
    }

    fn chat_req(model: &str, upstream: &str, stream: bool) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: model.into(),
            raw_model: model.into(),
            upstream_model: upstream.into(),
            messages: vec![json!({ "role": "user", "content": "hi" })],
            stream: Some(stream).filter(|s| *s),
            ..Default::default()
        }
    }

    fn chat_opts(adapter: Arc<dyn ProviderAdapter>, provider: &str, stream: bool) -> ChatDispatchOptions {
        ChatDispatchOptions {
            user_id: String::new(),
            api_key_id: None,
            provider: provider.into(),
            adapter: Some(adapter),
            req: chat_req(&format!("{provider}/some-model"), "some-model", stream),
            ..Default::default()
        }
    }

    // -------------------------------------------------------------------------------
    // Pure helpers
    // -------------------------------------------------------------------------------

    #[test]
    fn canonical_model_ids_are_prefix_plus_upstream() {
        assert_eq!(canonical_model_id("grok", "grok-4.5"), "grok/grok-4.5");
        assert_eq!(canonical_model_id("my-endpoint", "org/model"), "my-endpoint/org/model");
    }

    #[test]
    fn stream_close_error_codes_follow_the_documented_table() {
        assert_eq!(stream_close_error_code(StreamCloseReason::IdleTimeout, true).as_deref(), Some("upstream_stall"));
        assert_eq!(stream_close_error_code(StreamCloseReason::IdleTimeout, false).as_deref(), Some("upstream_stall"));
        assert_eq!(stream_close_error_code(StreamCloseReason::Done, true), None);
        assert_eq!(stream_close_error_code(StreamCloseReason::Cancel, true), None);
        assert_eq!(stream_close_error_code(StreamCloseReason::Cancel, false).as_deref(), Some("client_abort"));
        assert_eq!(stream_close_error_code(StreamCloseReason::Done, false).as_deref(), Some("incomplete_stream"));
        assert_eq!(stream_close_error_code(StreamCloseReason::Error, false).as_deref(), Some("incomplete_stream"));
    }

    #[test]
    fn a_lease_releases_only_for_an_output_less_error() {
        assert_eq!(lease_outcome(Some("upstream_error"), false), LeaseOutcome::Released);
        assert_eq!(lease_outcome(Some("incomplete_stream"), true), LeaseOutcome::Consumed);
        assert_eq!(lease_outcome(None, false), LeaseOutcome::Consumed);
        assert_eq!(lease_outcome(None, true), LeaseOutcome::Consumed);
    }

    #[test]
    fn retry_after_is_at_least_one_second_and_at_most_sixty() {
        assert_eq!(retry_after_seconds(now_ms() - 10_000), 1);
        assert_eq!(retry_after_seconds(now_ms() + 45_000), 45);
        assert_eq!(retry_after_seconds(now_ms() + 3_600_000), 60);
    }

    #[test]
    fn retry_markers_match_the_documented_vocabulary() {
        assert_eq!(retry_marker(503, Some("upstream_unavailable")), Some("true"));
        assert_eq!(retry_marker(400, Some("no_upstream_account")), Some("false"));
        assert_eq!(retry_marker(400, Some("invalid_model")), Some("false"));
        assert_eq!(retry_marker(400, Some("loop_detected")), Some("false"));
        assert_eq!(retry_marker(429, Some("spend_limit_exceeded")), Some("false"));
        assert_eq!(retry_marker(502, Some("upstream_error")), None);
        assert_eq!(retry_marker(503, None), None);
    }

    #[test]
    fn passthrough_headers_keep_only_content_type_cache_control_and_anthropic_budgets() {
        let mut h = HeaderMap::new();
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
        h.insert("anthropic-ratelimit-tokens-remaining", HeaderValue::from_static("5"));
        h.insert("x-request-id", HeaderValue::from_static("abc"));
        let own = passthrough_stream_headers(&h, false);
        assert_eq!(own.get(header::CONTENT_TYPE).unwrap(), "text/event-stream");
        assert_eq!(own.get(header::CACHE_CONTROL).unwrap(), "no-cache");
        assert_eq!(own.get("anthropic-ratelimit-tokens-remaining").unwrap(), "5");
        assert!(own.get("x-request-id").is_none());
        // A borrowed row never leaks the owner's budgets.
        let shared = passthrough_stream_headers(&h, true);
        assert!(shared.get("anthropic-ratelimit-tokens-remaining").is_none());
        assert_eq!(shared.get(header::CONTENT_TYPE).unwrap(), "text/event-stream");
    }

    #[test]
    fn upstream_error_messages_come_from_the_body_when_there_is_one() {
        assert_eq!(message_from_upstream_error_body(r#"{"error":{"message":"nope"}}"#, "fb"), "nope");
        assert_eq!(message_from_upstream_error_body(r#"{"error":"insufficient_credits"}"#, "fb"), "insufficient_credits");
        assert_eq!(message_from_upstream_error_body(r#"{"message":"bad"}"#, "fb"), "bad");
        assert_eq!(message_from_upstream_error_body("  plain text  ", "fb"), "plain text");
        assert_eq!(message_from_upstream_error_body("   ", "fb"), "fb");
    }

    // -------------------------------------------------------------------------------
    // Idle timeout (injectable for testability)
    // -------------------------------------------------------------------------------

    #[tokio::test]
    async fn a_stream_that_goes_silent_trips_the_idle_timeout_with_the_openai_stall_frame() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "stall@example.com").await;
        seed_accounts(state.pool(), &user.id, "grok", &["acc_grok"]).await;
        let adapter = ScriptedAdapter::new(
            "grok",
            vec![Reply::SilentSse(r#"data: {"choices":[{"delta":{"role":"assistant"}}]}"#.to_string() + "\n\n")],
        );

        let res = dispatch_chat_completions(
            &state,
            ChatDispatchOptions {
                user_id: user.id.clone(),
                api_key_id: None,
                idle_timeout: Some(Duration::from_millis(20)),
                ..chat_opts(adapter, "grok", true)
            },
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let text = body_text(res).await;
        assert!(
            text.contains(
                r#"data: {"error":{"message":"upstream stalled: no data received for 120s","type":"api_error","code":"upstream_stall"}}"#
            ),
            "{text}"
        );
        let rows = wait_for_logs(&state, 1).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provider, "grok");
        assert_eq!(rows[0].status_code, 200);
        assert_eq!(rows[0].error_code.as_deref(), Some("upstream_stall"));
    }

    #[tokio::test]
    async fn the_default_idle_timeout_never_fires_on_a_short_complete_stream() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "short@example.com").await;
        seed_accounts(state.pool(), &user.id, "grok", &["acc_grok"]).await;
        let adapter = ScriptedAdapter::new(
            "grok",
            vec![Reply::Sse(
                StatusCode::OK,
                "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n".into(),
            )],
        );
        let res = dispatch_chat_completions(
            &state,
            ChatDispatchOptions { user_id: user.id.clone(), ..chat_opts(adapter, "grok", true) },
        )
        .await;
        assert!(body_text(res).await.contains("hi"));
        let rows = wait_for_logs(&state, 1).await;
        assert_eq!(rows[0].error_code, None);
    }

    #[tokio::test]
    async fn an_anthropic_stream_that_goes_silent_gets_the_anthropic_stall_event() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "astall@example.com").await;
        seed_accounts(state.pool(), &user.id, "claude-code", &["acc_cc"]).await;
        let adapter = ScriptedAdapter::new(
            "claude-code",
            vec![Reply::SilentSse(
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":5}}}\n\n".into(),
            )],
        )
        .for_messages();

        let res = dispatch_anthropic_messages(
            &state,
            AnthropicDispatchOptions {
                user_id: user.id.clone(),
                body: json!({ "model": "claude-opus-5", "max_tokens": 10, "messages": [], "stream": true })
                    .as_object()
                    .unwrap()
                    .clone(),
                model: "claude-code/claude-opus-5".into(),
                provider: Some("claude-code".into()),
                adapter: Some(adapter),
                idle_timeout: Some(Duration::from_millis(20)),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let text = body_text(res).await;
        assert!(text.contains("event: error"), "{text}");
        assert!(
            text.contains(r#""error":{"type":"overloaded_error","message":"upstream stalled: no data received for 120s"}"#),
            "{text}"
        );
        let rows = wait_for_logs(&state, 1).await;
        assert_eq!(rows[0].provider, "claude-code");
        assert_eq!(rows[0].status_code, 200);
        assert_eq!(rows[0].error_code.as_deref(), Some("upstream_stall"));
    }

    // -------------------------------------------------------------------------------
    // Pool outcomes
    // -------------------------------------------------------------------------------

    #[tokio::test]
    async fn zero_bound_accounts_is_a_400_no_upstream_account_with_no_retry_after() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "noacct@example.com").await;
        let adapter = ScriptedAdapter::new("grok", vec![]);
        let res = dispatch_chat_completions(
            &state,
            ChatDispatchOptions { user_id: user.id.clone(), ..chat_opts(adapter.clone(), "grok", false) },
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(res.headers().get(header::RETRY_AFTER).is_none());
        assert_eq!(res.headers().get("x-should-retry").unwrap(), "false");
        let body: Value = serde_json::from_str(&body_text(res).await).unwrap();
        assert_eq!(
            body,
            json!({"error":{"message":"No usable grok account for this user","type":"invalid_request_error","code":"no_upstream_account"}})
        );
        assert!(adapter.calls().is_empty());
        let rows = logs(&state).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status_code, 400);
        assert_eq!(rows[0].error_code.as_deref(), Some("no_upstream_account"));
    }

    #[tokio::test]
    async fn anthropic_zero_bound_accounts_is_the_anthropic_400_envelope() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "anoacct@example.com").await;
        let adapter = ScriptedAdapter::new("claude-code", vec![]).for_messages();
        let res = dispatch_anthropic_messages(
            &state,
            AnthropicDispatchOptions {
                user_id: user.id.clone(),
                body: json!({ "model": "claude-opus-5", "max_tokens": 10, "messages": [] })
                    .as_object()
                    .unwrap()
                    .clone(),
                model: "claude-code/claude-opus-5".into(),
                provider: Some("claude-code".into()),
                adapter: Some(adapter),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(res.headers().get(header::RETRY_AFTER).is_none());
        let body: Value = serde_json::from_str(&body_text(res).await).unwrap();
        assert_eq!(
            body,
            json!({"type":"error","error":{"type":"invalid_request_error","message":"No usable claude-code account"}})
        );
        let rows = logs(&state).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status_code, 400);
        assert_eq!(rows[0].error_code.as_deref(), Some("no_upstream_account"));
    }

    #[tokio::test]
    async fn an_all_benched_pool_is_a_503_with_retry_after_from_the_earliest_expiry() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "benched@example.com").await;
        seed_accounts(state.pool(), &user.id, "grok", &["acc_1", "acc_2"]).await;
        let now = now_ms();
        // acc_2's cooldown (45s) is earlier than acc_1's (120s) — the header must reflect the
        // EARLIEST expiry across the pool, not the first row.
        for (id, offset) in [("acc_1", 120_000i64), ("acc_2", 45_000)] {
            sqlx::query("UPDATE upstream_accounts SET bench_until = $1 WHERE id = $2")
                .bind(crate::db::accounts::iso_from_ms(now + offset))
                .bind(id)
                .execute(state.pool())
                .await
                .unwrap();
        }
        let adapter = ScriptedAdapter::new("grok", vec![]);
        let res = dispatch_chat_completions(
            &state,
            ChatDispatchOptions { user_id: user.id.clone(), ..chat_opts(adapter.clone(), "grok", false) },
        )
        .await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(res.headers().get(header::RETRY_AFTER).unwrap(), "45");
        assert_eq!(res.headers().get("x-should-retry").unwrap(), "true");
        let body: Value = serde_json::from_str(&body_text(res).await).unwrap();
        assert_eq!(body, json!({"error":{"message":"All upstream accounts unavailable","code":"upstream_unavailable"}}));
        assert!(adapter.calls().is_empty(), "a known-unusable candidate is never called");
        let rows = logs(&state).await;
        assert_eq!(rows[0].status_code, 503);
        assert_eq!(rows[0].error_code.as_deref(), Some("upstream_unavailable"));
    }

    #[tokio::test]
    async fn every_credential_undecryptable_is_a_503_without_retry_after() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "badcred@example.com").await;
        seed_accounts(state.pool(), &user.id, "grok", &["acc_bad"]).await;
        sqlx::query("UPDATE upstream_accounts SET encrypted_payload = '!!!not-valid!!!' WHERE id = 'acc_bad'")
            .execute(state.pool())
            .await
            .unwrap();
        let adapter = ScriptedAdapter::new("grok", vec![]);
        let res = dispatch_chat_completions(
            &state,
            ChatDispatchOptions { user_id: user.id.clone(), ..chat_opts(adapter.clone(), "grok", false) },
        )
        .await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(res.headers().get(header::RETRY_AFTER).is_none());
        assert!(adapter.calls().is_empty());
        let rows = logs(&state).await;
        assert_eq!(rows[0].status_code, 503);
        assert_eq!(rows[0].error_code.as_deref(), Some("upstream_unavailable"));
    }

    // -------------------------------------------------------------------------------
    // Feedback: benches, edge timeouts, 529, first-byte timeout
    // -------------------------------------------------------------------------------

    #[tokio::test]
    async fn a_402_benches_that_account_and_the_walk_succeeds_on_the_next_one() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "billing@example.com").await;
        seed_accounts(state.pool(), &user.id, "openrouter", &["acc_1", "acc_2"]).await;
        let adapter = ScriptedAdapter::new(
            "openrouter",
            vec![
                Reply::Json(
                    StatusCode::PAYMENT_REQUIRED,
                    json!({ "error": "insufficient_credits", "message": "Insufficient credits" }),
                ),
                Reply::Json(
                    StatusCode::OK,
                    json!({ "choices": [{ "message": { "role": "assistant", "content": "hi" }, "finish_reason": "stop" }] }),
                ),
            ],
        );
        let res = dispatch_chat_completions(
            &state,
            ChatDispatchOptions {
                user_id: user.id.clone(),
                is_builtin: Some(false),
                ..chat_opts(adapter.clone(), "openrouter", false)
            },
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(adapter.calls(), ["acc_1", "acc_2"]);
        assert!(is_benched(state.pool(), &user.id, "acc_1", now_ms()).await.unwrap());
        assert!(!is_benched(state.pool(), &user.id, "acc_2", now_ms()).await.unwrap());
        let rows = logs(&state).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].account_id.as_deref(), Some("acc_2"));
        assert_eq!(rows[0].status_code, 200);
    }

    #[tokio::test]
    async fn the_first_two_edge_timeouts_fail_over_without_benching_and_the_third_benches_thirty_seconds() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "edge@example.com").await;
        seed_accounts(state.pool(), &user.id, "grok", &["acc_1", "acc_2"]).await;
        // Mixed 522/524/520 statuses share one strike counter.
        let statuses = Arc::new(Mutex::new(vec![520u16, 524, 522]));
        let adapter = ScriptedAdapter::always("grok", move || {
            Reply::Text(StatusCode::from_u16(statuses.lock().unwrap().pop().unwrap()).unwrap(), "edge failed")
        });
        // Only acc_1 fails: the scripted adapter answers per call, so acc_2 must succeed —
        // wrap it with a second adapter that keys off the account id.
        struct EdgeAdapter {
            statuses: Mutex<Vec<u16>>,
            calls: Arc<Mutex<Vec<String>>>,
        }
        #[async_trait]
        impl ProviderAdapter for EdgeAdapter {
            fn id(&self) -> &str {
                "grok"
            }
            async fn chat_completions(
                &self,
                _: &AppState,
                account: &crate::pool::AcquiredAccount,
                _: &ChatCompletionRequest,
                _: &CallExtras,
            ) -> Result<Response, AdapterError> {
                self.calls.lock().unwrap().push(account.row.id.clone());
                if account.row.id == "acc_1" {
                    let status = self.statuses.lock().unwrap().pop().unwrap_or(524);
                    return Ok(Response::builder()
                        .status(StatusCode::from_u16(status).unwrap())
                        .body(Body::from("edge failed"))
                        .unwrap());
                }
                Ok(json_response(StatusCode::OK, &json!({ "choices": [{ "message": { "content": "ok" } }] })))
            }
        }
        drop(adapter);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let edge: Arc<dyn ProviderAdapter> =
            Arc::new(EdgeAdapter { statuses: Mutex::new(vec![520, 524, 522]), calls: calls.clone() });

        for expected_strikes in [1i32, 2] {
            let res = dispatch_chat_completions(
                &state,
                ChatDispatchOptions { user_id: user.id.clone(), ..chat_opts(edge.clone(), "grok", false) },
            )
            .await;
            assert_eq!(res.status(), StatusCode::OK);
            assert!(!is_benched(state.pool(), &user.id, "acc_1", now_ms()).await.unwrap());
            let row = get_account(state.pool(), &user.id, "acc_1").await.unwrap().unwrap();
            assert_eq!(row.edge_strikes, expected_strikes);
            assert_eq!(row.bench_until, None);
        }
        let res = dispatch_chat_completions(
            &state,
            ChatDispatchOptions { user_id: user.id.clone(), ..chat_opts(edge.clone(), "grok", false) },
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let row = get_account(state.pool(), &user.id, "acc_1").await.unwrap().unwrap();
        assert_eq!(row.edge_strikes, 0, "the third strike resets the counter");
        assert_eq!(row.bench_reason.as_deref(), Some("520"), "the last status is the bench reason");
        let until = crate::db::accounts::parse_iso_ms(row.bench_until.as_deref().unwrap()).unwrap();
        assert!(until > now_ms() && until - now_ms() <= 30_000);
        assert_eq!(
            *calls.lock().unwrap(),
            ["acc_1", "acc_2", "acc_1", "acc_2", "acc_1", "acc_2"],
            "every edge status fails over locally"
        );
    }

    #[tokio::test]
    async fn the_first_byte_timeout_fails_over_without_benching_and_logs_the_successful_status() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let mut config = crate::db::test_support::test_config();
        config.upstream_first_byte_timeout_ms = 20;
        let state = AppState::builder(config, pool).transport(MockTransport::new()).build();
        let user = insert_user(state.pool(), "ttfb@example.com").await;
        seed_accounts(state.pool(), &user.id, "grok", &["acc_1", "acc_2"]).await;

        struct HangingFirst {
            calls: Arc<Mutex<Vec<String>>>,
        }
        #[async_trait]
        impl ProviderAdapter for HangingFirst {
            fn id(&self) -> &str {
                "grok"
            }
            async fn chat_completions(
                &self,
                _: &AppState,
                account: &crate::pool::AcquiredAccount,
                _: &ChatCompletionRequest,
                _: &CallExtras,
            ) -> Result<Response, AdapterError> {
                self.calls.lock().unwrap().push(account.row.id.clone());
                if account.row.id == "acc_1" {
                    std::future::pending::<()>().await;
                }
                Ok(json_response(StatusCode::OK, &json!({ "choices": [{ "message": { "content": "ok" } }] })))
            }
        }
        let calls = Arc::new(Mutex::new(Vec::new()));
        let adapter: Arc<dyn ProviderAdapter> = Arc::new(HangingFirst { calls: calls.clone() });

        let res = dispatch_chat_completions(
            &state,
            ChatDispatchOptions { user_id: user.id.clone(), ..chat_opts(adapter, "grok", false) },
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(*calls.lock().unwrap(), ["acc_1", "acc_2"]);
        assert!(!is_benched(state.pool(), &user.id, "acc_1", now_ms()).await.unwrap());
        assert_eq!(logs(&state).await[0].upstream_status, Some(200));
    }

    #[tokio::test]
    async fn a_529_is_retried_once_on_the_same_account_and_then_passes_through_unbenched() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "overload@example.com").await;
        seed_accounts(state.pool(), &user.id, "grok", &["acc_grok"]).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let adapter = ScriptedAdapter::always("grok", move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Reply::Text(StatusCode::from_u16(529).unwrap(), "overloaded")
        });
        let res = dispatch_chat_completions(
            &state,
            ChatDispatchOptions { user_id: user.id.clone(), ..chat_opts(adapter, "grok", false) },
        )
        .await;
        assert_eq!(res.status().as_u16(), 529);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(!is_benched(state.pool(), &user.id, "acc_grok", now_ms()).await.unwrap());
        assert_eq!(logs(&state).await[0].upstream_status, Some(529));
    }

    // -------------------------------------------------------------------------------
    // Exhaustion and terminal errors
    // -------------------------------------------------------------------------------

    #[tokio::test]
    async fn two_429s_exhaust_the_pool_into_a_503_that_names_the_last_candidate() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "exhaust@example.com").await;
        seed_accounts(state.pool(), &user.id, "grok", &["acc_1", "acc_2"]).await;
        let adapter = ScriptedAdapter::always("grok", || {
            Reply::Json(StatusCode::TOO_MANY_REQUESTS, json!({ "error": { "message": "limited" } }))
        });
        let res = dispatch_chat_completions(
            &state,
            ChatDispatchOptions { user_id: user.id.clone(), ..chat_opts(adapter.clone(), "grok", false) },
        )
        .await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(res.headers().get(header::RETRY_AFTER).is_some());
        let body: Value = serde_json::from_str(&body_text(res).await).unwrap();
        assert_eq!(body["error"]["code"], "upstream_unavailable");
        let rows = logs(&state).await;
        assert_eq!(rows[0].account_id.as_deref(), Some("acc_2"));
        assert_eq!(rows[0].status_code, 503);
        assert_eq!(rows[0].error_code.as_deref(), Some("upstream_unavailable"));
        assert_eq!(rows[0].upstream_status, Some(429));
    }

    #[tokio::test]
    async fn two_429s_become_the_eager_streams_terminal_unavailable_frame() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "exhauststream@example.com").await;
        seed_accounts(state.pool(), &user.id, "grok", &["acc_1", "acc_2"]).await;
        let adapter = ScriptedAdapter::always("grok", || {
            Reply::Json(StatusCode::TOO_MANY_REQUESTS, json!({ "error": { "message": "limited" } }))
        });
        let res = dispatch_chat_completions(
            &state,
            ChatDispatchOptions { user_id: user.id.clone(), ..chat_opts(adapter, "grok", true) },
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(body_text(res).await.contains(r#""code":"upstream_unavailable""#));
        let rows = wait_for_logs(&state, 1).await;
        assert_eq!(rows[0].account_id.as_deref(), Some("acc_2"));
        assert_eq!(rows[0].error_code.as_deref(), Some("upstream_unavailable"));
    }

    #[tokio::test]
    async fn eight_consecutive_429s_hit_the_attempt_cap_and_still_carry_retry_after() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "cap@example.com").await;
        let ids: Vec<String> = (1..=8).map(|i| format!("acc_{i}")).collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        seed_accounts(state.pool(), &user.id, "grok", &refs).await;
        let adapter = ScriptedAdapter::always("grok", || {
            Reply::Json(StatusCode::TOO_MANY_REQUESTS, json!({ "error": "rate_limited" }))
        });
        let res = dispatch_chat_completions(
            &state,
            ChatDispatchOptions { user_id: user.id.clone(), ..chat_opts(adapter.clone(), "grok", false) },
        )
        .await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(res.headers().get(header::RETRY_AFTER).unwrap(), "60");
        assert_eq!(adapter.calls().len(), MAX_ATTEMPTS);
        for id in &ids {
            assert!(is_benched(state.pool(), &user.id, id, now_ms()).await.unwrap(), "{id}");
        }
        let rows = logs(&state).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status_code, 503);
    }

    #[tokio::test]
    async fn a_non_bench_400_passes_through_verbatim_without_trying_a_later_candidate() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "passthru@example.com").await;
        seed_accounts(state.pool(), &user.id, "grok", &["acc_1", "acc_2"]).await;
        let adapter = ScriptedAdapter::always("grok", || Reply::Text(StatusCode::BAD_REQUEST, "upstream says no"));
        let res = dispatch_chat_completions(
            &state,
            ChatDispatchOptions { user_id: user.id.clone(), ..chat_opts(adapter.clone(), "grok", false) },
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_text(res).await, "upstream says no");
        assert_eq!(adapter.calls().len(), 1);
    }

    #[tokio::test]
    async fn a_pre_output_adapter_failure_becomes_a_structured_sse_error_frame() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "adapterfail@example.com").await;
        seed_accounts(state.pool(), &user.id, "grok", &["acc_grok"]).await;
        let adapter = ScriptedAdapter::always("grok", || Reply::Error);
        let res = dispatch_chat_completions(
            &state,
            ChatDispatchOptions { user_id: user.id.clone(), ..chat_opts(adapter, "grok", true) },
        )
        .await;
        let text = body_text(res).await;
        assert!(text.contains(r#""type":"api_error""#), "{text}");
        let rows = wait_for_logs(&state, 1).await;
        assert_eq!(rows[0].error_code.as_deref(), Some("upstream_error"));
    }

    #[tokio::test]
    async fn an_adapter_failure_on_the_non_stream_path_is_a_502() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "fetcherr@example.com").await;
        seed_accounts(state.pool(), &user.id, "grok", &["acc_grok"]).await;
        let adapter = ScriptedAdapter::always("grok", || Reply::Error);
        let res = dispatch_chat_completions(
            &state,
            ChatDispatchOptions { user_id: user.id.clone(), ..chat_opts(adapter, "grok", false) },
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        let rows = logs(&state).await;
        assert_eq!(rows[0].status_code, 502);
        assert_eq!(rows[0].error_code.as_deref(), Some("upstream_error"));
    }

    #[tokio::test]
    async fn a_relay_413_becomes_a_non_retryable_request_too_large_stream_error() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "toolarge@example.com").await;
        seed_accounts(state.pool(), &user.id, "grok", &["acc_grok"]).await;
        let message = "Codex relay rejected the request body as too large; compact the conversation and retry";
        let adapter = ScriptedAdapter::always("grok", move || {
            Reply::WithHeaders(
                StatusCode::PAYLOAD_TOO_LARGE,
                json!({ "error": { "message": message, "type": "invalid_request_error", "code": "request_too_large" } }),
                vec![("x-kano-relay-error", "request_too_large".into())],
            )
        });
        let res = dispatch_chat_completions(
            &state,
            ChatDispatchOptions { user_id: user.id.clone(), ..chat_opts(adapter, "grok", true) },
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let text = body_text(res).await;
        assert!(text.contains(r#""type":"invalid_request_error""#), "{text}");
        assert!(text.contains(r#""code":"request_too_large""#), "{text}");
        assert!(text.contains(message), "{text}");
        let rows = wait_for_logs(&state, 1).await;
        assert_eq!(rows[0].status_code, 200);
        assert_eq!(rows[0].error_code.as_deref(), Some("request_too_large"));
        assert_eq!(rows[0].upstream_status, Some(413));
    }

    #[tokio::test]
    async fn a_relay_413_on_the_anthropic_surface_is_a_non_retryable_413() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "atoolarge@example.com").await;
        seed_accounts(state.pool(), &user.id, "claude-code", &["acc_cc"]).await;
        let message = "Codex relay rejected the request body as too large; compact the conversation and retry";
        let adapter = ScriptedAdapter::always("claude-code", move || {
            Reply::WithHeaders(
                StatusCode::PAYLOAD_TOO_LARGE,
                json!({ "error": { "message": message, "type": "invalid_request_error", "code": "request_too_large" } }),
                vec![("x-kano-relay-error", "request_too_large".into())],
            )
        })
        .for_messages();
        let res = dispatch_anthropic_messages(
            &state,
            AnthropicDispatchOptions {
                user_id: user.id.clone(),
                body: json!({ "model": "claude-opus-5", "max_tokens": 10, "messages": [] })
                    .as_object()
                    .unwrap()
                    .clone(),
                model: "claude-code/claude-opus-5".into(),
                provider: Some("claude-code".into()),
                adapter: Some(adapter),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(res.headers().get("x-should-retry").unwrap(), "false");
        let body: Value = serde_json::from_str(&body_text(res).await).unwrap();
        assert_eq!(
            body,
            json!({ "type": "error", "error": { "type": "invalid_request_error", "message": message } })
        );
        let rows = logs(&state).await;
        assert_eq!(rows[0].status_code, 413);
        assert_eq!(rows[0].error_code.as_deref(), Some("request_too_large"));
        assert_eq!(rows[0].upstream_status, Some(413));
    }

    // -------------------------------------------------------------------------------
    // Eager streaming commit
    // -------------------------------------------------------------------------------

    #[tokio::test]
    async fn stream_true_commits_200_and_sse_headers_before_a_slow_upstream_resolves() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "eager@example.com").await;
        seed_accounts(state.pool(), &user.id, "grok", &["acc_grok"]).await;
        let gate = Arc::new(tokio::sync::Notify::new());
        struct Gated {
            gate: Arc<tokio::sync::Notify>,
        }
        #[async_trait]
        impl ProviderAdapter for Gated {
            fn id(&self) -> &str {
                "grok"
            }
            async fn chat_completions(
                &self,
                _: &AppState,
                _: &crate::pool::AcquiredAccount,
                _: &ChatCompletionRequest,
                _: &CallExtras,
            ) -> Result<Response, AdapterError> {
                self.gate.notified().await;
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from(
                        "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n",
                    ))
                    .unwrap())
            }
        }
        let adapter: Arc<dyn ProviderAdapter> = Arc::new(Gated { gate: gate.clone() });
        let res = dispatch_chat_completions(
            &state,
            ChatDispatchOptions { user_id: user.id.clone(), ..chat_opts(adapter, "grok", true) },
        )
        .await;
        // Headers committed without waiting for the hung adapter.
        assert_eq!(res.status(), StatusCode::OK);
        assert!(res.headers().get(header::CONTENT_TYPE).unwrap().to_str().unwrap().contains("text/event-stream"));
        gate.notify_waiters();
        // The notify may land before the adapter parks, so keep nudging while draining.
        let drain = tokio::spawn(body_text(res));
        for _ in 0..50 {
            gate.notify_waiters();
            tokio::time::sleep(Duration::from_millis(5)).await;
            if drain.is_finished() {
                break;
            }
        }
        assert!(drain.await.unwrap().contains("hi"));
        wait_for_logs(&state, 1).await;
    }

    #[tokio::test]
    async fn stream_true_with_no_account_answers_200_with_an_in_stream_error_and_logs_status_200() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "eagernoacct@example.com").await;
        let adapter = ScriptedAdapter::always("grok", || panic!("must not be called"));
        let res = dispatch_chat_completions(
            &state,
            ChatDispatchOptions { user_id: user.id.clone(), ..chat_opts(adapter, "grok", true) },
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(res.headers().get(header::CONTENT_TYPE).unwrap().to_str().unwrap().contains("text/event-stream"));
        let text = body_text(res).await;
        assert!(text.contains("no_upstream_account"), "{text}");
        assert!(text.contains("invalid_request_error"), "{text}");
        let rows = wait_for_logs(&state, 1).await;
        assert_eq!(rows[0].provider, "grok");
        assert_eq!(rows[0].status_code, 200);
        assert_eq!(rows[0].error_code.as_deref(), Some("no_upstream_account"));
    }

    #[tokio::test]
    async fn anthropic_stream_true_with_no_account_answers_200_with_an_error_event() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "aeagernoacct@example.com").await;
        let adapter = ScriptedAdapter::always("claude-code", || panic!("must not be called")).for_messages();
        let res = dispatch_anthropic_messages(
            &state,
            AnthropicDispatchOptions {
                user_id: user.id.clone(),
                body: json!({ "model": "claude-opus-5", "max_tokens": 10, "messages": [], "stream": true })
                    .as_object()
                    .unwrap()
                    .clone(),
                model: "claude-code/claude-opus-5".into(),
                provider: Some("claude-code".into()),
                adapter: Some(adapter),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let text = body_text(res).await;
        assert!(text.contains("event: error"), "{text}");
        assert!(text.contains("invalid_request_error"), "{text}");
        assert!(text.contains("No usable claude-code account"), "{text}");
        let rows = wait_for_logs(&state, 1).await;
        assert_eq!(rows[0].provider, "claude-code");
        assert_eq!(rows[0].status_code, 200);
        assert_eq!(rows[0].error_code.as_deref(), Some("no_upstream_account"));
    }

    #[tokio::test]
    async fn a_client_that_leaves_mid_stream_still_produces_a_client_abort_row() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "abort@example.com").await;
        seed_accounts(state.pool(), &user.id, "grok", &["acc_grok"]).await;
        let adapter = ScriptedAdapter::new(
            "grok",
            vec![Reply::SilentSse("data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n".into())],
        );
        let res = dispatch_chat_completions(
            &state,
            ChatDispatchOptions { user_id: user.id.clone(), ..chat_opts(adapter, "grok", true) },
        )
        .await;
        let mut body = res.into_body().into_data_stream();
        // Read the first real chunk, then walk away as a client would.
        let mut seen = String::new();
        while let Some(chunk) = body.next().await {
            seen.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
            if seen.contains("delta") {
                break;
            }
        }
        drop(body);
        let rows = wait_for_logs(&state, 1).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status_code, 200);
        assert_eq!(rows[0].error_code.as_deref(), Some("client_abort"));
    }

    // -------------------------------------------------------------------------------
    // Limit-aware skips and cross-target failover
    // -------------------------------------------------------------------------------

    async fn set_snapshot(state: &AppState, id: &str, snapshot: Option<&str>) {
        sqlx::query("UPDATE upstream_accounts SET usage_snapshot_json = $1 WHERE id = $2")
            .bind(snapshot)
            .bind(id)
            .execute(state.pool())
            .await
            .unwrap();
    }

    fn exhausted_snapshot(resets_at_ms: i64) -> String {
        json!({
            "windows": [{ "label": "5h", "utilization": 100, "resets_at": crate::db::accounts::iso_from_ms(resets_at_ms) }],
            "error": null, "stale": false, "edgeBlocked": false
        })
        .to_string()
    }

    #[tokio::test]
    async fn an_exhausted_usage_window_skips_that_account_without_a_live_call() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "limited@example.com").await;
        seed_accounts(state.pool(), &user.id, "claude-code", &["acc_limited", "acc_ok"]).await;
        set_snapshot(&state, "acc_limited", Some(&exhausted_snapshot(now_ms() + 3_600_000))).await;
        let adapter = ScriptedAdapter::always("claude-code", || {
            Reply::Json(
                StatusCode::OK,
                json!({ "choices": [{ "message": { "role": "assistant", "content": "hi" }, "finish_reason": "stop" }] }),
            )
        });
        let res = dispatch_chat_completions(
            &state,
            ChatDispatchOptions { user_id: user.id.clone(), ..chat_opts(adapter.clone(), "claude-code", false) },
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(adapter.calls(), ["acc_ok"], "the limited account is never called");
    }

    #[tokio::test]
    async fn a_past_reset_or_malformed_snapshot_fails_open_and_the_account_is_tried() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "failopen@example.com").await;
        seed_accounts(state.pool(), &user.id, "claude-code", &["acc_1"]).await;
        for snapshot in [exhausted_snapshot(now_ms() - 3_600_000), "{not json".to_string()] {
            set_snapshot(&state, "acc_1", Some(&snapshot)).await;
            let adapter = ScriptedAdapter::always("claude-code", || {
                Reply::Json(StatusCode::OK, json!({ "choices": [{ "message": { "content": "hi" } }] }))
            });
            let res = dispatch_chat_completions(
                &state,
                ChatDispatchOptions { user_id: user.id.clone(), ..chat_opts(adapter.clone(), "claude-code", false) },
            )
            .await;
            assert_eq!(res.status(), StatusCode::OK, "{snapshot}");
            assert_eq!(adapter.calls(), ["acc_1"], "{snapshot}");
        }
    }

    #[tokio::test]
    async fn every_candidate_limited_is_a_503_with_retry_after_and_no_live_call() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "alllimited@example.com").await;
        seed_accounts(state.pool(), &user.id, "claude-code", &["acc_1"]).await;
        set_snapshot(&state, "acc_1", Some(&exhausted_snapshot(now_ms() + 1_800_000))).await;
        let adapter = ScriptedAdapter::always("claude-code", || panic!("a known-unusable candidate is never called"));
        let res = dispatch_chat_completions(
            &state,
            ChatDispatchOptions { user_id: user.id.clone(), ..chat_opts(adapter, "claude-code", false) },
        )
        .await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(res.headers().get(header::RETRY_AFTER).unwrap(), "60");
    }

    #[tokio::test]
    async fn the_flattened_candidate_list_fails_over_across_provider_boundaries() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "cross@example.com").await;
        let cc = seed_accounts(state.pool(), &user.id, "claude-code", &["acc_cc"]).await;
        let grok = seed_accounts(state.pool(), &user.id, "grok", &["acc_grok"]).await;
        let calls = Arc::new(Mutex::new(Vec::new()));

        struct Tagged {
            id: &'static str,
            status: StatusCode,
            calls: Arc<Mutex<Vec<String>>>,
        }
        #[async_trait]
        impl ProviderAdapter for Tagged {
            fn id(&self) -> &str {
                self.id
            }
            async fn chat_completions(
                &self,
                _: &AppState,
                account: &crate::pool::AcquiredAccount,
                _: &ChatCompletionRequest,
                _: &CallExtras,
            ) -> Result<Response, AdapterError> {
                self.calls.lock().unwrap().push(format!("{}:{}", self.id, account.row.id));
                Ok(json_response(self.status, &json!({ "choices": [{ "message": { "content": "hi" } }] })))
            }
        }
        let cc_adapter: DynAdapter =
            Arc::new(Tagged { id: "claude-code", status: StatusCode::TOO_MANY_REQUESTS, calls: calls.clone() });
        let grok_adapter: DynAdapter =
            Arc::new(Tagged { id: "grok", status: StatusCode::OK, calls: calls.clone() });

        let candidates = vec![
            RoutingCandidate {
                target_index: 0,
                pinned: false,
                provider: "claude-code".into(),
                upstream_model: "claude-opus-5".into(),
                is_builtin: true,
                custom_provider: None,
                adapter: cc_adapter,
                account: cc[0].clone(),
                share: None,
            },
            RoutingCandidate {
                target_index: 1,
                pinned: false,
                provider: "grok".into(),
                upstream_model: "grok-4.5".into(),
                is_builtin: true,
                custom_provider: None,
                adapter: grok_adapter,
                account: grok[0].clone(),
                share: None,
            },
        ];

        let res = dispatch_chat_completions(
            &state,
            ChatDispatchOptions {
                user_id: user.id.clone(),
                provider: "claude-code".into(),
                candidates: Some(candidates),
                group_name: Some("opus".into()),
                req: chat_req("opus", "claude-opus-5", false),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(*calls.lock().unwrap(), ["claude-code:acc_cc", "grok:acc_grok"]);
        assert!(is_benched(state.pool(), &user.id, "acc_cc", now_ms()).await.unwrap());
        let rows = logs(&state).await;
        assert_eq!(rows[0].group_name.as_deref(), Some("opus"));
        assert_eq!(rows[0].model, "grok/grok-4.5", "the row stores the expanded canonical target");
    }

    #[tokio::test]
    async fn omitting_the_strategy_behaves_exactly_like_passing_ordered() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "strategy@example.com").await;
        seed_accounts(state.pool(), &user.id, "grok", &["acc_1", "acc_2"]).await;
        let mut seen = Vec::new();
        for strategy in [None, Some("ordered".to_string())] {
            let adapter = ScriptedAdapter::always("grok", || {
                Reply::Json(StatusCode::OK, json!({ "choices": [{ "message": { "content": "ok" } }] }))
            });
            let res = dispatch_chat_completions(
                &state,
                ChatDispatchOptions {
                    user_id: user.id.clone(),
                    strategy,
                    ..chat_opts(adapter.clone(), "grok", false)
                },
            )
            .await;
            seen.push((adapter.calls(), body_text(res).await));
        }
        assert_eq!(seen[0], seen[1]);
    }

    #[tokio::test]
    async fn an_adapter_without_the_endpoint_is_rejected_before_the_pool_is_touched() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "noendpoint@example.com").await;
        seed_accounts(state.pool(), &user.id, "claude-code", &["acc_cc"]).await;
        // A chat-only adapter has no `messages`.
        let adapter = ScriptedAdapter::always("claude-code", || panic!("must not be called"));
        let res = dispatch_anthropic_messages(
            &state,
            AnthropicDispatchOptions {
                user_id: user.id.clone(),
                body: json!({ "model": "claude-opus-5", "max_tokens": 10, "messages": [] })
                    .as_object()
                    .unwrap()
                    .clone(),
                model: "claude-code/claude-opus-5".into(),
                provider: Some("claude-code".into()),
                adapter: Some(adapter),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body: Value = serde_json::from_str(&body_text(res).await).unwrap();
        assert_eq!(body["error"]["message"], "messages not supported");
        assert!(logs(&state).await.is_empty(), "the pool was never touched, so no row is written");
    }
}
