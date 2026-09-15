//! The `fetch` seam. Bodies stream in both directions: [`UpstreamRequest`] carries bytes
//! (upstream bodies are built by adapters and are bounded), [`UpstreamResponse`] exposes a
//! byte stream so SSE is piped without buffering (docs/api.md § Streaming).

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::{Stream, StreamExt};
use http::{HeaderMap, Method, StatusCode};

pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>;

#[derive(Debug, Clone)]
pub struct UpstreamRequest {
    pub method: Method,
    pub url: String,
    pub headers: HeaderMap,
    pub body: Option<Bytes>,
    /// Wait for response headers; `None` uses the transport default.
    pub first_byte_timeout: Option<Duration>,
}

impl UpstreamRequest {
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self { method, url: url.into(), headers: HeaderMap::new(), body: None, first_byte_timeout: None }
    }
    pub fn get(url: impl Into<String>) -> Self {
        Self::new(Method::GET, url)
    }
    pub fn post(url: impl Into<String>) -> Self {
        Self::new(Method::POST, url)
    }
    pub fn header(mut self, name: &'static str, value: &str) -> Self {
        if let Ok(v) = http::HeaderValue::from_str(value) {
            self.headers.insert(name, v);
        }
        self
    }
    pub fn json(mut self, value: &serde_json::Value) -> Self {
        self.headers.insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("application/json"));
        self.body = Some(Bytes::from(serde_json::to_vec(value).expect("json value serializes")));
        self
    }
    pub fn body(mut self, body: Bytes) -> Self {
        self.body = Some(body);
        self
    }
    pub fn timeout(mut self, d: Duration) -> Self {
        self.first_byte_timeout = Some(d);
        self
    }
}

pub struct UpstreamResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: ByteStream,
}

impl UpstreamResponse {
    pub fn new(status: StatusCode, headers: HeaderMap, body: ByteStream) -> Self {
        Self { status, headers, body }
    }
    pub fn from_bytes(status: StatusCode, headers: HeaderMap, body: impl Into<Bytes>) -> Self {
        let bytes = body.into();
        Self { status, headers, body: Box::pin(futures::stream::once(async move { Ok(bytes) })) }
    }
    pub fn json(status: StatusCode, value: &serde_json::Value) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("application/json"));
        Self::from_bytes(status, headers, serde_json::to_vec(value).expect("json"))
    }
    /// SSE body from already-framed text (tests and stubs).
    pub fn sse(status: StatusCode, text: impl Into<Bytes>) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("text/event-stream"));
        Self::from_bytes(status, headers, text)
    }
    pub fn content_type(&self) -> Option<&str> {
        self.headers.get(http::header::CONTENT_TYPE).and_then(|v| v.to_str().ok())
    }
    /// Reads the whole body (bounded callers only: JSON responses, error bodies).
    pub async fn bytes(mut self) -> Result<Bytes, std::io::Error> {
        let mut out = Vec::new();
        while let Some(chunk) = self.body.next().await {
            out.extend_from_slice(&chunk?);
        }
        Ok(Bytes::from(out))
    }
    pub async fn text(self) -> Result<String, std::io::Error> {
        let b = self.bytes().await?;
        Ok(String::from_utf8_lossy(&b).into_owned())
    }
    pub async fn json_value(self) -> Result<serde_json::Value, std::io::Error> {
        let b = self.bytes().await?;
        serde_json::from_slice(&b).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// No response headers within the first-byte timeout (an "edge timeout" for benching).
    #[error("upstream timed out waiting for response headers")]
    Timeout,
    #[error("upstream connection failed: {0}")]
    Connect(String),
    #[error("upstream request failed: {0}")]
    Other(String),
}

#[async_trait]
pub trait UpstreamTransport: Send + Sync {
    async fn send(&self, req: UpstreamRequest) -> Result<UpstreamResponse, TransportError>;
}

/// Production transport: rustls, HTTP/2 where offered, no automatic redirects (adapters
/// must see upstream 3xx as errors, as `fetch` with `redirect: "manual"` would).
pub struct ReqwestTransport {
    client: reqwest::Client,
    default_first_byte_timeout: Duration,
}

impl ReqwestTransport {
    pub fn new(default_first_byte_timeout: Duration) -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .use_rustls_tls()
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .expect("reqwest client builds");
        Self { client, default_first_byte_timeout }
    }
}

#[async_trait]
impl UpstreamTransport for ReqwestTransport {
    async fn send(&self, req: UpstreamRequest) -> Result<UpstreamResponse, TransportError> {
        let mut builder = self.client.request(req.method, &req.url).headers(req.headers);
        if let Some(body) = req.body {
            builder = builder.body(body);
        }
        let timeout = req.first_byte_timeout.unwrap_or(self.default_first_byte_timeout);
        let response = match tokio::time::timeout(timeout, builder.send()).await {
            Err(_) => return Err(TransportError::Timeout),
            Ok(Err(e)) if e.is_connect() => return Err(TransportError::Connect(e.to_string())),
            Ok(Err(e)) if e.is_timeout() => return Err(TransportError::Timeout),
            Ok(Err(e)) => return Err(TransportError::Other(e.to_string())),
            Ok(Ok(r)) => r,
        };
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.bytes_stream().map(|r| r.map_err(|e| std::io::Error::other(e.to_string())));
        Ok(UpstreamResponse { status, headers, body: Box::pin(body) })
    }
}

type Handler = Box<dyn Fn(&UpstreamRequest) -> Result<UpstreamResponse, TransportError> + Send + Sync>;

/// Test transport: handlers answer in FIFO order (one per expected upstream call) and every
/// request is recorded for assertions. A missing handler is a test failure, never a real call.
#[derive(Default)]
pub struct MockTransport {
    handlers: Mutex<VecDeque<Handler>>,
    requests: Mutex<Vec<RecordedRequest>>,
}

#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: Method,
    pub url: String,
    pub headers: HeaderMap,
    pub body: Option<Bytes>,
}

impl RecordedRequest {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(self.body.as_deref().unwrap_or(b"null")).expect("recorded body is JSON")
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

impl MockTransport {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub fn expect(&self, handler: impl Fn(&UpstreamRequest) -> Result<UpstreamResponse, TransportError> + Send + Sync + 'static) {
        self.handlers.lock().unwrap().push_back(Box::new(handler));
    }
    pub fn respond_json(&self, status: StatusCode, value: serde_json::Value) {
        self.expect(move |_| Ok(UpstreamResponse::json(status, &value)));
    }
    pub fn respond_sse(&self, status: StatusCode, text: &'static str) {
        self.expect(move |_| Ok(UpstreamResponse::sse(status, text)));
    }
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }
    pub fn pending(&self) -> usize {
        self.handlers.lock().unwrap().len()
    }
}

#[async_trait]
impl UpstreamTransport for MockTransport {
    async fn send(&self, req: UpstreamRequest) -> Result<UpstreamResponse, TransportError> {
        self.requests.lock().unwrap().push(RecordedRequest {
            method: req.method.clone(),
            url: req.url.clone(),
            headers: req.headers.clone(),
            body: req.body.clone(),
        });
        let handler = self.handlers.lock().unwrap().pop_front();
        match handler {
            Some(h) => h(&req),
            None => panic!("MockTransport: unexpected upstream request {} {}", req.method, req.url),
        }
    }
}
