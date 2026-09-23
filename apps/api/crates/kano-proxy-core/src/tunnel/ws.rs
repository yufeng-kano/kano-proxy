//! The WebSocket half of the tunnel, as an axum upgrade handler.
//!
//! The route (`GET /agent/v1/connect/:providerId`) authenticates first: it
//! verifies the access token, checks the device still exists (revoking deletes it) and checks the
//! provider row belongs to the token's user, then hands the verified facts here
//! as [`ConnectParams`]. This layer never sees or validates tokens, exactly as
//! the Durable Object never did (docs/cli.md § Wire protocol).

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use futures::{SinkExt, StreamExt};

use super::mux::{MessageOutcome, MuxMessage};
use super::registry::{CommandReceiver, Connection, ConnectParams, SocketCommand, TunnelRegistry};

/// Accepts the upgrade and runs the socket until either end closes.
pub fn connect(registry: TunnelRegistry, params: ConnectParams, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| run(registry, params, socket))
}

async fn run(registry: TunnelRegistry, params: ConnectParams, socket: WebSocket) {
    let provider_id = params.provider_id.clone();
    // Registering queues `hello` before the writer starts, so it is the first
    // frame on the wire; any socket this one replaces is closed 4001 here.
    let (connection, commands) = registry.connect(params);
    let (sink, stream) = socket.split();
    let writer = tokio::spawn(write_loop(sink, commands));

    read_loop(&registry, &connection, stream).await;

    registry.disconnect(&connection);
    // Ends the writer when the read side stopped first (client close or error).
    connection.send_close(1000, "closed");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), writer).await;
    tracing::debug!(provider_id, "agent tunnel socket closed");
}

async fn write_loop(mut sink: futures::stream::SplitSink<WebSocket, Message>, mut commands: CommandReceiver) {
    while let Some(command) = commands.recv().await {
        let message = match command {
            SocketCommand::Frame(frame) => match frame {
                super::mux::OutFrame::Text(text) => Message::Text(text.into()),
                super::mux::OutFrame::Binary(data) => Message::Binary(data.into()),
            },
            SocketCommand::Close { code, reason } => {
                let _ = sink
                    .send(Message::Close(Some(CloseFrame { code, reason: reason.into() })))
                    .await;
                let _ = sink.flush().await;
                return;
            }
        };
        if sink.send(message).await.is_err() {
            return;
        }
    }
}

async fn read_loop(
    registry: &TunnelRegistry,
    connection: &std::sync::Arc<Connection>,
    mut stream: futures::stream::SplitStream<WebSocket>,
) {
    while let Some(message) = stream.next().await {
        let Ok(message) = message else { return };
        match message {
            Message::Text(text) => {
                // The heartbeat pair the DO configured as an auto-response: an
                // idle connected agent costs nothing beyond this reply.
                if text.as_str() == "ping" {
                    if connection.send_text("pong".to_string()).is_err() {
                        return;
                    }
                    continue;
                }
                if connection.mux().handle_message(MuxMessage::Text(text.as_str().to_string())).await
                    == MessageOutcome::ModelsPersistFailed
                {
                    // The CLI marked this list as sent the moment it enqueued the
                    // frame and will not repeat it until the list changes —
                    // closing retryably makes the reconnect re-report from a
                    // clean slate (docs/cli.md § Model catalog).
                    registry.close_retryable(connection);
                    return;
                }
            }
            Message::Binary(data) => {
                connection.mux().handle_message(MuxMessage::Binary(data)).await;
            }
            Message::Close(_) => return,
            // Protocol-level ping/pong is answered by the server implementation.
            Message::Ping(_) | Message::Pong(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use axum::extract::State;
    use axum::routing::get;
    use axum::Router;
    use bytes::Bytes;
    use futures::{SinkExt, StreamExt};
    use http::{HeaderMap, Method};
    use serde_json::{json, Value};
    use tokio_tungstenite::tungstenite::Message as ClientMessage;

    use super::*;
    use crate::tunnel::protocol::{
        encode_binary_frame, AgentFaultReason, CliProviderFormat, BODY_KIND_REQUEST, BODY_KIND_RESPONSE,
        CLOSE_REPLACED, CLOSE_RETRY, CLOSE_TOKEN_EXPIRED, MAX_CHUNK_BYTES, MAX_INFLIGHT,
    };
    use crate::tunnel::registry::{ConnectObserver, ModelsSink, TunnelRegistry};

    #[derive(Clone)]
    struct TestState {
        registry: TunnelRegistry,
        provider_id: String,
        token_ttl_ms: i64,
    }

    async fn handler(State(state): State<TestState>, ws: WebSocketUpgrade) -> Response {
        let params = ConnectParams {
            user_id: "user_1".into(),
            provider_id: state.provider_id.clone(),
            slug: "my-mac".into(),
            format: CliProviderFormat::OpenAI,
            token_exp_ms: crate::app::now_ms() + state.token_ttl_ms,
        };
        connect(state.registry.clone(), params, ws)
    }

    /// An axum server on an ephemeral loopback port — no real network beyond it.
    async fn serve(state: TestState) -> String {
        let app = Router::new().route("/connect", get(handler)).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("ws://{addr}/connect")
    }

    type Client = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

    async fn dial(url: &str) -> Client {
        tokio_tungstenite::connect_async(url).await.expect("the tunnel accepts the upgrade").0
    }

    async fn next_message(client: &mut Client) -> ClientMessage {
        tokio::time::timeout(Duration::from_secs(5), client.next())
            .await
            .expect("a frame within 5s")
            .expect("the socket stays open")
            .expect("a readable frame")
    }

    async fn next_control(client: &mut Client) -> Value {
        loop {
            match next_message(client).await {
                ClientMessage::Text(text) => return serde_json::from_str(&text).expect("a JSON control frame"),
                ClientMessage::Binary(_) => continue,
                other => panic!("expected a control frame, got {other:?}"),
            }
        }
    }

    async fn send_text(client: &mut Client, value: Value) {
        client.send(ClientMessage::Text(value.to_string().into())).await.unwrap();
    }

    async fn hello(client: &mut Client) -> Value {
        next_control(client).await
    }

    fn state(registry: TunnelRegistry, provider_id: &str, token_ttl_ms: i64) -> TestState {
        TestState { registry, provider_id: provider_id.to_string(), token_ttl_ms }
    }

    fn proxy(
        registry: &TunnelRegistry,
        provider_id: &str,
        path: &'static str,
    ) -> tokio::task::JoinHandle<Result<crate::upstream::UpstreamResponse, crate::tunnel::mux::TunnelFault>> {
        let registry = registry.clone();
        let provider_id = provider_id.to_string();
        tokio::spawn(async move {
            registry.proxy(&provider_id, &Method::POST, path, &HeaderMap::new(), None).await
        })
    }

    #[tokio::test]
    async fn hello_negotiates_the_protocol_version_the_cli_expects() {
        let registry = TunnelRegistry::new();
        let url = serve(state(registry.clone(), "p1", 60_000)).await;
        let mut client = dial(&url).await;
        assert_eq!(hello(&mut client).await, json!({"t":"hello","proto":1,"slug":"my-mac"}));
        assert!(registry.status("p1").connected);
    }

    #[tokio::test]
    async fn the_heartbeat_is_answered_with_pong() {
        let registry = TunnelRegistry::new();
        let url = serve(state(registry, "p1", 60_000)).await;
        let mut client = dial(&url).await;
        hello(&mut client).await;
        client.send(ClientMessage::Text("ping".into())).await.unwrap();
        match next_message(&mut client).await {
            ClientMessage::Text(text) => assert_eq!(text.as_str(), "pong"),
            other => panic!("expected pong, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_request_and_its_response_stream_chunk_by_chunk() {
        let registry = TunnelRegistry::new();
        let url = serve(state(registry.clone(), "p1", 60_000)).await;
        let mut client = dial(&url).await;
        hello(&mut client).await;

        let body: crate::upstream::transport::ByteStream =
            Box::pin(futures::stream::once(async { Ok(Bytes::from_static(b"{\"model\":\"llama3\"}")) }));
        let proxying = {
            let registry = registry.clone();
            tokio::spawn(async move {
                let mut headers = HeaderMap::new();
                headers.insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("application/json"));
                registry.proxy("p1", &Method::POST, "/chat/completions", &headers, Some(body)).await
            })
        };

        let req = next_control(&mut client).await;
        assert_eq!(req["t"], "req");
        assert_eq!(req["path"], "/chat/completions");
        assert_eq!(req["headers"], json!({"content-type":"application/json"}));
        let id = req["id"].as_u64().unwrap() as u32;
        match next_message(&mut client).await {
            ClientMessage::Binary(data) => {
                assert_eq!(data[4], BODY_KIND_REQUEST);
                assert_eq!(&data[5..], br#"{"model":"llama3"}"#);
            }
            other => panic!("expected the request body, got {other:?}"),
        }
        assert_eq!(next_control(&mut client).await, json!({"t":"req_end","id":id}));

        send_text(&mut client, json!({"t":"res","id":id,"status":200,"headers":{"content-type":"text/event-stream"}})).await;
        client
            .send(ClientMessage::Binary(encode_binary_frame(id, BODY_KIND_RESPONSE, b"data: one\n\n").into()))
            .await
            .unwrap();
        client
            .send(ClientMessage::Binary(encode_binary_frame(id, BODY_KIND_RESPONSE, b"data: two\n\n").into()))
            .await
            .unwrap();
        send_text(&mut client, json!({"t":"res_end","id":id})).await;

        let response = proxying.await.unwrap().expect("the local server's answer");
        assert_eq!(response.status, http::StatusCode::OK);
        assert_eq!(response.headers.get(crate::tunnel::mux::AGENT_UPSTREAM_HEADER).unwrap(), "1");
        assert_eq!(response.content_type(), Some("text/event-stream"));
        assert_eq!(response.text().await.unwrap(), "data: one\n\ndata: two\n\n");
    }

    #[tokio::test]
    async fn the_fifth_in_flight_request_is_refused_busy() {
        let registry = TunnelRegistry::new();
        let url = serve(state(registry.clone(), "p1", 60_000)).await;
        let mut client = dial(&url).await;
        hello(&mut client).await;

        let mut open = Vec::new();
        for _ in 0..MAX_INFLIGHT {
            open.push(proxy(&registry, "p1", "/chat/completions"));
            next_control(&mut client).await; // req
            next_control(&mut client).await; // req_end
        }
        let fault = registry
            .proxy("p1", &Method::POST, "/chat/completions", &HeaderMap::new(), None)
            .await
            .err()
            .map(|f| f.reason);
        assert_eq!(fault, Some(AgentFaultReason::Busy));
        for handle in open {
            handle.abort();
        }
    }

    #[tokio::test]
    async fn a_silent_agent_faults_the_request_timeout() {
        let registry = TunnelRegistry::new().with_first_res_timeout(Duration::from_millis(80));
        let url = serve(state(registry.clone(), "p1", 60_000)).await;
        let mut client = dial(&url).await;
        hello(&mut client).await;

        let proxying = proxy(&registry, "p1", "/chat/completions");
        let req = next_control(&mut client).await;
        let id = req["id"].as_u64().unwrap();
        assert_eq!(next_control(&mut client).await, json!({"t":"req_end","id":id}));
        assert_eq!(proxying.await.unwrap().err().map(|f| f.reason), Some(AgentFaultReason::Timeout));
        // The CLI is told to stop working on it.
        assert_eq!(next_control(&mut client).await, json!({"t":"cancel","id":id}));
    }

    #[tokio::test]
    async fn a_second_connect_closes_the_first_with_4001_replaced() {
        let registry = TunnelRegistry::new();
        let url = serve(state(registry.clone(), "p1", 60_000)).await;
        let mut first = dial(&url).await;
        hello(&mut first).await;
        let mut second = dial(&url).await;
        hello(&mut second).await;

        match next_message(&mut first).await {
            ClientMessage::Close(Some(frame)) => {
                assert_eq!(u16::from(frame.code), CLOSE_REPLACED);
                assert_eq!(frame.reason.as_str(), "replaced");
            }
            other => panic!("expected the 4001 close, got {other:?}"),
        }
        // The survivor is still the live socket.
        assert!(registry.status("p1").connected);
        second.send(ClientMessage::Text("ping".into())).await.unwrap();
        match next_message(&mut second).await {
            ClientMessage::Text(text) => assert_eq!(text.as_str(), "pong"),
            other => panic!("expected pong, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_access_token_expiry_closes_with_4003_token_expired() {
        let registry = TunnelRegistry::new();
        let url = serve(state(registry.clone(), "p1", 60)).await;
        let mut client = dial(&url).await;
        hello(&mut client).await;
        match next_message(&mut client).await {
            ClientMessage::Close(Some(frame)) => {
                assert_eq!(u16::from(frame.code), CLOSE_TOKEN_EXPIRED);
                assert_eq!(frame.reason.as_str(), "token_expired");
            }
            other => panic!("expected the 4003 close, got {other:?}"),
        }
        assert!(!registry.is_connected("p1"));
    }

    #[tokio::test]
    async fn an_oversized_response_frame_is_a_protocol_fault() {
        let registry = TunnelRegistry::new();
        let url = serve(state(registry.clone(), "p1", 60_000)).await;
        let mut client = dial(&url).await;
        hello(&mut client).await;

        let proxying = proxy(&registry, "p1", "/chat/completions");
        let id = next_control(&mut client).await["id"].as_u64().unwrap() as u32;
        next_control(&mut client).await; // req_end
        send_text(&mut client, json!({"t":"res","id":id,"status":200,"headers":{}})).await;
        client
            .send(ClientMessage::Binary(
                encode_binary_frame(id, BODY_KIND_RESPONSE, &vec![0u8; MAX_CHUNK_BYTES + 1]).into(),
            ))
            .await
            .unwrap();

        // The response headers already settled, so the fault reaches the body stream.
        let response = proxying.await.unwrap().expect("headers arrived before the bad frame");
        assert!(response.text().await.is_err());
        assert_eq!(next_control(&mut client).await, json!({"t":"cancel","id":id}));
        assert_eq!(registry.status("p1").inflight, 0);
    }

    #[tokio::test]
    async fn a_dropped_socket_faults_open_requests_offline() {
        let registry = TunnelRegistry::new();
        let url = serve(state(registry.clone(), "p1", 60_000)).await;
        let mut client = dial(&url).await;
        hello(&mut client).await;
        let proxying = proxy(&registry, "p1", "/chat/completions");
        next_control(&mut client).await;
        client.close(None).await.unwrap();
        assert_eq!(proxying.await.unwrap().err().map(|f| f.reason), Some(AgentFaultReason::Offline));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!registry.is_connected("p1"));
    }

    struct Sink {
        reports: Mutex<Vec<Vec<String>>>,
        fail: bool,
    }

    #[async_trait]
    impl ModelsSink for Sink {
        async fn persist_models(&self, _provider_id: &str, models: Vec<String>) -> anyhow::Result<()> {
            if self.fail {
                anyhow::bail!("d1 write failed");
            }
            self.reports.lock().unwrap().push(models);
            Ok(())
        }
    }

    struct Observer {
        seen: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl ConnectObserver for Observer {
        async fn on_connect(&self, _user_id: &str, slug: &str, _provider_id: &str) {
            self.seen.lock().unwrap().push(slug.to_string());
        }
    }

    #[tokio::test]
    async fn a_models_report_is_validated_and_persisted() {
        let sink = Arc::new(Sink { reports: Mutex::new(Vec::new()), fail: false });
        let observer = Arc::new(Observer { seen: Mutex::new(Vec::new()) });
        let registry = TunnelRegistry::new()
            .with_models_sink(sink.clone())
            .with_connect_observer(observer.clone());
        let url = serve(state(registry, "p1", 60_000)).await;
        let mut client = dial(&url).await;
        hello(&mut client).await;

        send_text(&mut client, json!({"t":"models","models":["llama3.3:70b","org/model"]})).await;
        send_text(&mut client, json!({"t":"models","models":["bad id"]})).await;
        send_text(&mut client, json!({"t":"models","models":["x".repeat(129)]})).await;
        // A round trip proves the earlier frames were processed.
        client.send(ClientMessage::Text("ping".into())).await.unwrap();
        next_message(&mut client).await;

        assert_eq!(
            sink.reports.lock().unwrap().clone(),
            vec![vec!["llama3.3:70b".to_string(), "org/model".to_string()]]
        );
        assert_eq!(observer.seen.lock().unwrap().clone(), vec!["my-mac".to_string()]);
    }

    #[tokio::test]
    async fn a_failed_models_write_closes_4008_so_the_reconnect_re_reports() {
        let sink = Arc::new(Sink { reports: Mutex::new(Vec::new()), fail: true });
        let registry = TunnelRegistry::new().with_models_sink(sink);
        let url = serve(state(registry.clone(), "p1", 60_000)).await;
        let mut client = dial(&url).await;
        hello(&mut client).await;
        send_text(&mut client, json!({"t":"models","models":["llama3"]})).await;
        match next_message(&mut client).await {
            ClientMessage::Close(Some(frame)) => {
                assert_eq!(u16::from(frame.code), CLOSE_RETRY);
                assert_eq!(frame.reason.as_str(), "models_persist_failed");
            }
            other => panic!("expected the 4008 close, got {other:?}"),
        }
        assert!(!registry.is_connected("p1"));
    }
}
