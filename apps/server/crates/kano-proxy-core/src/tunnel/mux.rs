//! the agent tunnel's request
//! multiplexer (docs/cli.md § Wire protocol / § Failover semantics).
//!
//! One mux serves one live socket. In-flight state is memory-only by design: if
//! the socket drops, the CLI aborts its local requests and every open request
//! here faults; frames that arrive for an id nobody awaits are answered with
//! `cancel` so the CLI stops streaming into the void. The transport is behind
//! [`MuxSocket`] so tests drive both ends with in-memory frames, exactly as the
//! vitest suite drove the TypeScript mux.

use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::{Stream, StreamExt};
use http::{HeaderMap, HeaderValue, StatusCode};
use serde_json::json;

use super::protocol::{
    decode_binary_frame, encode_binary_frame, encode_control_frame, fault_from_res_err_reason,
    parse_control_frame, validate_models_report, AgentFaultReason, ControlFrame, BODY_KIND_REQUEST,
    BODY_KIND_RESPONSE, FIRST_RES_TIMEOUT_MS, MAX_CHUNK_BYTES, MAX_INFLIGHT,
    REQUEST_BODY_LIMIT_BYTES, RESPONSE_BUFFER_LIMIT_BYTES,
};
use crate::upstream::transport::ByteStream;
use crate::upstream::UpstreamResponse;

pub const AGENT_UPSTREAM_HEADER: &str = "x-agent-upstream";
pub const AGENT_FAULT_HEADER: &str = "x-agent-fault";

/// A tunnel-level failure: 502 with `x-agent-fault: <reason>`, never an upstream
/// answer (docs/cli.md § Failover semantics — the tri-state guard).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TunnelFault {
    pub reason: AgentFaultReason,
}

impl TunnelFault {
    pub fn new(reason: AgentFaultReason) -> Self {
        Self { reason }
    }
    pub fn into_upstream_response(self) -> UpstreamResponse {
        agent_fault_response(self.reason)
    }
}

impl std::fmt::Display for TunnelFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "agent tunnel fault: {}", self.reason)
    }
}

impl std::error::Error for TunnelFault {}

impl axum::response::IntoResponse for TunnelFault {
    fn into_response(self) -> axum::response::Response {
        let body = json!({ "error": { "type": "agent_fault", "reason": self.reason.as_str() } });
        let mut response = (StatusCode::BAD_GATEWAY, axum::Json(body)).into_response();
        response
            .headers_mut()
            .insert(AGENT_FAULT_HEADER, HeaderValue::from_static(self.reason.as_str()));
        response
    }
}

/// The fault answer the routing layer reads markers off (`agentFaultResponse`).
pub fn agent_fault_response(reason: AgentFaultReason) -> UpstreamResponse {
    let mut headers = HeaderMap::new();
    headers.insert(AGENT_FAULT_HEADER, HeaderValue::from_static(reason.as_str()));
    headers.insert(http::header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let body = json!({ "error": { "type": "agent_fault", "reason": reason.as_str() } });
    UpstreamResponse::from_bytes(StatusCode::BAD_GATEWAY, headers, serde_json::to_vec(&body).expect("json"))
}

/// One outbound WebSocket frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutFrame {
    Text(String),
    Binary(Vec<u8>),
}

/// The socket seam (`MuxSocket` in TypeScript): `ws.send`, with a dead socket
/// reported instead of thrown.
pub trait MuxSocket: Send + Sync + 'static {
    fn send(&self, frame: OutFrame) -> Result<(), SocketClosed>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketClosed;

/// A valid `models` frame arrived — persist the report (docs/cli.md § Model
/// catalog). The db layer implements this; the mux never writes SQL.
#[async_trait]
pub trait ModelsReportHook: Send + Sync + 'static {
    async fn on_models_report(&self, models: Vec<String>) -> anyhow::Result<()>;
}

/// What a received frame asked of the socket owner. `ModelsPersistFailed` is the
/// 4008 retryable close: the CLI already marked the list as sent, so the
/// reconnect must re-report from a clean slate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageOutcome {
    Handled,
    ModelsPersistFailed,
}

/// An inbound WebSocket message (heartbeats are answered before this).
#[derive(Debug, Clone)]
pub enum MuxMessage {
    Text(String),
    Binary(Bytes),
}

pub struct OpenRequest {
    pub method: String,
    pub path: String,
    pub headers: std::collections::BTreeMap<String, String>,
    pub body: Option<ByteStream>,
}

type Settle = tokio::sync::oneshot::Sender<Result<UpstreamResponse, TunnelFault>>;
type BodySender = tokio::sync::mpsc::UnboundedSender<Result<Bytes, io::Error>>;

struct Pending {
    settle: Option<Settle>,
    body_tx: Option<BodySender>,
    buffered: Arc<AtomicUsize>,
    first_res_timer: Option<tokio::task::JoinHandle<()>>,
}

struct Inner {
    socket: Arc<dyn MuxSocket>,
    hook: Option<Arc<dyn ModelsReportHook>>,
    first_res_timeout: Duration,
    pending: Mutex<HashMap<u32, Pending>>,
    next_id: AtomicU32,
}

/// The multiplexer. Cheap to clone; every clone drives the same socket.
#[derive(Clone)]
pub struct TunnelMux {
    inner: Arc<Inner>,
}

impl TunnelMux {
    pub fn new(socket: Arc<dyn MuxSocket>) -> Self {
        Self::with_options(socket, None, Duration::from_millis(FIRST_RES_TIMEOUT_MS))
    }

    pub fn with_options(
        socket: Arc<dyn MuxSocket>,
        hook: Option<Arc<dyn ModelsReportHook>>,
        first_res_timeout: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                socket,
                hook,
                first_res_timeout,
                pending: Mutex::new(HashMap::new()),
                next_id: AtomicU32::new(1),
            }),
        }
    }

    pub fn inflight_count(&self) -> usize {
        self.inner.pending.lock().expect("mux lock").len()
    }

    /// Opens one proxied request over the socket. Resolves with either the local
    /// server's answer (marked `x-agent-upstream: 1`, status passed through) or a
    /// tunnel fault. The caller has already enforced the path allowlist and
    /// reduced the headers.
    pub async fn open_request(&self, opts: OpenRequest) -> Result<UpstreamResponse, TunnelFault> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let id = {
            let mut pending = self.inner.pending.lock().expect("mux lock");
            if pending.len() >= MAX_INFLIGHT {
                return Err(TunnelFault::new(AgentFaultReason::Busy));
            }
            let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
            pending.insert(
                id,
                Pending { settle: Some(tx), body_tx: None, buffered: Arc::new(AtomicUsize::new(0)), first_res_timer: None },
            );
            id
        };
        let frame = ControlFrame::Req {
            id,
            method: opts.method,
            path: opts.path,
            headers: opts.headers,
        };
        if self.inner.socket.send(OutFrame::Text(encode_control_frame(&frame))).is_err() {
            self.inner.pending.lock().expect("mux lock").remove(&id);
            return Err(TunnelFault::new(AgentFaultReason::Offline));
        }
        let inner = self.inner.clone();
        tokio::spawn(async move { pump_request_body(inner, id, opts.body).await });
        match rx.await {
            Ok(result) => result,
            // The entry was dropped without settling (only reachable through a
            // `res_end` for a request that never saw `res`); the TypeScript
            // promise hung there, a fault is the honest answer.
            Err(_) => Err(TunnelFault::new(AgentFaultReason::Protocol)),
        }
    }

    /// Socket gone (close/error/replaced/expired): every open request faults.
    pub fn abort_all(&self, reason: AgentFaultReason) {
        let ids: Vec<u32> = self.inner.pending.lock().expect("mux lock").keys().copied().collect();
        for id in ids {
            self.inner.fail_pending(id, reason, false);
        }
    }

    /// Handles one inbound frame. Only a models report does async work (the
    /// catalog write), which is awaited so the socket owner can close 4008 when
    /// it fails.
    pub async fn handle_message(&self, message: MuxMessage) -> MessageOutcome {
        match message {
            MuxMessage::Binary(data) => {
                let Some(frame) = decode_binary_frame(&data) else { return MessageOutcome::Handled };
                if frame.kind != BODY_KIND_RESPONSE {
                    return MessageOutcome::Handled;
                }
                // The 1 MiB per-frame bound is a protocol rule, not a
                // suggestion: an oversized frame from a malformed agent must not
                // reach the queue (the 8 MiB backpressure check only runs after
                // an enqueue).
                if frame.chunk.len() > MAX_CHUNK_BYTES {
                    self.inner.fail_pending(frame.id, AgentFaultReason::Protocol, true);
                    return MessageOutcome::Handled;
                }
                let (id, chunk) = (frame.id, Bytes::copy_from_slice(frame.chunk));
                self.inner.handle_response_chunk(id, chunk);
                MessageOutcome::Handled
            }
            MuxMessage::Text(text) => {
                let Some(frame) = parse_control_frame(&text) else { return MessageOutcome::Handled };
                match frame {
                    ControlFrame::Res { id, status, headers } => {
                        self.inner.handle_res(id, status, &headers);
                        MessageOutcome::Handled
                    }
                    ControlFrame::ResEnd { id } => {
                        self.inner.handle_res_end(id);
                        MessageOutcome::Handled
                    }
                    ControlFrame::ResErr { id, reason } => {
                        self.inner.fail_pending(id, fault_from_res_err_reason(&reason), false);
                        MessageOutcome::Handled
                    }
                    // The CLI's local abort raced a partial response — drop it like res_err.
                    ControlFrame::Cancel { id } => {
                        self.inner.fail_pending(id, AgentFaultReason::Protocol, false);
                        MessageOutcome::Handled
                    }
                    ControlFrame::Models { models } => {
                        // An out-of-bounds report is ignored whole — the last good report stays.
                        let Some(models) = validate_models_report(&models) else {
                            return MessageOutcome::Handled;
                        };
                        let Some(hook) = self.inner.hook.clone() else { return MessageOutcome::Handled };
                        match hook.on_models_report(models).await {
                            Ok(()) => MessageOutcome::Handled,
                            Err(error) => {
                                tracing::error!(%error, "agent tunnel: models report write failed");
                                MessageOutcome::ModelsPersistFailed
                            }
                        }
                    }
                    ControlFrame::Hello { .. } | ControlFrame::Req { .. } | ControlFrame::ReqEnd { .. } => {
                        MessageOutcome::Handled
                    }
                }
            }
        }
    }
}

impl Inner {
    fn has_pending(&self, id: u32) -> bool {
        self.pending.lock().expect("mux lock").contains_key(&id)
    }

    fn try_send_cancel(&self, id: u32) {
        let _ = self.socket.send(OutFrame::Text(encode_control_frame(&ControlFrame::Cancel { id })));
    }

    fn fail_pending(&self, id: u32, reason: AgentFaultReason, send_cancel: bool) {
        let entry = { self.pending.lock().expect("mux lock").remove(&id) };
        let Some(mut entry) = entry else { return };
        if let Some(timer) = entry.first_res_timer.take() {
            timer.abort();
        }
        if send_cancel {
            self.try_send_cancel(id);
        }
        match entry.settle.take() {
            Some(settle) => {
                let _ = settle.send(Err(TunnelFault::new(reason)));
            }
            None => {
                if let Some(tx) = entry.body_tx.take() {
                    let _ = tx.send(Err(io::Error::other(format!("agent tunnel: {reason}"))));
                }
            }
        }
    }

    fn handle_res(self: &Arc<Self>, id: u32, status: u16, headers: &std::collections::BTreeMap<String, String>) {
        let mut guard = self.pending.lock().expect("mux lock");
        if !guard.contains_key(&id) {
            // A response for a request nobody awaits — tell the CLI to stop streaming it.
            drop(guard);
            self.try_send_cancel(id);
            return;
        }
        let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
        // Statuses that must not carry a body get none; the CLI still sends
        // res_end, which is then tolerated as an unknown id.
        let bodyless = matches!(status.as_u16(), 204 | 205 | 304);

        let settle;
        let stream_parts;
        {
            let entry = guard.get_mut(&id).expect("checked above");
            match entry.settle.take() {
                None => return,
                Some(sender) => settle = sender,
            }
            if let Some(timer) = entry.first_res_timer.take() {
                timer.abort();
            }
            if bodyless {
                stream_parts = None;
            } else {
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                entry.body_tx = Some(tx);
                stream_parts = Some((rx, entry.buffered.clone()));
            }
        }
        if bodyless {
            guard.remove(&id);
        }
        drop(guard);

        let mut response_headers = HeaderMap::new();
        response_headers.insert(AGENT_UPSTREAM_HEADER, HeaderValue::from_static("1"));
        // Header reduction discipline (docs/cli.md): content-type only.
        if let Some(value) = headers.get("content-type").and_then(|v| HeaderValue::from_str(v).ok()) {
            response_headers.insert(http::header::CONTENT_TYPE, value);
        }

        let response = match stream_parts {
            None => UpstreamResponse::from_bytes(status, response_headers, Bytes::new()),
            Some((rx, buffered)) => {
                let body: ByteStream = Box::pin(ResponseBody { rx, buffered, mux: self.clone(), id, done: false });
                UpstreamResponse::new(status, response_headers, body)
            }
        };
        let _ = settle.send(Ok(response));
    }

    fn handle_res_end(&self, id: u32) {
        // Dropping the entry closes the body channel, which ends the stream.
        if let Some(mut entry) = self.pending.lock().expect("mux lock").remove(&id) {
            if let Some(timer) = entry.first_res_timer.take() {
                timer.abort();
            }
        }
    }

    fn handle_response_chunk(&self, id: u32, chunk: Bytes) {
        let (tx, buffered) = {
            let mut guard = self.pending.lock().expect("mux lock");
            if !guard.contains_key(&id) {
                drop(guard);
                self.try_send_cancel(id);
                return;
            }
            let entry = guard.get_mut(&id).expect("checked above");
            // No `res` yet: the CLI is streaming ahead of its own header frame.
            let Some(tx) = entry.body_tx.as_ref() else { return };
            (tx.clone(), entry.buffered.clone())
        };
        let len = chunk.len();
        if tx.send(Ok(chunk)).is_err() {
            // The reader is gone; its drop guard already cancelled down the tunnel.
            self.pending.lock().expect("mux lock").remove(&id);
            return;
        }
        // The end client reads slower than the CLI sends and the gap passed the
        // cap: cancel rather than buffer without bound (docs/cli.md — `too_large`).
        if buffered.fetch_add(len, Ordering::SeqCst) + len > RESPONSE_BUFFER_LIMIT_BYTES {
            self.fail_pending(id, AgentFaultReason::TooLarge, true);
        }
    }

    /// End client disconnected while the response streamed — propagate the abort.
    fn on_response_cancelled(&self, id: u32) {
        let entry = { self.pending.lock().expect("mux lock").remove(&id) };
        let Some(mut entry) = entry else { return };
        if let Some(timer) = entry.first_res_timer.take() {
            timer.abort();
        }
        self.try_send_cancel(id);
    }
}

async fn pump_request_body(inner: Arc<Inner>, id: u32, body: Option<ByteStream>) {
    if let Some(mut body) = body {
        let mut sent: usize = 0;
        while let Some(chunk) = body.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                // Reading the client's request body failed (usually a client
                // abort) — tell the CLI to drop the request rather than leaving
                // it half-sent.
                Err(_) => return inner.fail_pending(id, AgentFaultReason::Protocol, true),
            };
            if !inner.has_pending(id) {
                return;
            }
            // ws.send has no drain signal, so a body arriving faster than the
            // socket empties would queue unboundedly — the hard cap is the
            // honest request-side bound (docs/cli.md).
            sent += chunk.len();
            if sent > REQUEST_BODY_LIMIT_BYTES {
                return inner.fail_pending(id, AgentFaultReason::TooLarge, true);
            }
            for part in chunk.chunks(MAX_CHUNK_BYTES) {
                if inner.socket.send(OutFrame::Binary(encode_binary_frame(id, BODY_KIND_REQUEST, part))).is_err() {
                    return inner.fail_pending(id, AgentFaultReason::Offline, false);
                }
            }
        }
    }
    if !inner.has_pending(id) {
        return;
    }
    if inner.socket.send(OutFrame::Text(encode_control_frame(&ControlFrame::ReqEnd { id }))).is_err() {
        return inner.fail_pending(id, AgentFaultReason::Offline, false);
    }
    let timer = {
        let timeout = inner.first_res_timeout;
        let inner = inner.clone();
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            inner.fail_pending(id, AgentFaultReason::Timeout, true);
        })
    };
    let mut guard = inner.pending.lock().expect("mux lock");
    match guard.get_mut(&id) {
        Some(entry) => entry.first_res_timer = Some(timer),
        None => timer.abort(),
    }
}

/// The response body handed to the caller: chunks as the CLI sends them, an
/// error when the tunnel faults mid-stream, and a `cancel` frame down the tunnel
/// if the end client stops reading.
struct ResponseBody {
    rx: tokio::sync::mpsc::UnboundedReceiver<Result<Bytes, io::Error>>,
    buffered: Arc<AtomicUsize>,
    mux: Arc<Inner>,
    id: u32,
    done: bool,
}

impl Stream for ResponseBody {
    type Item = Result<Bytes, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.rx.poll_recv(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                let len = chunk.len();
                let _ = this.buffered.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| Some(v.saturating_sub(len)));
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Err(error))) => {
                this.done = true;
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                this.done = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for ResponseBody {
    fn drop(&mut self) {
        if !self.done {
            self.mux.on_response_cancelled(self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// The vitest `recordingSocket`: every frame the mux sends, kept for assertions.
    #[derive(Default)]
    struct RecordingSocket {
        sent: Mutex<Vec<OutFrame>>,
        dead: std::sync::atomic::AtomicBool,
    }

    impl RecordingSocket {
        fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }
        fn controls(&self, t: &str) -> Vec<Value> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .filter_map(|f| match f {
                    OutFrame::Text(text) => serde_json::from_str::<Value>(text).ok(),
                    OutFrame::Binary(_) => None,
                })
                .filter(|v| v.get("t").and_then(Value::as_str) == Some(t))
                .collect()
        }
        fn binaries(&self) -> Vec<(u32, u8, Vec<u8>)> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .filter_map(|f| match f {
                    OutFrame::Binary(data) => {
                        let frame = decode_binary_frame(data).unwrap();
                        Some((frame.id, frame.kind, frame.chunk.to_vec()))
                    }
                    OutFrame::Text(_) => None,
                })
                .collect()
        }
        fn kill(&self) {
            self.dead.store(true, Ordering::SeqCst);
        }
    }

    impl MuxSocket for RecordingSocket {
        fn send(&self, frame: OutFrame) -> Result<(), SocketClosed> {
            if self.dead.load(Ordering::SeqCst) {
                return Err(SocketClosed);
            }
            self.sent.lock().unwrap().push(frame);
            Ok(())
        }
    }

    struct RecordingHook {
        reports: Mutex<Vec<Vec<String>>>,
        fail: bool,
        delay: Duration,
    }

    impl RecordingHook {
        fn new() -> Arc<Self> {
            Arc::new(Self { reports: Mutex::new(Vec::new()), fail: false, delay: Duration::ZERO })
        }
        fn failing() -> Arc<Self> {
            Arc::new(Self { reports: Mutex::new(Vec::new()), fail: true, delay: Duration::ZERO })
        }
        fn slow() -> Arc<Self> {
            Arc::new(Self { reports: Mutex::new(Vec::new()), fail: false, delay: Duration::from_millis(20) })
        }
        fn reports(&self) -> Vec<Vec<String>> {
            self.reports.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ModelsReportHook for RecordingHook {
        async fn on_models_report(&self, models: Vec<String>) -> anyhow::Result<()> {
            tokio::time::sleep(self.delay).await;
            if self.fail {
                anyhow::bail!("d1 write failed");
            }
            self.reports.lock().unwrap().push(models);
            Ok(())
        }
    }

    fn expect_fault(result: Result<UpstreamResponse, TunnelFault>) -> TunnelFault {
        match result {
            Err(fault) => fault,
            Ok(response) => panic!("expected a tunnel fault, got {}", response.status),
        }
    }

    fn expect_ok(result: Result<UpstreamResponse, TunnelFault>) -> UpstreamResponse {
        match result {
            Ok(response) => response,
            Err(fault) => panic!("expected a response, got fault {}", fault.reason),
        }
    }

    fn mux_with(socket: Arc<RecordingSocket>) -> TunnelMux {
        TunnelMux::new(socket)
    }

    fn post(path: &str, body: Option<ByteStream>) -> OpenRequest {
        OpenRequest { method: "POST".into(), path: path.into(), headers: Default::default(), body }
    }

    fn body_of(chunks: Vec<&'static [u8]>) -> Option<ByteStream> {
        Some(Box::pin(futures::stream::iter(chunks.into_iter().map(|c| Ok(Bytes::from_static(c))))))
    }

    fn text_frame(value: Value) -> MuxMessage {
        MuxMessage::Text(value.to_string())
    }

    fn binary_frame(id: u32, kind: u8, chunk: &[u8]) -> MuxMessage {
        MuxMessage::Binary(Bytes::from(encode_binary_frame(id, kind, chunk)))
    }

    /// Lets the spawned body pump run, the vitest `flush()`.
    async fn flush() {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    async fn collect(response: UpstreamResponse) -> Result<String, io::Error> {
        let mut body = response.body;
        let mut out = Vec::new();
        while let Some(chunk) = body.next().await {
            out.extend_from_slice(&chunk?);
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    fn open(mux: &TunnelMux, req: OpenRequest) -> tokio::task::JoinHandle<Result<UpstreamResponse, TunnelFault>> {
        let mux = mux.clone();
        tokio::spawn(async move { mux.open_request(req).await })
    }

    #[tokio::test]
    async fn sends_req_body_and_req_end_then_streams_the_response_through() {
        let socket = RecordingSocket::new();
        let mux = mux_with(socket.clone());
        let handle = open(&mux, post("/chat/completions", body_of(vec![b"request-body"])));
        flush().await;

        let reqs = socket.controls("req");
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0]["id"], 1);
        assert_eq!(reqs[0]["method"], "POST");
        assert_eq!(reqs[0]["path"], "/chat/completions");
        assert_eq!(socket.binaries().iter().map(|b| b.1).collect::<Vec<_>>(), vec![BODY_KIND_REQUEST]);
        assert_eq!(socket.controls("req_end").len(), 1);

        mux.handle_message(text_frame(
            json!({"t":"res","id":1,"status":200,"headers":{"content-type":"text/event-stream"}}),
        ))
        .await;
        let response = expect_ok(handle.await.unwrap());
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.headers.get(AGENT_UPSTREAM_HEADER).unwrap(), "1");
        assert_eq!(response.content_type(), Some("text/event-stream"));

        mux.handle_message(binary_frame(1, BODY_KIND_RESPONSE, b"data: hi\n\n")).await;
        mux.handle_message(text_frame(json!({"t":"res_end","id":1}))).await;
        assert_eq!(collect(response).await.unwrap(), "data: hi\n\n");
        assert_eq!(mux.inflight_count(), 0);
    }

    #[tokio::test]
    async fn passes_a_local_error_status_through_as_a_real_upstream_answer() {
        let socket = RecordingSocket::new();
        let mux = mux_with(socket.clone());
        let handle = open(&mux, post("/chat/completions", None));
        flush().await;
        mux.handle_message(text_frame(
            json!({"t":"res","id":1,"status":429,"headers":{"content-type":"application/json"}}),
        ))
        .await;
        mux.handle_message(binary_frame(1, BODY_KIND_RESPONSE, b"{}")).await;
        mux.handle_message(text_frame(json!({"t":"res_end","id":1}))).await;
        let response = expect_ok(handle.await.unwrap());
        assert_eq!(response.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers.get(AGENT_UPSTREAM_HEADER).unwrap(), "1");
        assert!(response.headers.get(AGENT_FAULT_HEADER).is_none());
        assert_eq!(collect(response).await.unwrap(), "{}");
    }

    #[tokio::test]
    async fn refuses_the_fifth_concurrent_request_with_fault_busy() {
        let socket = RecordingSocket::new();
        let mux = mux_with(socket.clone());
        for _ in 0..MAX_INFLIGHT {
            open(&mux, post("/chat/completions", None));
        }
        flush().await;
        let fault = expect_fault(mux.open_request(post("/chat/completions", None)).await);
        assert_eq!(fault.reason, AgentFaultReason::Busy);
        let response = fault.into_upstream_response();
        assert_eq!(response.status, StatusCode::BAD_GATEWAY);
        assert_eq!(response.headers.get(AGENT_FAULT_HEADER).unwrap(), "busy");
    }

    #[tokio::test]
    async fn maps_res_err_connect_refused_to_fault_offline() {
        let socket = RecordingSocket::new();
        let mux = mux_with(socket.clone());
        let handle = open(&mux, post("/v1/messages", None));
        flush().await;
        mux.handle_message(text_frame(json!({"t":"res_err","id":1,"reason":"connect_refused"}))).await;
        assert_eq!(expect_fault(handle.await.unwrap()).reason, AgentFaultReason::Offline);
    }

    #[tokio::test]
    async fn faults_timeout_when_no_res_arrives_after_req_end() {
        let socket = RecordingSocket::new();
        let mux = TunnelMux::with_options(socket.clone(), None, Duration::from_millis(50));
        let handle = open(&mux, post("/chat/completions", None));
        let fault = expect_fault(handle.await.unwrap());
        assert_eq!(fault.reason, AgentFaultReason::Timeout);
        assert_eq!(socket.controls("cancel").len(), 1);
        assert_eq!(socket.controls("cancel")[0]["id"], 1);
        assert_eq!(mux.inflight_count(), 0);
    }

    #[tokio::test]
    async fn abort_all_faults_every_open_request() {
        let socket = RecordingSocket::new();
        let mux = mux_with(socket.clone());
        let a = open(&mux, post("/chat/completions", None));
        let b = open(&mux, post("/chat/completions", None));
        flush().await;
        mux.abort_all(AgentFaultReason::Offline);
        assert_eq!(expect_fault(a.await.unwrap()).reason, AgentFaultReason::Offline);
        assert_eq!(expect_fault(b.await.unwrap()).reason, AgentFaultReason::Offline);
        // abortAll never sends cancel: the socket is already gone.
        assert!(socket.controls("cancel").is_empty());
    }

    #[tokio::test]
    async fn errors_an_in_flight_stream_when_the_socket_dies_mid_response() {
        let socket = RecordingSocket::new();
        let mux = mux_with(socket.clone());
        let handle = open(&mux, post("/chat/completions", None));
        flush().await;
        mux.handle_message(text_frame(json!({"t":"res","id":1,"status":200,"headers":{}}))).await;
        let response = expect_ok(handle.await.unwrap());
        mux.handle_message(binary_frame(1, BODY_KIND_RESPONSE, b"partial")).await;
        mux.abort_all(AgentFaultReason::Offline);
        let error = collect(response).await.unwrap_err();
        assert!(error.to_string().contains("offline"));
    }

    #[tokio::test]
    async fn refuses_an_inbound_binary_frame_past_the_one_mib_bound() {
        let socket = RecordingSocket::new();
        let mux = mux_with(socket.clone());
        let handle = open(&mux, post("/chat/completions", None));
        flush().await;
        mux.handle_message(binary_frame(1, BODY_KIND_RESPONSE, &vec![0u8; MAX_CHUNK_BYTES + 1])).await;
        assert_eq!(expect_fault(handle.await.unwrap()).reason, AgentFaultReason::Protocol);
        assert_eq!(mux.inflight_count(), 0);
    }

    #[tokio::test]
    async fn cancels_a_response_nobody_awaits() {
        let socket = RecordingSocket::new();
        let mux = mux_with(socket.clone());
        mux.handle_message(text_frame(json!({"t":"res","id":42,"status":200,"headers":{}}))).await;
        let cancels = socket.controls("cancel");
        assert_eq!(cancels.len(), 1);
        assert_eq!(cancels[0]["id"], 42);
        // So is a body chunk for a forgotten id.
        mux.handle_message(binary_frame(43, BODY_KIND_RESPONSE, b"x")).await;
        assert_eq!(socket.controls("cancel").len(), 2);
    }

    #[tokio::test]
    async fn cancels_down_the_tunnel_when_the_client_stops_reading_past_the_buffer_cap() {
        let socket = RecordingSocket::new();
        let mux = mux_with(socket.clone());
        let handle = open(&mux, post("/chat/completions", None));
        flush().await;
        mux.handle_message(text_frame(json!({"t":"res","id":1,"status":200,"headers":{}}))).await;
        let response = expect_ok(handle.await.unwrap());
        // 9 x 1 MiB unread chunks exceed the 8 MiB bound.
        let chunk = vec![0u8; MAX_CHUNK_BYTES];
        for _ in 0..9 {
            mux.handle_message(binary_frame(1, BODY_KIND_RESPONSE, &chunk)).await;
        }
        let cancels = socket.controls("cancel");
        assert_eq!(cancels.len(), 1);
        assert_eq!(cancels[0]["id"], 1);
        assert!(collect(response).await.is_err());
        assert_eq!(mux.inflight_count(), 0);
    }

    #[tokio::test]
    async fn a_reading_client_never_trips_the_buffer_cap() {
        let socket = RecordingSocket::new();
        let mux = mux_with(socket.clone());
        let handle = open(&mux, post("/chat/completions", None));
        flush().await;
        mux.handle_message(text_frame(json!({"t":"res","id":1,"status":200,"headers":{}}))).await;
        let mut response = expect_ok(handle.await.unwrap());
        let chunk = vec![7u8; MAX_CHUNK_BYTES];
        let mut total = 0usize;
        for _ in 0..16 {
            mux.handle_message(binary_frame(1, BODY_KIND_RESPONSE, &chunk)).await;
            total += response.body.next().await.unwrap().unwrap().len();
        }
        assert_eq!(total, 16 * MAX_CHUNK_BYTES);
        assert!(socket.controls("cancel").is_empty());
        assert_eq!(mux.inflight_count(), 1);
    }

    #[tokio::test]
    async fn propagates_end_client_cancellation_as_a_cancel_frame() {
        let socket = RecordingSocket::new();
        let mux = mux_with(socket.clone());
        let handle = open(&mux, post("/chat/completions", None));
        flush().await;
        mux.handle_message(text_frame(json!({"t":"res","id":1,"status":200,"headers":{}}))).await;
        let response = expect_ok(handle.await.unwrap());
        drop(response);
        let cancels = socket.controls("cancel");
        assert_eq!(cancels.len(), 1);
        assert_eq!(cancels[0]["id"], 1);
        assert_eq!(mux.inflight_count(), 0);
    }

    #[tokio::test]
    async fn delivers_valid_models_reports_and_ignores_out_of_bounds_ones() {
        let socket = RecordingSocket::new();
        let hook = RecordingHook::new();
        let mux = TunnelMux::with_options(socket, Some(hook.clone() as Arc<dyn ModelsReportHook>), Duration::from_millis(FIRST_RES_TIMEOUT_MS));
        assert_eq!(
            mux.handle_message(text_frame(json!({"t":"models","models":["llama3.3:70b"]}))).await,
            MessageOutcome::Handled
        );
        mux.handle_message(text_frame(json!({"t":"models","models":["bad id"]}))).await;
        assert_eq!(hook.reports(), vec![vec!["llama3.3:70b".to_string()]]);
    }

    #[tokio::test]
    async fn a_failed_models_write_asks_for_the_retryable_close() {
        let socket = RecordingSocket::new();
        let hook = RecordingHook::failing();
        let mux = TunnelMux::with_options(socket, Some(hook as Arc<dyn ModelsReportHook>), Duration::from_millis(FIRST_RES_TIMEOUT_MS));
        assert_eq!(
            mux.handle_message(text_frame(json!({"t":"models","models":["llama3"]}))).await,
            MessageOutcome::ModelsPersistFailed
        );
    }

    #[tokio::test]
    async fn handle_message_awaits_the_models_hook() {
        let socket = RecordingSocket::new();
        let hook = RecordingHook::slow();
        let mux = TunnelMux::with_options(socket, Some(hook.clone() as Arc<dyn ModelsReportHook>), Duration::from_millis(FIRST_RES_TIMEOUT_MS));
        mux.handle_message(text_frame(json!({"t":"models","models":["llama3"]}))).await;
        assert_eq!(hook.reports(), vec![vec!["llama3".to_string()]]);
    }

    #[tokio::test]
    async fn faults_too_large_when_the_request_body_passes_the_32_mib_cap() {
        let socket = RecordingSocket::new();
        let mux = mux_with(socket.clone());
        let chunks = futures::stream::iter((0..5).map(|_| Ok(Bytes::from(vec![0u8; 8 * 1024 * 1024]))));
        let fault = expect_fault(mux
            .open_request(post("/audio/transcriptions", Some(Box::pin(chunks))))
            .await);
        assert_eq!(fault.reason, AgentFaultReason::TooLarge);
        let cancels = socket.controls("cancel");
        assert_eq!(cancels.len(), 1);
        assert_eq!(cancels[0]["id"], 1);
        assert_eq!(mux.inflight_count(), 0);
    }

    #[tokio::test]
    async fn splits_an_oversized_request_body_chunk_into_one_mib_frames() {
        let socket = RecordingSocket::new();
        let mux = mux_with(socket.clone());
        let size = 2 * MAX_CHUNK_BYTES + 5;
        let body = futures::stream::once(async move { Ok(Bytes::from(vec![1u8; size])) });
        let handle = open(&mux, post("/chat/completions", Some(Box::pin(body))));
        flush().await;
        let sizes: Vec<usize> = socket.binaries().iter().map(|b| b.2.len()).collect();
        assert_eq!(sizes, vec![MAX_CHUNK_BYTES, MAX_CHUNK_BYTES, 5]);
        assert_eq!(sizes.iter().sum::<usize>(), size);
        mux.handle_message(text_frame(json!({"t":"res_err","id":1,"reason":"aborted"}))).await;
        assert_eq!(expect_fault(handle.await.unwrap()).reason, AgentFaultReason::Protocol);
    }

    #[tokio::test]
    async fn a_dead_socket_faults_the_request_offline() {
        let socket = RecordingSocket::new();
        let mux = mux_with(socket.clone());
        socket.kill();
        let fault = expect_fault(mux.open_request(post("/chat/completions", None)).await);
        assert_eq!(fault.reason, AgentFaultReason::Offline);
        assert_eq!(mux.inflight_count(), 0);
    }

    #[tokio::test]
    async fn a_cli_cancel_frame_drops_the_request_like_res_err() {
        let socket = RecordingSocket::new();
        let mux = mux_with(socket.clone());
        let handle = open(&mux, post("/chat/completions", None));
        flush().await;
        mux.handle_message(text_frame(json!({"t":"cancel","id":1}))).await;
        assert_eq!(expect_fault(handle.await.unwrap()).reason, AgentFaultReason::Protocol);
    }

    #[tokio::test]
    async fn bodyless_statuses_carry_no_body_and_tolerate_the_trailing_res_end() {
        let socket = RecordingSocket::new();
        let mux = mux_with(socket.clone());
        let handle = open(&mux, post("/chat/completions", None));
        flush().await;
        mux.handle_message(text_frame(json!({"t":"res","id":1,"status":204,"headers":{}}))).await;
        let response = expect_ok(handle.await.unwrap());
        assert_eq!(response.status, StatusCode::NO_CONTENT);
        assert_eq!(collect(response).await.unwrap(), "");
        assert_eq!(mux.inflight_count(), 0);
        mux.handle_message(text_frame(json!({"t":"res_end","id":1}))).await;
    }

    #[tokio::test]
    async fn ids_increase_per_request() {
        let socket = RecordingSocket::new();
        let mux = mux_with(socket.clone());
        open(&mux, post("/chat/completions", None));
        open(&mux, post("/chat/completions", None));
        flush().await;
        let ids: Vec<u64> = socket.controls("req").iter().map(|f| f["id"].as_u64().unwrap()).collect();
        assert_eq!(ids, vec![1, 2]);
    }

    #[tokio::test]
    async fn fault_response_carries_the_marker_and_a_json_body() {
        let response = agent_fault_response(AgentFaultReason::Offline);
        assert_eq!(response.status, StatusCode::BAD_GATEWAY);
        assert_eq!(response.headers.get(AGENT_FAULT_HEADER).unwrap(), "offline");
        assert_eq!(
            response.json_value().await.unwrap(),
            json!({"error":{"type":"agent_fault","reason":"offline"}})
        );
    }
}
