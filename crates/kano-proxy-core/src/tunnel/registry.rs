//! Port of apps/api/src/do/agent_tunnel.ts — the AgentTunnel Durable Object as
//! an in-process registry (docs/cli.md § AgentTunnel Durable Object,
//! docs/rust-server.md § Storage: "a tunnel registry keyed by `cli_providers.id`").
//!
//! One live socket per `cli_providers.id`: a second connect replaces the first
//! with close `4001 replaced`, the access token's `exp` schedules a
//! `4003 token_expired` close (the DO's alarm), and a failed models-report write
//! closes `4008` retryably so the reconnect re-reports. Persistence and the
//! reconnect bench clear are callbacks — this module never writes SQL.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use http::{HeaderMap, Method};

use super::mux::{
    agent_fault_response, MuxSocket, ModelsReportHook, OpenRequest, OutFrame, SocketClosed, TunnelFault, TunnelMux,
};
use super::protocol::{
    encode_control_frame, is_allowed_path, AgentFaultReason, CliProviderFormat, ControlFrame, AGENT_PROTO,
    CLOSE_REPLACED, CLOSE_RETRY, CLOSE_TOKEN_EXPIRED, FIRST_RES_TIMEOUT_MS,
};
use crate::upstream::transport::{ByteStream, TransportError, UpstreamRequest, UpstreamTransport};
use crate::upstream::UpstreamResponse;

/// Request headers forwarded down the tunnel — same reduction discipline as the codex relay.
pub const FORWARDED_REQUEST_HEADERS: [&str; 4] = ["content-type", "accept", "anthropic-version", "anthropic-beta"];

/// What the route verified before the upgrade: the DO trusted the internal
/// `x-kano-*` headers, the registry trusts these fields.
#[derive(Debug, Clone)]
pub struct ConnectParams {
    pub user_id: String,
    pub provider_id: String,
    pub slug: String,
    pub format: CliProviderFormat,
    /// Access-token `exp`, epoch milliseconds — the revocation close is scheduled here.
    pub token_exp_ms: i64,
}

/// Persists an agent-reported catalog to `cli_providers.models_json` +
/// `models_updated_at` (docs/cli.md § Model catalog). The db layer implements it.
#[async_trait]
pub trait ModelsSink: Send + Sync + 'static {
    async fn persist_models(&self, provider_id: &str, models: Vec<String>) -> anyhow::Result<()>;
}

/// Reconnect clears the bench on the provider's internal account row
/// (docs/cli.md § Failover semantics). The db layer implements it; failures are
/// best-effort, exactly as the DO's try/catch was.
#[async_trait]
pub trait ConnectObserver: Send + Sync + 'static {
    async fn on_connect(&self, user_id: &str, slug: &str, provider_id: &str);
}

/// `/status`-style introspection (the DO's `/status` fetch and the provider
/// list's `connected` read-through).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelStatus {
    pub connected: bool,
    pub proto: u32,
    pub slug: Option<String>,
    pub format: Option<CliProviderFormat>,
    pub inflight: usize,
    pub connected_at_ms: Option<i64>,
}

impl TunnelStatus {
    fn offline() -> Self {
        Self { connected: false, proto: AGENT_PROTO, slug: None, format: None, inflight: 0, connected_at_ms: None }
    }
}

/// One frame (or a close) for the socket writer task in `ws.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocketCommand {
    Frame(OutFrame),
    Close { code: u16, reason: &'static str },
}

pub type CommandSender = tokio::sync::mpsc::UnboundedSender<SocketCommand>;
pub type CommandReceiver = tokio::sync::mpsc::UnboundedReceiver<SocketCommand>;

struct ChannelSocket {
    tx: CommandSender,
}

impl MuxSocket for ChannelSocket {
    fn send(&self, frame: OutFrame) -> Result<(), SocketClosed> {
        self.tx.send(SocketCommand::Frame(frame)).map_err(|_| SocketClosed)
    }
}

/// One live CLI socket. Held by the registry and by its `ws.rs` task; the
/// writer half is the command channel.
pub struct Connection {
    params: ConnectParams,
    mux: TunnelMux,
    tx: CommandSender,
    connected_at_ms: i64,
    expiry_timer: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Connection {
    pub fn params(&self) -> &ConnectParams {
        &self.params
    }
    pub fn provider_id(&self) -> &str {
        &self.params.provider_id
    }
    pub fn mux(&self) -> &TunnelMux {
        &self.mux
    }
    pub fn send_text(&self, text: String) -> Result<(), SocketClosed> {
        self.tx.send(SocketCommand::Frame(OutFrame::Text(text))).map_err(|_| SocketClosed)
    }
    pub(crate) fn send_close(&self, code: u16, reason: &'static str) {
        let _ = self.tx.send(SocketCommand::Close { code, reason });
    }
    fn cancel_expiry(&self) {
        if let Some(timer) = self.expiry_timer.lock().expect("tunnel lock").take() {
            timer.abort();
        }
    }
}

struct RegistryInner {
    connections: Mutex<HashMap<String, Arc<Connection>>>,
    models: Option<Arc<dyn ModelsSink>>,
    observer: Option<Arc<dyn ConnectObserver>>,
    first_res_timeout: Duration,
}

/// The Durable Object namespace's replacement: one live connection per
/// `cli_providers.id`. Clone freely — every clone shares the same state.
#[derive(Clone)]
pub struct TunnelRegistry {
    inner: Arc<RegistryInner>,
}

impl Default for TunnelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl TunnelRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                connections: Mutex::new(HashMap::new()),
                models: None,
                observer: None,
                first_res_timeout: Duration::from_millis(FIRST_RES_TIMEOUT_MS),
            }),
        }
    }

    /// The db layer plugs its catalog write in here (docs/cli.md § Model catalog).
    pub fn with_models_sink(mut self, sink: Arc<dyn ModelsSink>) -> Self {
        Arc::get_mut(&mut self.inner).expect("registry not shared yet").models = Some(sink);
        self
    }

    /// The db layer plugs the reconnect bench clear in here.
    pub fn with_connect_observer(mut self, observer: Arc<dyn ConnectObserver>) -> Self {
        Arc::get_mut(&mut self.inner).expect("registry not shared yet").observer = Some(observer);
        self
    }

    /// Shorter first-`res` deadline for tests; production keeps the documented 120 s.
    pub fn with_first_res_timeout(mut self, timeout: Duration) -> Self {
        Arc::get_mut(&mut self.inner).expect("registry not shared yet").first_res_timeout = timeout;
        self
    }

    /// Accepts a socket for `params.provider_id`, replacing any live one with
    /// close `4001 replaced`, and returns the connection plus the receiver the
    /// socket writer drains. The `hello` frame is already queued on it.
    pub fn connect(&self, params: ConnectParams) -> (Arc<Connection>, CommandReceiver) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let hook: Option<Arc<dyn ModelsReportHook>> = self.inner.models.clone().map(|sink| {
            Arc::new(ProviderModelsHook { sink, provider_id: params.provider_id.clone() }) as Arc<dyn ModelsReportHook>
        });
        let mux = TunnelMux::with_options(
            Arc::new(ChannelSocket { tx: tx.clone() }),
            hook,
            self.inner.first_res_timeout,
        );
        let connection = Arc::new(Connection {
            connected_at_ms: crate::app::now_ms(),
            params: params.clone(),
            mux,
            tx,
            expiry_timer: Mutex::new(None),
        });

        // One live socket: a second successful connect replaces the first —
        // laptop-resume reconnects are self-healing instead of "address in use".
        let replaced = {
            let mut guard = self.inner.connections.lock().expect("tunnel lock");
            guard.insert(params.provider_id.clone(), connection.clone())
        };
        if let Some(old) = replaced {
            old.cancel_expiry();
            old.mux.abort_all(AgentFaultReason::Replaced);
            old.send_close(CLOSE_REPLACED, "replaced");
        }

        // The only scheduled work: revocation reaches a live socket at
        // access-token expiry (the DO's alarm).
        let delay = Duration::from_millis(params.token_exp_ms.saturating_sub(crate::app::now_ms()).max(0) as u64);
        let registry = self.clone();
        let expiring = connection.clone();
        let timer = tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            // The CLI treats 4003 as "refresh, then reconnect".
            registry.close_connection(&expiring, CLOSE_TOKEN_EXPIRED, "token_expired");
        });
        *connection.expiry_timer.lock().expect("tunnel lock") = Some(timer);

        let _ = connection.send_text(encode_control_frame(&ControlFrame::Hello {
            proto: AGENT_PROTO,
            slug: params.slug.clone(),
        }));

        // Reconnect clears the bench: opening the laptop restores service on the
        // next request, no operator action (docs/cli.md § Failover semantics).
        if let Some(observer) = self.inner.observer.clone() {
            tokio::spawn(async move {
                observer.on_connect(&params.user_id, &params.slug, &params.provider_id).await;
            });
        }

        (connection, rx)
    }

    /// The socket ended (close or error): every open request faults `offline`.
    pub fn disconnect(&self, connection: &Arc<Connection>) {
        self.take_if_current(connection);
        connection.cancel_expiry();
        connection.mux.abort_all(AgentFaultReason::Offline);
    }

    /// The models-report write failed: close retryably so the reconnect
    /// re-reports from a clean slate (docs/cli.md § Model catalog).
    pub fn close_retryable(&self, connection: &Arc<Connection>) {
        self.close_connection(connection, CLOSE_RETRY, "models_persist_failed");
    }

    /// Closes the live socket for a provider, if any (provider deletion: the
    /// DO's `/close`).
    pub fn close(&self, provider_id: &str) -> bool {
        let connection = { self.inner.connections.lock().expect("tunnel lock").get(provider_id).cloned() };
        match connection {
            Some(connection) => {
                self.close_connection(&connection, 1000, "closed");
                true
            }
            None => false,
        }
    }

    fn close_connection(&self, connection: &Arc<Connection>, code: u16, reason: &'static str) {
        self.take_if_current(connection);
        connection.cancel_expiry();
        // Token expiry is a revocation, not a takeover: open requests see `offline`.
        connection.mux.abort_all(if code == CLOSE_TOKEN_EXPIRED {
            AgentFaultReason::Offline
        } else {
            AgentFaultReason::Replaced
        });
        connection.send_close(code, reason);
    }

    /// Removes the connection only when it is still the live one, so a replaced
    /// socket's late close cannot evict its successor.
    fn take_if_current(&self, connection: &Arc<Connection>) {
        let mut guard = self.inner.connections.lock().expect("tunnel lock");
        let current = guard.get(connection.provider_id());
        if current.is_some_and(|live| Arc::ptr_eq(live, connection)) {
            guard.remove(connection.provider_id());
        }
    }

    pub fn is_connected(&self, provider_id: &str) -> bool {
        self.inner.connections.lock().expect("tunnel lock").contains_key(provider_id)
    }

    pub fn status(&self, provider_id: &str) -> TunnelStatus {
        let connection = { self.inner.connections.lock().expect("tunnel lock").get(provider_id).cloned() };
        match connection {
            None => TunnelStatus::offline(),
            Some(connection) => TunnelStatus {
                connected: true,
                proto: AGENT_PROTO,
                slug: Some(connection.params.slug.clone()),
                format: Some(connection.params.format),
                inflight: connection.mux.inflight_count(),
                connected_at_ms: Some(connection.connected_at_ms),
            },
        }
    }

    /// Every live connection, newest state first read — admin introspection.
    pub fn connected_provider_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.inner.connections.lock().expect("tunnel lock").keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Proxies one client request down the provider's tunnel. `Ok` is the local
    /// server's answer marked `x-agent-upstream: 1`; `Err` is a tunnel fault
    /// (502 `x-agent-fault: …`) — the tri-state guard of docs/cli.md.
    pub async fn proxy(
        &self,
        provider_id: &str,
        method: &Method,
        path: &str,
        headers: &HeaderMap,
        body: Option<ByteStream>,
    ) -> Result<UpstreamResponse, TunnelFault> {
        let connection = { self.inner.connections.lock().expect("tunnel lock").get(provider_id).cloned() };
        let Some(connection) = connection else {
            return Err(TunnelFault::new(AgentFaultReason::Offline));
        };
        if !is_allowed_path(connection.params.format, path) {
            return Err(TunnelFault::new(AgentFaultReason::Protocol));
        }
        let forwarded = FORWARDED_REQUEST_HEADERS
            .iter()
            .filter_map(|name| {
                headers.get(*name).and_then(|v| v.to_str().ok()).map(|v| ((*name).to_string(), v.to_string()))
            })
            .collect();
        connection
            .mux
            .open_request(OpenRequest { method: method.as_str().to_string(), path: path.to_string(), headers: forwarded, body })
            .await
    }

    /// The transport the CLI provider adapter injects in place of the network
    /// (apps/api/src/providers/cli.ts § `createAgentFetch`): the custom adapters
    /// build `${base_url}<suffix>` over an empty base, so the request URL is
    /// already the bare allowlisted suffix. Faults come back as the 502 marker
    /// response, never as a transport error, exactly as the injected `fetch` did.
    pub fn transport(&self, provider_id: &str) -> Arc<dyn UpstreamTransport> {
        Arc::new(AgentTransport { registry: self.clone(), provider_id: provider_id.to_string() })
    }
}

struct ProviderModelsHook {
    sink: Arc<dyn ModelsSink>,
    provider_id: String,
}

#[async_trait]
impl ModelsReportHook for ProviderModelsHook {
    async fn on_models_report(&self, models: Vec<String>) -> anyhow::Result<()> {
        self.sink.persist_models(&self.provider_id, models).await
    }
}

struct AgentTransport {
    registry: TunnelRegistry,
    provider_id: String,
}

/// The path that goes on the wire: the bare suffix the CLI joins onto its one
/// configured target base, query string and any absolute prefix removed.
fn wire_path(url: &str) -> String {
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    match without_query.find("://") {
        None => without_query.to_string(),
        Some(scheme_end) => match without_query[scheme_end + 3..].find('/') {
            Some(slash) => without_query[scheme_end + 3 + slash..].to_string(),
            None => "/".to_string(),
        },
    }
}

#[async_trait]
impl UpstreamTransport for AgentTransport {
    async fn send(&self, req: UpstreamRequest) -> Result<UpstreamResponse, TransportError> {
        // Auth headers are stripped: the placeholder credential means nothing,
        // and the CLI injects the local server's own key on its side.
        let mut headers = req.headers.clone();
        headers.remove(http::header::AUTHORIZATION);
        headers.remove("x-api-key");
        let body: Option<ByteStream> = req
            .body
            .filter(|b| !b.is_empty())
            .map(|bytes| Box::pin(futures::stream::once(async move { Ok(bytes) })) as ByteStream);
        let result = self
            .registry
            .proxy(&self.provider_id, &req.method, &wire_path(&req.url), &headers, body)
            .await;
        Ok(match result {
            Ok(response) => response,
            Err(fault) => agent_fault_response(fault.reason),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use crate::tunnel::mux::{MuxMessage, AGENT_FAULT_HEADER, AGENT_UPSTREAM_HEADER};
    use crate::tunnel::protocol::{encode_binary_frame, BODY_KIND_RESPONSE};
    use serde_json::{json, Value};

    struct Sink {
        reports: Mutex<Vec<(String, Vec<String>)>>,
        fail: bool,
    }

    #[async_trait]
    impl ModelsSink for Sink {
        async fn persist_models(&self, provider_id: &str, models: Vec<String>) -> anyhow::Result<()> {
            if self.fail {
                anyhow::bail!("d1 write failed");
            }
            self.reports.lock().unwrap().push((provider_id.to_string(), models));
            Ok(())
        }
    }

    struct Observer {
        seen: Mutex<Vec<(String, String, String)>>,
    }

    #[async_trait]
    impl ConnectObserver for Observer {
        async fn on_connect(&self, user_id: &str, slug: &str, provider_id: &str) {
            self.seen.lock().unwrap().push((user_id.to_string(), slug.to_string(), provider_id.to_string()));
        }
    }

    fn params(provider_id: &str, exp_in_ms: i64) -> ConnectParams {
        ConnectParams {
            user_id: "user_1".into(),
            provider_id: provider_id.into(),
            slug: "my-mac".into(),
            format: CliProviderFormat::OpenAI,
            token_exp_ms: crate::app::now_ms() + exp_in_ms,
        }
    }

    fn drain(rx: &mut CommandReceiver) -> Vec<SocketCommand> {
        let mut out = Vec::new();
        while let Ok(cmd) = rx.try_recv() {
            out.push(cmd);
        }
        out
    }

    fn controls(commands: &[SocketCommand], t: &str) -> Vec<Value> {
        commands
            .iter()
            .filter_map(|c| match c {
                SocketCommand::Frame(OutFrame::Text(text)) => serde_json::from_str::<Value>(text).ok(),
                _ => None,
            })
            .filter(|v| v.get("t").and_then(Value::as_str) == Some(t))
            .collect()
    }

    fn closes(commands: &[SocketCommand]) -> Vec<(u16, &'static str)> {
        commands
            .iter()
            .filter_map(|c| match c {
                SocketCommand::Close { code, reason } => Some((*code, *reason)),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn a_connect_sends_hello_and_reports_status() {
        let registry = TunnelRegistry::new();
        assert_eq!(registry.status("p1"), TunnelStatus::offline());
        let (conn, mut rx) = registry.connect(params("p1", 60_000));
        let hello = controls(&drain(&mut rx), "hello");
        assert_eq!(hello, vec![json!({"t":"hello","proto":1,"slug":"my-mac"})]);
        let status = registry.status("p1");
        assert!(status.connected);
        assert_eq!(status.proto, AGENT_PROTO);
        assert_eq!(status.slug.as_deref(), Some("my-mac"));
        assert_eq!(registry.connected_provider_ids(), vec!["p1".to_string()]);
        registry.disconnect(&conn);
        assert!(!registry.is_connected("p1"));
        assert_eq!(registry.status("p1"), TunnelStatus::offline());
    }

    #[tokio::test]
    async fn a_second_connect_replaces_the_first_with_4001() {
        let registry = TunnelRegistry::new();
        let (first, mut first_rx) = registry.connect(params("p1", 60_000));
        let pending = {
            let conn = first.clone();
            tokio::spawn(async move {
                conn.mux()
                    .open_request(OpenRequest {
                        method: "POST".into(),
                        path: "/chat/completions".into(),
                        headers: Default::default(),
                        body: None,
                    })
                    .await
                    .err()
                    .map(|f| f.reason)
            })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;

        let (_second, mut second_rx) = registry.connect(params("p1", 60_000));
        assert_eq!(closes(&drain(&mut first_rx)), vec![(CLOSE_REPLACED, "replaced")]);
        assert_eq!(pending.await.unwrap(), Some(AgentFaultReason::Replaced));
        assert_eq!(controls(&drain(&mut second_rx), "hello").len(), 1);
        // The replaced socket's late close must not evict its successor.
        registry.disconnect(&first);
        assert!(registry.is_connected("p1"));
    }

    #[tokio::test]
    async fn the_token_expiry_closes_with_4003() {
        let registry = TunnelRegistry::new();
        let (_conn, mut rx) = registry.connect(params("p1", 30));
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert_eq!(closes(&drain(&mut rx)), vec![(CLOSE_TOKEN_EXPIRED, "token_expired")]);
        assert!(!registry.is_connected("p1"));
    }

    #[tokio::test]
    async fn a_failed_models_write_closes_4008_retryably() {
        let sink = Arc::new(Sink { reports: Mutex::new(Vec::new()), fail: true });
        let registry = TunnelRegistry::new().with_models_sink(sink);
        let (conn, mut rx) = registry.connect(params("p1", 60_000));
        let outcome = conn.mux().handle_message(MuxMessage::Text(json!({"t":"models","models":["llama3"]}).to_string())).await;
        assert_eq!(outcome, crate::tunnel::mux::MessageOutcome::ModelsPersistFailed);
        registry.close_retryable(&conn);
        assert_eq!(closes(&drain(&mut rx)), vec![(CLOSE_RETRY, "models_persist_failed")]);
        assert!(!registry.is_connected("p1"));
    }

    #[tokio::test]
    async fn a_models_report_reaches_the_sink_with_its_provider_id() {
        let sink = Arc::new(Sink { reports: Mutex::new(Vec::new()), fail: false });
        let registry = TunnelRegistry::new().with_models_sink(sink.clone());
        let (conn, _rx) = registry.connect(params("p1", 60_000));
        conn.mux().handle_message(MuxMessage::Text(json!({"t":"models","models":["llama3"]}).to_string())).await;
        assert_eq!(sink.reports.lock().unwrap().clone(), vec![("p1".to_string(), vec!["llama3".to_string()])]);
    }

    #[tokio::test]
    async fn a_connect_notifies_the_observer_so_the_bench_clears() {
        let observer = Arc::new(Observer { seen: Mutex::new(Vec::new()) });
        let registry = TunnelRegistry::new().with_connect_observer(observer.clone());
        let (_conn, _rx) = registry.connect(params("p1", 60_000));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            observer.seen.lock().unwrap().clone(),
            vec![("user_1".to_string(), "my-mac".to_string(), "p1".to_string())]
        );
    }

    #[tokio::test]
    async fn proxy_faults_offline_without_a_connection() {
        let registry = TunnelRegistry::new();
        let fault = registry
            .proxy("p1", &Method::POST, "/chat/completions", &HeaderMap::new(), None)
            .await
            .err()
            .map(|f| f.reason);
        assert_eq!(fault, Some(AgentFaultReason::Offline));
    }

    #[tokio::test]
    async fn proxy_refuses_a_path_outside_the_formats_allowlist() {
        let registry = TunnelRegistry::new();
        let (_conn, _rx) = registry.connect(params("p1", 60_000));
        let fault = registry
            .proxy("p1", &Method::POST, "/v1/messages", &HeaderMap::new(), None)
            .await
            .err()
            .map(|f| f.reason);
        assert_eq!(fault, Some(AgentFaultReason::Protocol));
    }

    #[tokio::test]
    async fn proxy_forwards_only_the_reduced_headers_and_streams_the_answer() {
        let registry = TunnelRegistry::new();
        let (conn, mut rx) = registry.connect(params("p1", 60_000));
        let mut headers = HeaderMap::new();
        headers.insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("application/json"));
        headers.insert(http::header::AUTHORIZATION, http::HeaderValue::from_static("Bearer secret"));
        headers.insert("anthropic-version", http::HeaderValue::from_static("2023-06-01"));
        headers.insert("x-forwarded-for", http::HeaderValue::from_static("1.2.3.4"));

        let proxying = {
            let registry = registry.clone();
            tokio::spawn(async move {
                registry
                    .proxy("p1", &Method::POST, "/chat/completions", &headers, None)
                    .await
                    .ok()
            })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        let sent = drain(&mut rx);
        let req = &controls(&sent, "req")[0];
        assert_eq!(req["method"], "POST");
        assert_eq!(req["path"], "/chat/completions");
        assert_eq!(req["headers"], json!({"anthropic-version":"2023-06-01","content-type":"application/json"}));
        assert_eq!(controls(&sent, "req_end").len(), 1);

        conn.mux()
            .handle_message(MuxMessage::Text(
                json!({"t":"res","id":1,"status":200,"headers":{"content-type":"application/json"}}).to_string(),
            ))
            .await;
        conn.mux()
            .handle_message(MuxMessage::Binary(Bytes::from(encode_binary_frame(1, BODY_KIND_RESPONSE, b"{\"ok\":true}"))))
            .await;
        conn.mux().handle_message(MuxMessage::Text(json!({"t":"res_end","id":1}).to_string())).await;

        let response = proxying.await.unwrap().expect("a response");
        assert_eq!(response.status, http::StatusCode::OK);
        assert_eq!(response.headers.get(AGENT_UPSTREAM_HEADER).unwrap(), "1");
        assert_eq!(response.json_value().await.unwrap(), json!({"ok":true}));
    }

    #[tokio::test]
    async fn the_injected_transport_answers_faults_as_the_502_marker_response() {
        let registry = TunnelRegistry::new();
        let transport = registry.transport("p1");
        let response = transport
            .send(UpstreamRequest::post("/chat/completions").json(&json!({"model":"llama3"})))
            .await
            .expect("the agent transport never errors");
        assert_eq!(response.status, http::StatusCode::BAD_GATEWAY);
        assert_eq!(response.headers.get(AGENT_FAULT_HEADER).unwrap(), "offline");
        assert_eq!(response.json_value().await.unwrap(), json!({"error":{"type":"agent_fault","reason":"offline"}}));
    }

    #[tokio::test]
    async fn the_injected_transport_strips_auth_and_sends_the_body_down_the_tunnel() {
        let registry = TunnelRegistry::new();
        let (conn, mut rx) = registry.connect(params("p1", 60_000));
        let transport = registry.transport("p1");
        let sending = tokio::spawn(async move {
            transport
                .send(
                    UpstreamRequest::post("/chat/completions")
                        .json(&json!({"model":"llama3"}))
                        .header("authorization", "Bearer placeholder")
                        .header("x-api-key", "placeholder"),
                )
                .await
                .map(|r| r.status)
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let sent = drain(&mut rx);
        let req = &controls(&sent, "req")[0];
        assert_eq!(req["headers"], json!({"content-type":"application/json"}));
        let bodies: Vec<Vec<u8>> = sent
            .iter()
            .filter_map(|c| match c {
                SocketCommand::Frame(OutFrame::Binary(data)) => Some(data[5..].to_vec()),
                _ => None,
            })
            .collect();
        assert_eq!(bodies, vec![br#"{"model":"llama3"}"#.to_vec()]);

        conn.mux()
            .handle_message(MuxMessage::Text(json!({"t":"res_err","id":1,"reason":"connect_refused"}).to_string()))
            .await;
        assert_eq!(sending.await.unwrap().unwrap(), http::StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn disconnect_faults_open_requests_offline() {
        let registry = TunnelRegistry::new();
        let (conn, _rx) = registry.connect(params("p1", 60_000));
        let proxying = {
            let registry = registry.clone();
            tokio::spawn(async move {
                registry
                    .proxy("p1", &Method::POST, "/chat/completions", &HeaderMap::new(), None)
                    .await
                    .err()
                    .map(|f| f.reason)
            })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        registry.disconnect(&conn);
        assert_eq!(proxying.await.unwrap(), Some(AgentFaultReason::Offline));
    }

    #[tokio::test]
    async fn close_ends_the_socket_for_a_deleted_provider() {
        let registry = TunnelRegistry::new();
        let (_conn, mut rx) = registry.connect(params("p1", 60_000));
        assert!(registry.close("p1"));
        assert_eq!(closes(&drain(&mut rx)), vec![(1000, "closed")]);
        assert!(!registry.close("p1"));
    }

    #[tokio::test]
    async fn a_first_res_timeout_faults_the_request() {
        let registry = TunnelRegistry::new().with_first_res_timeout(Duration::from_millis(50));
        let (_conn, _rx) = registry.connect(params("p1", 60_000));
        let fault = registry
            .proxy("p1", &Method::POST, "/chat/completions", &HeaderMap::new(), None)
            .await
            .err()
            .map(|f| f.reason);
        assert_eq!(fault, Some(AgentFaultReason::Timeout));
    }

    #[test]
    fn wire_paths_are_bare_suffixes() {
        assert_eq!(wire_path("/chat/completions"), "/chat/completions");
        assert_eq!(wire_path("/models?limit=1"), "/models");
        assert_eq!(wire_path("https://agent-tunnel/v1/messages"), "/v1/messages");
        assert_eq!(wire_path("https://agent-tunnel"), "/");
    }

    /// A body stream that the registry proxies whole, in 1 MiB frames.
    #[tokio::test]
    async fn proxy_streams_a_request_body_in_bounded_frames() {
        let registry = TunnelRegistry::new();
        let (conn, mut rx) = registry.connect(params("p1", 60_000));
        let body: ByteStream = Box::pin(futures::stream::iter(vec![
            Ok(Bytes::from(vec![1u8; 1_500_000])),
            Ok(Bytes::from_static(b"tail")),
        ]));
        let proxying = {
            let registry = registry.clone();
            tokio::spawn(async move {
                registry
                    .proxy("p1", &Method::POST, "/audio/transcriptions", &HeaderMap::new(), Some(body))
                    .await
                    .err()
                    .map(|f| f.reason)
            })
        };
        tokio::time::sleep(Duration::from_millis(30)).await;
        let sent = drain(&mut rx);
        let sizes: Vec<usize> = sent
            .iter()
            .filter_map(|c| match c {
                SocketCommand::Frame(OutFrame::Binary(data)) => Some(data.len() - 5),
                _ => None,
            })
            .collect();
        assert_eq!(sizes, vec![1024 * 1024, 1_500_000 - 1024 * 1024, 4]);
        conn.mux().handle_message(MuxMessage::Text(json!({"t":"res_err","id":1,"reason":"timeout"}).to_string())).await;
        assert_eq!(proxying.await.unwrap(), Some(AgentFaultReason::Timeout));
    }
}
