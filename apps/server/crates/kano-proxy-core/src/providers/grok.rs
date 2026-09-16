//! The Grok provider (docs/providers.md § Grok, docs/auth.md § Grok).
//!
//! Two upstream surfaces behind one adapter:
//!   - `/openai/v1` → Chat Completions on `api.x.ai` (near-passthrough, `include_reasoning`)
//!   - `/anthropic` → Responses on `cli-chat-proxy.grok.com` via [`crate::proxy::grok_anthropic`],
//!     with the session replay cache and the opaque-decode (compaction blob) recovery.
//!
//! Sticky ids (`x-grok-conv-id`, `x-grok-session-id`, `x-grok-turn-idx`) are forwarded only
//! when the client supplied them — never synthesized (measurement in docs/providers.md).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::response::Response;
use bytes::Bytes;
use http::{header, HeaderMap, StatusCode};
use rand::RngCore;
use serde_json::{json, Map, Value};

use crate::db::accounts::iso_from_ms;
use crate::pool::{AcquiredAccount, StoredCredential};
use crate::providers::grok_reasoning_cache::{
    delete_grok_reasoning_replay, grok_reasoning_replay_session_key, hash_assistant_text, read_grok_reasoning_replay,
    write_grok_reasoning_replay, GrokReasoningReplayEntry,
};
use crate::providers::grok_reasoning_recovery::{
    affinity_present, is_grok_opaque_decode_failure, strip_prompt_cache_key, strip_responses_opaque_state,
};
use crate::providers::refresh::refresh_oauth_credential;
use crate::providers::types::{
    AdapterError, AffinityIds, AudioInput, CallExtras, ChatCompletionRequest, DynAdapter, FetchedUsage, ListedModels,
    ProviderAdapter, UpstreamModel, UsageWindow,
};
use crate::providers::ProviderId;
use crate::proxy::grok_anthropic::{
    anthropic_to_grok_responses, collect_grok_responses_sse_to_anthropic, grok_responses_sse_to_anthropic_stream,
    last_assistant_text_from_anthropic_messages, GrokStreamOptions, GrokTurnOutcome,
};
use crate::upstream::transport::{TransportError, UpstreamRequest, UpstreamResponse};
use crate::utils::reasoning::map_reasoning;
use crate::AppState;

const XAI_API: &str = "https://api.x.ai/v1";
const CLI_CHAT_PROXY: &str = "https://cli-chat-proxy.grok.com/v1";
const DEFAULT_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";

/// Bound refresh / models / usage fetches so a hung xAI edge cannot stall a request.
const REFRESH_TIMEOUT: Duration = Duration::from_millis(10_000);

/// Official Grok Build CLI client identity — used on the Chat Completions (`api.x.ai`) path.
const GROK_CLI_VERSION: &str = "0.2.117";
const GROK_CLIENT_IDENTIFIER: &str = "grok-shell";

/// CLI chat-proxy identity — used on `/anthropic` → Responses (`xai-grok-workspace`).
const GROK_WORKSPACE_VERSION: &str = "0.2.93";

fn grok_user_agent() -> String {
    format!("{GROK_CLIENT_IDENTIFIER}/{GROK_CLI_VERSION} (linux; x86_64)")
}

fn grok_workspace_user_agent() -> String {
    format!("xai-grok-workspace/{GROK_WORKSPACE_VERSION}")
}

/// `crypto.randomUUID()` — a v4 uuid for `x-grok-req-id`.
fn random_uuid() -> String {
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex::encode(b);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

fn with_chat_completions_headers(req: UpstreamRequest, access_token: &str) -> UpstreamRequest {
    req.header("authorization", &format!("Bearer {access_token}"))
        .header("user-agent", &grok_user_agent())
        .header("x-grok-client-identifier", GROK_CLIENT_IDENTIFIER)
}

fn with_responses_headers(req: UpstreamRequest, access_token: &str, affinity: Option<&AffinityIds>) -> UpstreamRequest {
    let mut req = req
        .header("authorization", &format!("Bearer {access_token}"))
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .header("user-agent", &grok_workspace_user_agent())
        .header("x-grok-client-version", GROK_WORKSPACE_VERSION)
        .header("x-xai-token-auth", "xai-grok-cli");
    if let Some(aff) = affinity {
        if let Some(v) = non_empty(aff.conv_id.as_deref()) {
            req = req.header("x-grok-conv-id", v);
        }
        if let Some(v) = non_empty(aff.session_id.as_deref()) {
            req = req.header("x-grok-session-id", v);
        }
        if let Some(v) = non_empty(aff.turn_idx.as_deref()) {
            req = req.header("x-grok-turn-idx", v);
        }
    }
    req
}

fn non_empty(v: Option<&str>) -> Option<&str> {
    v.filter(|s| !s.is_empty())
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn affinity_from_headers(headers: &HeaderMap) -> AffinityIds {
    AffinityIds {
        conv_id: header_str(headers, "x-grok-conv-id").map(str::to_string),
        session_id: header_str(headers, "x-grok-session-id").map(str::to_string),
        turn_idx: header_str(headers, "x-grok-turn-idx").map(str::to_string),
    }
}

fn client_id(cx: &AppState) -> String {
    cx.config()
        .grok_oauth_client_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_CLIENT_ID)
        .to_string()
}

/// Refresh an hour before expiry (or when the stored credential carries no expiry at all),
/// through the shared single-flight in `providers::refresh`.
async fn refresh_grok(cx: &AppState, account: AcquiredAccount) -> AcquiredAccount {
    let fallback_client_id = client_id(cx);
    refresh_oauth_credential(
        cx,
        account,
        |credential: &StoredCredential| {
            if credential.refresh_token.is_none() || credential.token_endpoint.is_none() {
                return false;
            }
            let exp = credential
                .expires_at
                .as_deref()
                .and_then(crate::db::accounts::parse_iso_ms)
                .unwrap_or(0);
            exp == 0 || exp - 3_600_000 <= crate::app::now_ms()
        },
        move |credential: &StoredCredential| {
            let credential = credential.clone();
            let fallback_client_id = fallback_client_id.clone();
            async move {
                let endpoint = credential.token_endpoint.clone()?;
                let refresh_token = credential.refresh_token.clone()?;
                let form = url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("grant_type", "refresh_token")
                    .append_pair("refresh_token", &refresh_token)
                    .append_pair(
                        "client_id",
                        credential.client_id.as_deref().filter(|s| !s.is_empty()).unwrap_or(&fallback_client_id),
                    )
                    .finish();
                let req = UpstreamRequest::post(endpoint)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Bytes::from(form))
                    .timeout(REFRESH_TIMEOUT);
                let res = cx.transport().send(req).await.ok()?;
                if !res.status.is_success() {
                    return None;
                }
                let json = res.json_value().await.ok()?;
                let access_token = json.get("access_token")?.as_str()?.to_string();
                let refresh_token = json
                    .get("refresh_token")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or(credential.refresh_token.clone());
                let expires_at = match json.get("expires_in").and_then(Value::as_i64) {
                    Some(seconds) => Some(iso_from_ms(crate::app::now_ms() + seconds * 1000)),
                    None => credential.expires_at.clone(),
                };
                Some(StoredCredential { access_token, refresh_token, expires_at, ..credential })
            }
        },
    )
    .await
}

fn json_response(status: StatusCode, value: &Value) -> Response {
    let body = serde_json::to_vec(value).expect("json response serializes");
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .expect("json response builds")
}

/// Forward an upstream response verbatim, streaming its body. Framing headers are dropped:
/// the transport may have decoded the payload, so the stored lengths no longer describe it.
fn passthrough(res: UpstreamResponse) -> Response {
    let mut builder = Response::builder().status(res.status);
    for (name, value) in res.headers.iter() {
        if name == header::CONTENT_LENGTH || name == header::CONTENT_ENCODING || name == header::TRANSFER_ENCODING {
            continue;
        }
        builder = builder.header(name, value);
    }
    builder.body(Body::from_stream(res.body)).expect("upstream response builds")
}

/// `raw_body`'s own number keeps the client's exact literal (`0` stays `0`); the parsed
/// request field is the fallback for callers that did not carry one.
fn number_field(raw_body: &Map<String, Value>, key: &str, parsed: Option<f64>) -> Option<Value> {
    if let Some(v) = raw_body.get(key).filter(|v| v.is_number()) {
        return Some(v.clone());
    }
    parsed.map(Value::from)
}

pub struct GrokAdapter;

pub fn adapter() -> DynAdapter {
    Arc::new(GrokAdapter)
}

#[async_trait]
impl ProviderAdapter for GrokAdapter {
    fn id(&self) -> &str {
        ProviderId::Grok.as_str()
    }

    /// `messages` goes to xAI verbatim, so an audio part is xAI's call, not ours.
    fn audio_input(&self) -> Option<AudioInput> {
        Some(AudioInput::Passthrough)
    }

    async fn refresh_if_needed(&self, cx: &AppState, account: AcquiredAccount) -> Result<AcquiredAccount, AdapterError> {
        Ok(refresh_grok(cx, account).await)
    }

    fn has_list_models(&self) -> bool {
        true
    }

    async fn list_models(&self, cx: &AppState, account: &AcquiredAccount) -> ListedModels {
        let acc = refresh_grok(cx, account.clone()).await;
        let req = with_chat_completions_headers(UpstreamRequest::get(format!("{XAI_API}/models")), &acc.credential.access_token)
            .header("accept", "application/json")
            .timeout(REFRESH_TIMEOUT);
        let res = match cx.transport().send(req).await {
            Ok(res) => res,
            Err(e) => return ListedModels { models: Vec::new(), error: Some(e.to_string()) },
        };
        if !res.status.is_success() {
            return ListedModels { models: Vec::new(), error: Some(format!("models {}", res.status.as_u16())) };
        }
        let json = match res.json_value().await {
            Ok(json) => json,
            Err(e) => return ListedModels { models: Vec::new(), error: Some(e.to_string()) },
        };
        let models = json
            .get("data")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|m| {
                        let id = non_empty(m.get("id").and_then(Value::as_str))?;
                        Some(UpstreamModel {
                            id: id.to_string(),
                            display_name: non_empty(m.get("name").and_then(Value::as_str)).map(str::to_string),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        ListedModels { models, error: None }
    }

    /// OpenAI surface: Chat Completions on api.x.ai (unchanged wire format).
    /// The Anthropic surface uses `messages()` → Responses instead.
    async fn chat_completions(
        &self,
        cx: &AppState,
        account: &AcquiredAccount,
        req: &ChatCompletionRequest,
        extras: &CallExtras,
    ) -> Result<Response, AdapterError> {
        let acc = refresh_grok(cx, account.clone()).await;
        let mapped = map_reasoning(ProviderId::Grok, req.reasoning_effort);

        let mut body = Map::new();
        body.insert("model".into(), json!(req.upstream_model));
        body.insert("messages".into(), Value::Array(req.messages.clone()));
        body.insert("stream".into(), json!(req.stream.unwrap_or(false)));
        if let Some(max_tokens) = req.max_tokens {
            body.insert("max_tokens".into(), json!(max_tokens));
        }
        if let Some(tools) = req.tools.as_ref().filter(|v| !v.is_null()) {
            body.insert("tools".into(), tools.clone());
        }
        if let Some(tool_choice) = req.tool_choice.as_ref().filter(|v| !v.is_null()) {
            body.insert("tool_choice".into(), tool_choice.clone());
        }
        if let Some(response_format) = req.response_format.as_ref().filter(|v| !v.is_null()) {
            body.insert("response_format".into(), response_format.clone());
        }
        if let Some(stop) = req.stop.as_ref().filter(|s| !s.is_empty()) {
            body.insert("stop".into(), json!(stop));
        }
        if let Some(effort) = mapped.get("reasoning_effort") {
            body.insert("reasoning_effort".into(), effort.clone());
        }
        // Ask for the final usage chunk: without it every converted Anthropic response
        // reports input_tokens 0 and clients cannot track context.
        if req.stream.unwrap_or(false) {
            body.insert("stream_options".into(), json!({ "include_usage": true }));
        }
        // Pin the surface default explicitly: Anthropic and OpenAI both document `1`,
        // xAI's own default is unspecified (docs/providers.md "Sampling").
        body.insert(
            "temperature".into(),
            number_field(&req.raw_body, "temperature", req.temperature).unwrap_or_else(|| json!(1)),
        );
        if let Some(top_p) = number_field(&req.raw_body, "top_p", req.top_p) {
            body.insert("top_p".into(), top_p);
        }
        // Chat Completions still needs this flag for plaintext reasoning_content when the
        // egress gate allows it (docs/api.md).
        body.insert("include_reasoning".into(), json!(true));

        let mut request = with_chat_completions_headers(
            UpstreamRequest::post(format!("{XAI_API}/chat/completions")),
            &acc.credential.access_token,
        )
        // Per-request id, like the CLI's x_grok_req_id.
        .header("x-grok-req-id", &random_uuid())
        .header("x-grok-model-override", &req.upstream_model)
        .json(&Value::Object(body));
        // Sticky routing for prompt cache — only when the client supplied an id.
        request = apply_affinity_headers(request, req.affinity.as_ref());
        if let Some(timeout) = extras.first_byte_timeout {
            request = request.timeout(timeout);
        }

        let res = cx.transport().send(request).await?;
        Ok(passthrough(res))
    }

    fn has_messages(&self) -> bool {
        true
    }

    /// Anthropic surface: Messages ↔ cli-chat-proxy Responses with encrypted reasoning.
    /// The caller's api-key id isolates the replay cache.
    async fn messages(
        &self,
        cx: &AppState,
        account: &AcquiredAccount,
        body: &Value,
        headers: &HeaderMap,
        extras: &CallExtras,
    ) -> Result<Response, AdapterError> {
        let acc = refresh_grok(cx, account.clone()).await;

        let affinity = affinity_from_headers(headers);
        let api_key_id = extras
            .api_key_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| header_str(headers, "x-kano-api-key-id").map(str::trim).filter(|s| !s.is_empty()))
            .unwrap_or("")
            .to_string();
        let session_key = grok_reasoning_replay_session_key(Some(&affinity)).unwrap_or_default();
        let raw_model = body.get("model").and_then(Value::as_str).unwrap_or("");
        // Routing already rewrote model to the bare upstream id for native adapters; keep a
        // display id if the client sent provider/model.
        let upstream_model = match raw_model.find('/') {
            Some(i) => &raw_model[i + 1..],
            None => raw_model,
        }
        .to_string();
        let display_model = header_str(headers, "x-kano-raw-model")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("grok/{upstream_model}"));
        let cacheable = !api_key_id.is_empty() && !session_key.is_empty();

        let mut replay_encrypted: Option<String> = None;
        if cacheable {
            let disabled = body
                .get("thinking")
                .filter(|t| t.is_object())
                .and_then(|t| t.get("type"))
                .and_then(Value::as_str)
                .map(|t| t.to_lowercase() == "disabled")
                .unwrap_or(false);
            // Empty assistant text (tool-only turns) shares one hash — refuse a replay match
            // rather than risk injecting another turn's ciphertext.
            let last_text = last_assistant_text_from_anthropic_messages(body);
            if !disabled && !last_text.is_empty() {
                if let Some(cached) = read_grok_reasoning_replay(cx.cache(), &api_key_id, &upstream_model, &session_key).await {
                    if hash_assistant_text(&last_text) == cached.assistant_text_hash {
                        replay_encrypted = Some(cached.encrypted_content);
                    }
                }
            }
        }

        let converted = match anthropic_to_grok_responses(body, &upstream_model, replay_encrypted.as_deref()) {
            Ok(converted) => converted,
            Err(_) => {
                return Ok(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({
                        "type": "error",
                        "error": { "type": "invalid_request_error", "message": "invalid reasoning_effort" },
                    }),
                ))
            }
        };

        let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);

        // Opaque-state decode recovery (compaction / encrypted_content 400):
        // 1) clear the replay cache entry for this session
        // 2) strip reasoning.encrypted_content (+ drop compaction items), retry
        // 3) if still decode-fail and affinity was set, drop sticky headers + prompt_cache_key
        // On unrecovered failure, return the *original* upstream 400 body.
        let mut res = post_responses(cx, &acc.credential.access_token, &converted.body, Some(&affinity), extras).await?;
        if !res.status.is_success() {
            let original_status = res.status;
            let original_type = res.content_type().unwrap_or("application/json").to_string();
            let original_text = res.text().await.unwrap_or_default();
            let mut recovered: Option<UpstreamResponse> = None;

            if original_status == StatusCode::BAD_REQUEST && is_grok_opaque_decode_failure(&original_text) {
                if cacheable {
                    delete_grok_reasoning_replay(cx.cache(), &api_key_id, &upstream_model, &session_key).await;
                }

                let stripped = strip_responses_opaque_state(&converted.body);
                if stripped.changed {
                    let retry = post_responses(cx, &acc.credential.access_token, &stripped.body, Some(&affinity), extras).await?;
                    if retry.status.is_success() {
                        recovered = Some(retry);
                    } else {
                        let retry_status = retry.status;
                        let retry_text = retry.text().await.unwrap_or_default();
                        if retry_status == StatusCode::BAD_REQUEST
                            && is_grok_opaque_decode_failure(&retry_text)
                            && affinity_present(Some(&affinity))
                            && !has_key(&stripped.body, "previous_response_id")
                        {
                            let stateless = strip_prompt_cache_key(&stripped.body);
                            let reset = post_responses(cx, &acc.credential.access_token, &stateless, None, extras).await?;
                            if reset.status.is_success() {
                                recovered = Some(reset);
                            } else {
                                let _ = reset.bytes().await;
                            }
                        }
                    }
                } else if affinity_present(Some(&affinity)) && !has_key(&converted.body, "previous_response_id") {
                    // No ciphertext in the body — sticky session/cache alone may be bad.
                    let stateless = strip_prompt_cache_key(&converted.body);
                    let reset = post_responses(cx, &acc.credential.access_token, &stateless, None, extras).await?;
                    if reset.status.is_success() {
                        recovered = Some(reset);
                    } else {
                        let _ = reset.bytes().await;
                    }
                }
            }

            match recovered {
                None => {
                    return Ok(Response::builder()
                        .status(original_status)
                        .header(header::CONTENT_TYPE, original_type)
                        .body(Body::from(original_text))
                        .expect("upstream error response builds"))
                }
                Some(next) => res = next,
            }
        }

        let opts = GrokStreamOptions::default().thinking_mode(converted.thinking_mode).on_turn_outcome({
            let cache = cx.cache().clone();
            let api_key_id = api_key_id.clone();
            let model = upstream_model.clone();
            let session_key = session_key.clone();
            move |outcome: GrokTurnOutcome| {
                if !cacheable {
                    return;
                }
                let cache = cache.clone();
                let api_key_id = api_key_id.clone();
                let model = model.clone();
                let session_key = session_key.clone();
                match outcome {
                    GrokTurnOutcome::Clear => {
                        tokio::spawn(async move {
                            delete_grok_reasoning_replay(&cache, &api_key_id, &model, &session_key).await;
                        });
                    }
                    // Skip persisting tool-only (empty text) turns — a match would be ambiguous.
                    GrokTurnOutcome::Replayable { encrypted_content, assistant_text } => {
                        if encrypted_content.is_empty() || assistant_text.is_empty() {
                            return;
                        }
                        tokio::spawn(async move {
                            let entry = GrokReasoningReplayEntry {
                                encrypted_content,
                                assistant_text_hash: hash_assistant_text(&assistant_text),
                            };
                            write_grok_reasoning_replay(&cache, &api_key_id, &model, &session_key, &entry).await;
                        });
                    }
                }
            }
        });

        if stream {
            let anthropic = grok_responses_sse_to_anthropic_stream(res.body, &display_model, opts);
            return Ok(Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
                .header(header::CACHE_CONTROL, "no-cache")
                .body(Body::from_stream(anthropic))
                .expect("sse response builds"));
        }

        let msg = collect_grok_responses_sse_to_anthropic(res.body, &display_model, opts).await;
        if let Some(error) = msg.get("error") {
            return Ok(json_response(StatusCode::BAD_GATEWAY, &json!({ "type": "error", "error": error })));
        }
        Ok(json_response(StatusCode::OK, &msg))
    }

    fn has_fetch_usage(&self) -> bool {
        true
    }

    /// Unofficial SuperGrok billing surface.
    async fn fetch_usage(&self, cx: &AppState, account: &AcquiredAccount) -> FetchedUsage {
        let acc = refresh_grok(cx, account.clone()).await;
        let token = acc.credential.access_token.clone();
        let email = acc.credential.email.clone();

        let user_req = UpstreamRequest::get(format!("{CLI_CHAT_PROXY}/user"))
            .header("authorization", &format!("Bearer {token}"))
            .header("accept", "application/json")
            .timeout(REFRESH_TIMEOUT);
        let user_res = match cx.transport().send(user_req).await {
            Ok(res) => res,
            Err(e) => return usage_failure(e.to_string()),
        };
        let mut user_id: Option<String> = None;
        if user_res.status.is_success() {
            let json = match user_res.json_value().await {
                Ok(json) => json,
                Err(e) => return usage_failure(e.to_string()),
            };
            user_id = non_empty(json.get("userId").and_then(Value::as_str))
                .or_else(|| non_empty(json.get("id").and_then(Value::as_str)))
                .map(str::to_string);
        }

        let mut bill_req = UpstreamRequest::get(format!("{CLI_CHAT_PROXY}/billing?format=credits"))
            .header("authorization", &format!("Bearer {token}"))
            .header("accept", "application/json")
            .timeout(REFRESH_TIMEOUT);
        if let Some(user_id) = user_id.as_deref() {
            bill_req = bill_req.header("x-userid", user_id);
        }
        let bill_res = match cx.transport().send(bill_req).await {
            Ok(res) => res,
            Err(e) => return usage_failure(e.to_string()),
        };
        if !bill_res.status.is_success() {
            let mut account = Map::new();
            account.insert("email".into(), email.map(Value::String).unwrap_or(Value::Null));
            return FetchedUsage {
                windows: Vec::new(),
                account,
                stale: true,
                error: Some(format!("billing {}", bill_res.status.as_u16())),
                edge_blocked: false,
            };
        }
        let bill = match bill_res.json_value().await {
            Ok(json) => json,
            Err(e) => return usage_failure(e.to_string()),
        };

        let config = bill.get("config");
        let utilization = config.and_then(|c| c.get("creditUsagePercent")).and_then(Value::as_f64);
        let resets_at = config
            .and_then(|c| c.get("currentPeriod"))
            .and_then(|p| p.get("end"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let plan_type = bill
            .get("subscriptionTier")
            .and_then(Value::as_str)
            .or_else(|| bill.get("subscriptionTiers").and_then(Value::as_str))
            .map(str::to_string);

        let mut account = Map::new();
        account.insert("email".into(), email.map(Value::String).unwrap_or(Value::Null));
        account.insert("plan_type".into(), plan_type.map(Value::String).unwrap_or(Value::Null));
        FetchedUsage {
            windows: vec![UsageWindow { label: "Week".into(), utilization, resets_at, value: None }],
            account,
            stale: false,
            error: None,
            edge_blocked: false,
        }
    }
}

fn usage_failure(error: String) -> FetchedUsage {
    FetchedUsage { windows: Vec::new(), account: Map::new(), stale: true, error: Some(error), edge_blocked: false }
}

fn has_key(body: &Value, key: &str) -> bool {
    body.as_object().is_some_and(|o| o.contains_key(key))
}

fn apply_affinity_headers(mut req: UpstreamRequest, affinity: Option<&AffinityIds>) -> UpstreamRequest {
    let Some(aff) = affinity else { return req };
    if let Some(v) = non_empty(aff.conv_id.as_deref()) {
        req = req.header("x-grok-conv-id", v);
    }
    if let Some(v) = non_empty(aff.session_id.as_deref()) {
        req = req.header("x-grok-session-id", v);
    }
    if let Some(v) = non_empty(aff.turn_idx.as_deref()) {
        req = req.header("x-grok-turn-idx", v);
    }
    req
}

async fn post_responses(
    cx: &AppState,
    access_token: &str,
    body: &Value,
    affinity: Option<&AffinityIds>,
    extras: &CallExtras,
) -> Result<UpstreamResponse, TransportError> {
    let mut req = with_responses_headers(UpstreamRequest::post(format!("{CLI_CHAT_PROXY}/responses")), access_token, affinity)
        .json(body);
    if let Some(timeout) = extras.first_byte_timeout {
        req = req.timeout(timeout);
    }
    cx.transport().send(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{insert_account, insert_user, skip_without_db, test_pool, test_state};
    use crate::providers::grok_encrypted_content::test_fixtures::fake_grok_encrypted_content;
    use crate::upstream::transport::MockTransport;
    use http::Method;
    use http_body_util::BodyExt;
    use sqlx::PgPool;

    const OK_SSE: &str = concat!(
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
    );
    const RECOVERED_SSE: &str = concat!(
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"recovered\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
    );
    const COMPACTION_400: &str = concat!(
        "{\"code\":\"invalid-argument\",\"error\":\"Could not decode the compaction blob. ",
        "Ensure it is unmodified from the compact response.\"}",
    );

    async fn state_with(pool: PgPool, transport: Arc<MockTransport>) -> (AppState, AcquiredAccount) {
        let user = insert_user(&pool, &format!("grok-{}@example.com", crate::ids::new_id(""))).await;
        let credential = StoredCredential { access_token: "tok_test".into(), ..Default::default() };
        let row = insert_account(&pool, &user.id, "grok", &credential).await;
        let cx = test_state(pool, transport);
        (cx, AcquiredAccount { row, credential })
    }

    fn chat_request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "grok/grok-4.5".into(),
            raw_model: "grok/grok-4.5".into(),
            upstream_model: "grok-4.5".into(),
            messages: vec![json!({ "role": "user", "content": "hi" })],
            ..Default::default()
        }
    }

    async fn captured_body(req: ChatCompletionRequest) -> (Value, crate::upstream::transport::RecordedRequest) {
        let Some(pool) = test_pool().await else { panic!("no database") };
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "choices": [] }));
        let (cx, account) = state_with(pool, transport.clone()).await;
        GrokAdapter.chat_completions(&cx, &account, &req, &CallExtras::default()).await.expect("chat completions");
        let recorded = transport.requests().pop().expect("one upstream call");
        (recorded.json(), recorded)
    }

    async fn body_text(res: Response) -> String {
        let bytes = res.into_body().collect().await.expect("body collects").to_bytes();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    async fn body_json(res: Response) -> Value {
        serde_json::from_str(&body_text(res).await).expect("json body")
    }

    // ── chatCompletions — reasoning + sampling ─────────────────────────────

    #[tokio::test]
    async fn always_sends_include_reasoning_true() {
        if test_pool().await.is_none() {
            return skip_without_db();
        }
        let (body, _) = captured_body(chat_request()).await;
        assert_eq!(body["include_reasoning"], json!(true));
    }

    #[tokio::test]
    async fn defaults_temperature_to_one_when_the_client_sent_none() {
        if test_pool().await.is_none() {
            return skip_without_db();
        }
        let (body, _) = captured_body(chat_request()).await;
        assert_eq!(body["temperature"].as_f64(), Some(1.0));
    }

    #[tokio::test]
    async fn forwards_a_client_supplied_temperature() {
        if test_pool().await.is_none() {
            return skip_without_db();
        }
        let (body, _) = captured_body(ChatCompletionRequest { temperature: Some(0.2), ..chat_request() }).await;
        assert_eq!(body["temperature"].as_f64(), Some(0.2));
    }

    #[tokio::test]
    async fn forwards_top_p_only_when_the_client_sent_it() {
        if test_pool().await.is_none() {
            return skip_without_db();
        }
        let (with_top_p, _) = captured_body(ChatCompletionRequest { top_p: Some(0.9), ..chat_request() }).await;
        assert_eq!(with_top_p["top_p"].as_f64(), Some(0.9));
        let (without_top_p, _) = captured_body(chat_request()).await;
        assert!(without_top_p.get("top_p").is_none());
    }

    #[tokio::test]
    async fn temperature_zero_is_forwarded_as_is() {
        if test_pool().await.is_none() {
            return skip_without_db();
        }
        let (body, _) = captured_body(ChatCompletionRequest { temperature: Some(0.0), ..chat_request() }).await;
        assert_eq!(body["temperature"].as_f64(), Some(0.0));
    }

    #[tokio::test]
    async fn openai_surface_hits_api_x_ai_with_grok_shell_identity() {
        if test_pool().await.is_none() {
            return skip_without_db();
        }
        let (_, recorded) = captured_body(chat_request()).await;
        assert_eq!(recorded.method, Method::POST);
        assert_eq!(recorded.url, "https://api.x.ai/v1/chat/completions");
        assert_eq!(recorded.header("x-grok-client-identifier"), Some("grok-shell"));
        assert!(recorded.header("user-agent").expect("user-agent").contains("grok-shell/"));
        assert!(recorded.header("x-grok-req-id").is_some());
        assert_eq!(recorded.header("x-grok-model-override"), Some("grok-4.5"));
    }

    // ── messages — Anthropic → Responses ───────────────────────────────────

    #[tokio::test]
    async fn posts_to_cli_chat_proxy_responses_with_workspace_client_headers() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.respond_sse(StatusCode::OK, OK_SSE);
        let (cx, account) = state_with(pool, transport.clone()).await;

        let mut headers = HeaderMap::new();
        headers.insert("x-kano-api-key-id", "key_1".parse().unwrap());
        headers.insert("x-kano-raw-model", "grok/grok-4.5".parse().unwrap());
        headers.insert("x-grok-conv-id", "conv_1".parse().unwrap());

        let res = GrokAdapter
            .messages(
                &cx,
                &account,
                &json!({
                    "model": "grok-4.5",
                    "max_tokens": 64,
                    "stream": false,
                    "thinking": { "type": "adaptive" },
                    "output_config": { "effort": "high" },
                    "messages": [{ "role": "user", "content": "hi" }],
                }),
                &headers,
                &CallExtras::default(),
            )
            .await
            .expect("messages");
        assert_eq!(res.status(), StatusCode::OK);

        let recorded = transport.requests().pop().expect("one upstream call");
        assert_eq!(recorded.url, "https://cli-chat-proxy.grok.com/v1/responses");
        assert_eq!(recorded.header("user-agent"), Some("xai-grok-workspace/0.2.93"));
        assert_eq!(recorded.header("x-grok-client-version"), Some("0.2.93"));
        assert_eq!(recorded.header("x-xai-token-auth"), Some("xai-grok-cli"));
        assert_eq!(recorded.header("x-grok-conv-id"), Some("conv_1"));
        let body = recorded.json();
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(body["reasoning"], json!({ "effort": "high" }));

        let json = body_json(res).await;
        let content = json["content"].as_array().expect("content array");
        assert!(content.iter().any(|b| b["type"] == "text"));
    }

    #[tokio::test]
    async fn omits_include_when_thinking_is_disabled() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.respond_sse(
            StatusCode::OK,
            concat!(
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
            ),
        );
        let (cx, account) = state_with(pool, transport.clone()).await;
        let mut headers = HeaderMap::new();
        headers.insert("x-kano-raw-model", "grok/grok-4.5".parse().unwrap());

        GrokAdapter
            .messages(
                &cx,
                &account,
                &json!({
                    "model": "grok-4.5",
                    "stream": false,
                    "thinking": { "type": "disabled" },
                    "messages": [{ "role": "user", "content": "hi" }],
                }),
                &headers,
                &CallExtras::default(),
            )
            .await
            .expect("messages");

        let body = transport.requests().pop().expect("one upstream call").json();
        assert!(body.get("include").is_none());
        assert!(body.get("reasoning").is_none());
    }

    // ── messages — opaque decode recovery (grok_reasoning_recovery.test.ts) ─

    fn recovery_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-kano-api-key-id", "key_1".parse().unwrap());
        headers.insert("x-kano-raw-model", "grok/grok-4.5".parse().unwrap());
        headers.insert("x-grok-conv-id", "conv_bad".parse().unwrap());
        headers
    }

    fn recovery_body(thinking_text: &str, signature: &str) -> Value {
        json!({
            "model": "grok-4.5",
            "stream": false,
            "thinking": { "type": "adaptive" },
            "messages": [
                { "role": "user", "content": "hi" },
                {
                    "role": "assistant",
                    "content": [
                        { "type": "thinking", "thinking": thinking_text, "signature": signature },
                        { "type": "text", "text": "hello" },
                    ],
                },
                { "role": "user", "content": "continue" },
            ],
        })
    }

    #[tokio::test]
    async fn strips_encrypted_content_and_retries_after_a_compaction_blob_400() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.expect(|_| Ok(UpstreamResponse::json(StatusCode::BAD_REQUEST, &serde_json::from_str(COMPACTION_400).unwrap())));
        transport.respond_sse(StatusCode::OK, RECOVERED_SSE);
        let (cx, account) = state_with(pool, transport.clone()).await;

        let res = GrokAdapter
            .messages(
                &cx,
                &account,
                &recovery_body("plan", &fake_grok_encrypted_content(7)),
                &recovery_headers(),
                &CallExtras::default(),
            )
            .await
            .expect("messages");
        assert_eq!(res.status(), StatusCode::OK);

        let calls = transport.requests();
        assert_eq!(calls.len(), 2);
        let first_input = calls[0].json()["input"].as_array().cloned().expect("input array");
        assert!(first_input
            .iter()
            .any(|i| i["type"] == "reasoning" && i.get("encrypted_content").and_then(Value::as_str).is_some()));
        let second_input = calls[1].json()["input"].as_array().cloned().expect("input array");
        assert!(!second_input
            .iter()
            .any(|i| i["type"] == "reasoning" && i.get("encrypted_content").and_then(Value::as_str).is_some()));

        let json = body_json(res).await;
        let content = json["content"].as_array().expect("content array");
        assert!(content.iter().any(|b| b["text"] == "recovered"));
    }

    #[tokio::test]
    async fn returns_the_original_400_when_recovery_retries_also_fail() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        for _ in 0..3 {
            transport
                .expect(|_| Ok(UpstreamResponse::json(StatusCode::BAD_REQUEST, &serde_json::from_str(COMPACTION_400).unwrap())));
        }
        let (cx, account) = state_with(pool, transport.clone()).await;

        // Empty thinking text: the reasoning item carries only ciphertext, so the strip
        // retry drops it entirely and the affinity-reset retry still runs.
        let res = GrokAdapter
            .messages(
                &cx,
                &account,
                &recovery_body("", &fake_grok_encrypted_content(11)),
                &recovery_headers(),
                &CallExtras::default(),
            )
            .await
            .expect("messages");

        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(body_text(res).await.contains("Could not decode the compaction blob"));
        // original + strip retry + affinity-reset retry
        assert_eq!(transport.requests().len(), 3);
    }

    // ── fetchUsage — window mapping and the utilization scale contract ──────

    async fn fetch_usage_with(bill: Value, bill_status: StatusCode) -> FetchedUsage {
        let Some(pool) = test_pool().await else { panic!("no database") };
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "userId": "u_1" }));
        transport.expect(move |_| Ok(UpstreamResponse::json(bill_status, &bill)));
        let (cx, account) = state_with(pool, transport).await;
        GrokAdapter.fetch_usage(&cx, &account).await
    }

    #[tokio::test]
    async fn regression_credit_usage_percent_one_stays_one() {
        if test_pool().await.is_none() {
            return skip_without_db();
        }
        let usage =
            fetch_usage_with(json!({ "config": { "creditUsagePercent": 1, "currentPeriod": { "end": "2026-08-10T00:00:00Z" } } }), StatusCode::OK)
                .await;
        assert_eq!(
            usage.windows,
            vec![UsageWindow {
                label: "Week".into(),
                utilization: Some(1.0),
                resets_at: Some("2026-08-10T00:00:00Z".into()),
                value: None
            }]
        );
    }

    #[tokio::test]
    async fn a_mid_range_percent_passes_through_unchanged() {
        if test_pool().await.is_none() {
            return skip_without_db();
        }
        let usage = fetch_usage_with(json!({ "config": { "creditUsagePercent": 73 } }), StatusCode::OK).await;
        assert_eq!(usage.windows[0].utilization, Some(73.0));
    }

    #[tokio::test]
    async fn one_hundred_passes_through_unchanged() {
        if test_pool().await.is_none() {
            return skip_without_db();
        }
        let usage = fetch_usage_with(json!({ "config": { "creditUsagePercent": 100 } }), StatusCode::OK).await;
        assert_eq!(usage.windows[0].utilization, Some(100.0));
    }

    #[tokio::test]
    async fn a_missing_percent_maps_to_none_not_zero() {
        if test_pool().await.is_none() {
            return skip_without_db();
        }
        let usage = fetch_usage_with(json!({ "config": {} }), StatusCode::OK).await;
        assert_eq!(
            usage.windows,
            vec![UsageWindow { label: "Week".into(), utilization: None, resets_at: None, value: None }]
        );
    }

    #[tokio::test]
    async fn resets_at_is_none_when_the_period_has_no_end() {
        if test_pool().await.is_none() {
            return skip_without_db();
        }
        let usage = fetch_usage_with(json!({ "config": { "creditUsagePercent": 5 } }), StatusCode::OK).await;
        assert_eq!(usage.windows[0].resets_at, None);
    }

    #[tokio::test]
    async fn account_meta_carries_credential_email_and_subscription_tier() {
        if test_pool().await.is_none() {
            return skip_without_db();
        }
        let usage =
            fetch_usage_with(json!({ "config": { "creditUsagePercent": 5 }, "subscriptionTier": "SuperGrok" }), StatusCode::OK).await;
        // The shared account fixture's credential carries no email.
        assert_eq!(Value::Object(usage.account), json!({ "email": null, "plan_type": "SuperGrok" }));
    }

    #[tokio::test]
    async fn a_non_ok_billing_response_is_stale_with_the_documented_error() {
        if test_pool().await.is_none() {
            return skip_without_db();
        }
        let usage = fetch_usage_with(json!({}), StatusCode::SERVICE_UNAVAILABLE).await;
        assert!(usage.stale);
        assert_eq!(usage.error.as_deref(), Some("billing 503"));
        assert!(usage.windows.is_empty());
    }

    #[tokio::test]
    async fn a_transport_failure_is_caught_as_stale_with_the_error_message() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.expect(|_| Err(TransportError::Other("network down".into())));
        let (cx, account) = state_with(pool, transport).await;
        let usage = GrokAdapter.fetch_usage(&cx, &account).await;
        assert!(usage.stale);
        assert!(usage.windows.is_empty());
        assert!(usage.account.is_empty());
        assert!(usage.error.expect("error").contains("network down"));
    }

    // ── listModels ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_models_maps_ids_and_reports_a_non_ok_status() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.respond_json(
            StatusCode::OK,
            json!({ "data": [{ "id": "grok-4.5", "name": "Grok 4.5" }, { "id": "", "name": "skip" }, { "name": "no id" }] }),
        );
        let (cx, account) = state_with(pool, transport).await;
        let listed = GrokAdapter.list_models(&cx, &account).await;
        assert_eq!(listed.error, None);
        assert_eq!(
            listed.models,
            vec![UpstreamModel { id: "grok-4.5".into(), display_name: Some("Grok 4.5".into()) }]
        );

        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::FORBIDDEN, json!({}));
        let (cx, account) = state_with(pool, transport).await;
        let listed = GrokAdapter.list_models(&cx, &account).await;
        assert_eq!(listed.error.as_deref(), Some("models 403"));
        assert!(listed.models.is_empty());
    }
}
