//! The long-running tunnel (docs/cli.md § Runtime behavior): one WebSocket
//! per registered provider, protocol v1, streaming both directions
//! chunk-for-chunk with no whole-body buffering. Reconnects forever on
//! network failures (1s → 60s full-jitter backoff); `4003` refreshes the
//! token silently; `4001` drops the provider (another `start` owns it); a
//! failed refresh / 401 exits with a message to re-run init.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, Mutex, Semaphore};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;

use crate::api;
use crate::protocol::{
    decode_binary, encode_binary, encode_control, is_allowed_path, parse_control, ControlFrame,
    AGENT_PROTO, BODY_KIND_REQUEST, BODY_KIND_RESPONSE, CLOSE_REPLACED, CLOSE_TOKEN_EXPIRED,
    MAX_CHUNK_BYTES,
};
use crate::state::{ProviderState, StateFile};

const HEARTBEAT: Duration = Duration::from_secs(30);
const ACCESS_TOKEN_REUSE: Duration = Duration::from_secs(50 * 60);

/// Shared access-token cache: one rotation serves every provider socket until
/// it nears expiry. `force_refresh` is the 4003 path.
pub struct TokenCache {
    file: Arc<StateFile>,
    cached: Mutex<Option<(String, Instant)>>,
}

impl TokenCache {
    pub fn new(file: Arc<StateFile>) -> Self {
        Self { file, cached: Mutex::new(None) }
    }

    pub async fn get(&self, force: bool) -> Result<String> {
        let mut cached = self.cached.lock().await;
        if !force {
            if let Some((token, at)) = cached.as_ref() {
                if at.elapsed() < ACCESS_TOKEN_REUSE {
                    return Ok(token.clone());
                }
            }
        }
        let token = api::fresh_access_token(&self.file).await?;
        *cached = Some((token.clone(), Instant::now()));
        Ok(token)
    }
}

/// Deterministic-enough full jitter without a rand dependency: subsecond
/// clock noise scaled into [0, cap].
fn jitter_ms(cap_ms: u64) -> u64 {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().subsec_nanos() as u64;
    nanos % cap_ms.max(1)
}

enum SocketOutcome {
    /// Reconnect after backoff (network failure, unexpected close).
    Retry,
    /// Refresh the token and reconnect immediately (4003).
    RefreshAndRetry,
    /// Another `start` took this provider over (4001) — stop, don't fight it.
    Replaced,
    /// Clean shutdown requested.
    Shutdown,
}

pub async fn run_provider(
    file: Arc<StateFile>,
    tokens: Arc<TokenCache>,
    provider: ProviderState,
    concurrency: usize,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut backoff = Duration::from_secs(1);
    let mut force_refresh = false;
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return Ok(());
        }
        let token = match tokens.get(force_refresh).await {
            Ok(t) => t,
            Err(e) => {
                // A rejected refresh is terminal: the device was revoked or
                // unregistered. Retrying cannot help (docs/cli.md).
                return Err(anyhow!("{}: sign-in rejected ({e}) — run `kano-proxy init` again", provider.slug));
            }
        };
        force_refresh = false;

        match run_socket(&file, &provider, &token, concurrency, &shutdown).await {
            Ok(SocketOutcome::Shutdown) => return Ok(()),
            Ok(SocketOutcome::Replaced) => {
                eprintln!("[{}] replaced by another `kano-proxy start` — dropping this provider", provider.slug);
                return Ok(());
            }
            Ok(SocketOutcome::RefreshAndRetry) => {
                force_refresh = true;
                backoff = Duration::from_secs(1);
            }
            Ok(SocketOutcome::Retry) | Err(_) => {
                let wait = backoff + Duration::from_millis(jitter_ms(backoff.as_millis() as u64));
                eprintln!("[{}] disconnected — reconnecting in {:.0?}", provider.slug, wait);
                tokio::time::sleep(wait).await;
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        }
    }
}

struct InflightRequest {
    body_tx: Option<mpsc::Sender<Result<Vec<u8>, std::io::Error>>>,
    abort: tokio::task::AbortHandle,
}

type Registry = Arc<Mutex<HashMap<u32, InflightRequest>>>;

async fn run_socket(
    file: &Arc<StateFile>,
    provider: &ProviderState,
    access_token: &str,
    concurrency: usize,
    shutdown: &Arc<AtomicBool>,
) -> Result<SocketOutcome> {
    let state = file.load()?;
    let ws_base = state
        .base_url
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    let url = format!("{}/agent/v1/connect/{}", ws_base, provider.id);
    let mut request = url.clone().into_client_request().context("bad connect URL")?;
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {access_token}").parse().unwrap());

    let (stream, _res) = match tokio_tungstenite::connect_async(request).await {
        Ok(ok) => ok,
        Err(tokio_tungstenite::tungstenite::Error::Http(res)) if res.status() == 401 => {
            return Err(anyhow!("connect rejected (401)"));
        }
        Err(e) => {
            eprintln!("[{}] connect failed: {e}", provider.slug);
            return Ok(SocketOutcome::Retry);
        }
    };
    let (mut sink, mut source) = stream.split();

    // Single writer: every task sends through this channel.
    let (out_tx, mut out_rx) = mpsc::channel::<Message>(64);
    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let closing = matches!(msg, Message::Close(_));
            if sink.send(msg).await.is_err() {
                break;
            }
            if closing {
                let _ = sink.flush().await;
                break;
            }
        }
    });

    let registry: Registry = Arc::new(Mutex::new(HashMap::new()));
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let http = reqwest::Client::builder()
        .user_agent(concat!("kano-proxy-cli/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("reqwest client builds");

    // Heartbeat: text "ping", answered by the DO's auto-response without
    // waking it (docs/cli.md § AgentTunnel).
    let hb_tx = out_tx.clone();
    let heartbeat = tokio::spawn(async move {
        loop {
            tokio::time::sleep(HEARTBEAT).await;
            if hb_tx.send(Message::Text("ping".into())).await.is_err() {
                break;
            }
        }
    });

    // Model report: push after hello, then re-check every 5 minutes and push
    // only on change (docs/cli.md § Model catalog).
    let models_tx = out_tx.clone();
    let models_provider = provider.clone();
    let models = tokio::spawn(async move {
        let mut last_sent: Option<Vec<String>> = None;
        loop {
            match api::probe_local_models(
                &models_provider.format,
                &models_provider.target,
                models_provider.target_key.as_deref(),
            )
            .await
            {
                Ok(models) if last_sent.as_ref() != Some(&models) => {
                    let frame = encode_control(&ControlFrame::Models { models: models.clone() });
                    if models_tx.send(Message::Text(frame.into())).await.is_err() {
                        break;
                    }
                    eprintln!("[{}] reported {} models", models_provider.slug, models.len());
                    last_sent = Some(models);
                }
                Ok(_) => {}
                Err(e) => eprintln!("[{}] models probe failed: {e}", models_provider.slug),
            }
            tokio::time::sleep(Duration::from_secs(300)).await;
        }
    });

    let outcome = read_loop(provider, &mut source, &out_tx, &registry, &semaphore, &http, shutdown).await;

    heartbeat.abort();
    models.abort();
    // Abort in-flight local requests — the DO already faulted them when the
    // socket dropped, or is about to.
    for (_, inflight) in registry.lock().await.drain() {
        inflight.abort.abort();
    }
    if matches!(outcome, Ok(SocketOutcome::Shutdown)) {
        let _ = out_tx
            .send(Message::Close(Some(CloseFrame { code: CloseCode::Normal, reason: "".into() })))
            .await;
        let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
    } else {
        writer.abort();
    }
    outcome
}

async fn read_loop(
    provider: &ProviderState,
    source: &mut (impl StreamExt<Item = std::result::Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin),
    out_tx: &mpsc::Sender<Message>,
    registry: &Registry,
    semaphore: &Arc<Semaphore>,
    http: &reqwest::Client,
    shutdown: &Arc<AtomicBool>,
) -> Result<SocketOutcome> {
    loop {
        let msg = tokio::select! {
            msg = source.next() => msg,
            _ = wait_for_shutdown(shutdown) => return Ok(SocketOutcome::Shutdown),
        };
        let Some(msg) = msg else {
            return Ok(SocketOutcome::Retry);
        };
        let msg = match msg {
            Ok(m) => m,
            Err(_) => return Ok(SocketOutcome::Retry),
        };
        match msg {
            Message::Text(text) => {
                if text == "pong" {
                    continue;
                }
                let Some(frame) = parse_control(&text) else { continue };
                match frame {
                    ControlFrame::Hello { proto, slug } => {
                        if proto != AGENT_PROTO {
                            return Err(anyhow!(
                                "server speaks protocol {proto}, this CLI speaks {AGENT_PROTO} — upgrade with `kano-proxy update`"
                            ));
                        }
                        eprintln!("[{}] connected (proto {proto})", slug);
                    }
                    ControlFrame::Req { id, method, path, headers } => {
                        spawn_request(provider, id, method, path, headers, out_tx, registry, semaphore, http).await;
                    }
                    ControlFrame::ReqEnd { id } => {
                        // Body complete: close the request's body channel.
                        let mut reg = registry.lock().await;
                        if let Some(inflight) = reg.get_mut(&id) {
                            inflight.body_tx = None;
                        }
                    }
                    ControlFrame::Cancel { id } => {
                        let mut reg = registry.lock().await;
                        if let Some(inflight) = reg.remove(&id) {
                            inflight.abort.abort();
                        }
                    }
                    // Frames that only flow CLI → DO; ignore echoes.
                    ControlFrame::Res { .. }
                    | ControlFrame::ResEnd { .. }
                    | ControlFrame::ResErr { .. }
                    | ControlFrame::Models { .. } => {}
                }
            }
            Message::Binary(data) => {
                let Some((id, kind, chunk)) = decode_binary(&data) else { continue };
                if kind != BODY_KIND_REQUEST {
                    continue;
                }
                let sender = {
                    let reg = registry.lock().await;
                    reg.get(&id).and_then(|r| r.body_tx.clone())
                };
                if let Some(tx) = sender {
                    // Backpressure: an unread body slows this read loop down,
                    // which is exactly the flow control the pipe needs.
                    let _ = tx.send(Ok(chunk.to_vec())).await;
                }
            }
            Message::Close(frame) => {
                let code = frame.as_ref().map(|f| u16::from(f.code)).unwrap_or(1000);
                return Ok(match code {
                    CLOSE_REPLACED => SocketOutcome::Replaced,
                    CLOSE_TOKEN_EXPIRED => SocketOutcome::RefreshAndRetry,
                    _ => SocketOutcome::Retry,
                });
            }
            _ => {}
        }
    }
}

async fn wait_for_shutdown(flag: &Arc<AtomicBool>) {
    while !flag.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn spawn_request(
    provider: &ProviderState,
    id: u32,
    method: String,
    path: String,
    headers: std::collections::BTreeMap<String, String>,
    out_tx: &mpsc::Sender<Message>,
    registry: &Registry,
    semaphore: &Arc<Semaphore>,
    http: &reqwest::Client,
) {
    let out = out_tx.clone();
    // Both ends distrust the other: re-check the allowlist even though the DO
    // already enforced it (docs/cli.md § Wire protocol).
    if !is_allowed_path(&provider.format, &path) {
        let _ = out
            .send(Message::Text(encode_control(&ControlFrame::ResErr { id, reason: "aborted".into() }).into()))
            .await;
        return;
    }
    let Ok(permit) = semaphore.clone().try_acquire_owned() else {
        // Should not happen — the DO caps in-flight at the same bound — but
        // refuse locally rather than queueing.
        let _ = out
            .send(Message::Text(encode_control(&ControlFrame::ResErr { id, reason: "aborted".into() }).into()))
            .await;
        return;
    };

    let (body_tx, body_rx) = mpsc::channel::<Result<Vec<u8>, std::io::Error>>(8);
    let slug = provider.slug.clone();
    let target = provider.target.trim_end_matches('/').to_string();
    let target_key = provider.target_key.clone();
    let format = provider.format.clone();
    let registry_for_task = registry.clone();
    let http = http.clone();

    let handle = tokio::spawn(async move {
        let _permit = permit;
        let started = Instant::now();
        let url = format!("{target}{path}");
        let mut req = http.request(method.parse().unwrap_or(reqwest::Method::POST), &url);
        for (name, value) in &headers {
            // Auth is ours to add, never the tunnel's to smuggle.
            if name.eq_ignore_ascii_case("authorization") || name.eq_ignore_ascii_case("x-api-key") {
                continue;
            }
            req = req.header(name.as_str(), value.as_str());
        }
        if let Some(key) = &target_key {
            req = match format.as_str() {
                "anthropic" => req.header("x-api-key", key.as_str()),
                _ => req.bearer_auth(key),
            };
        }
        // GET/HEAD carry no body on this protocol (the DO sends req_end with
        // no chunks) and some local servers reject a chunked body on them.
        if !matches!(method.as_str(), "GET" | "HEAD") {
            let body_stream = futures_util::stream::unfold(body_rx, |mut rx| async move {
                rx.recv().await.map(|item| (item, rx))
            });
            req = req.body(reqwest::Body::wrap_stream(body_stream));
        } else {
            drop(body_rx);
        }

        let result = req.send().await;
        let status = match &result {
            Ok(res) => res.status().as_u16(),
            Err(_) => 0,
        };
        match result {
            Ok(res) => {
                let mut headers = std::collections::BTreeMap::new();
                if let Some(ct) = res.headers().get("content-type").and_then(|v| v.to_str().ok()) {
                    // Header reduction discipline: content-type only.
                    headers.insert("content-type".to_string(), ct.to_string());
                }
                let frame = encode_control(&ControlFrame::Res { id, status, headers });
                if out.send(Message::Text(frame.into())).await.is_err() {
                    return;
                }
                let mut body = res.bytes_stream();
                while let Some(chunk) = body.next().await {
                    match chunk {
                        Ok(bytes) => {
                            for part in bytes.chunks(MAX_CHUNK_BYTES) {
                                let frame = encode_binary(id, BODY_KIND_RESPONSE, part);
                                if out.send(Message::Binary(frame.into())).await.is_err() {
                                    return;
                                }
                            }
                        }
                        Err(_) => {
                            let _ = out
                                .send(Message::Text(
                                    encode_control(&ControlFrame::ResErr { id, reason: "aborted".into() }).into(),
                                ))
                                .await;
                            registry_for_task.lock().await.remove(&id);
                            return;
                        }
                    }
                }
                let _ = out.send(Message::Text(encode_control(&ControlFrame::ResEnd { id }).into())).await;
                eprintln!("[{slug}] #{id} {method} {path} -> {status} ({}ms)", started.elapsed().as_millis());
            }
            Err(e) => {
                let reason = if e.is_connect() {
                    "connect_refused"
                } else if e.is_timeout() {
                    "timeout"
                } else {
                    "aborted"
                };
                let _ = out
                    .send(Message::Text(encode_control(&ControlFrame::ResErr { id, reason: reason.into() }).into()))
                    .await;
                eprintln!("[{slug}] #{id} {method} {path} -> local {reason} ({}ms)", started.elapsed().as_millis());
            }
        }
        registry_for_task.lock().await.remove(&id);
    });

    registry.lock().await.insert(
        id,
        InflightRequest { body_tx: Some(body_tx), abort: handle.abort_handle() },
    );
}
