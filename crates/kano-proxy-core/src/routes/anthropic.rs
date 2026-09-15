//! Port of apps/api/src/routes/anthropic.ts — the Anthropic-shaped surface (docs/api.md
//! § Anthropic surface): `POST /anthropic/v1/messages`,
//! `POST /anthropic/v1/messages/count_tokens` and `GET /anthropic/v1/models`.
//!
//! The POST handlers are shared with the group mounts (`/g/{slug}/anthropic/v1/…`): resolution
//! branches on the mount's `slug`, everything past resolution is identical.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use bytes::Bytes;
use serde_json::{json, Value};

use crate::app::now_ms;
use crate::catalog::models::{list_models_for_user, ListModelsOptions};
use crate::extensions::ApiKeyIdentity;
use crate::http::errors::ApiError;
use crate::logging::request_log::LogEntry;
use crate::providers::codex_count::count_anthropic_tokens;
use crate::providers::ProviderId;
use crate::proxy::dispatch::{
    canonical_model_id, dispatch_anthropic_messages, AnthropicDispatchOptions, AnthropicEndpoint,
};
use crate::proxy::dispatch_anthropic_via_openai::{dispatch_anthropic_via_openai, ViaOpenAiDispatchOptions};
use crate::proxy::request_json::read_proxy_json;
use crate::utils::loop_guard::{detect_anthropic_tool_loop, loop_detected_message};
use crate::utils::model::logging_provider_from_raw_model;
use crate::AppState;

use super::openai::{affinity_from, log_model, model_string, no_retry, spawn_log};
use super::resolve_request::{resolve_request_model, RequestModelResolution};

/// Builtins that expose `adapter.messages` but **convert** to a non-Anthropic upstream format
/// rather than passing the body through: grok → xAI Responses, antigravity → Gemini
/// `GenerateContent`. Everything else with a `messages()` (claude-code, custom
/// anthropic-format) is a true native passthrough, which is what decides `cache_control`
/// handling and whether the tool-loop guard runs (docs/api.md § Degenerate tool-call loop
/// guard).
const CONVERTING_MESSAGES_PROVIDERS: [&str; 2] = ["grok", "antigravity"];

pub fn is_native_anthropic_passthrough(provider: &str, has_messages: bool) -> bool {
    has_messages && !CONVERTING_MESSAGES_PROVIDERS.contains(&provider)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/models", get(models))
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
}

// ---------------------------------------------------------------------------------------
// GET /anthropic/v1/models
// ---------------------------------------------------------------------------------------

async fn models(State(state): State<AppState>, Extension(id): Extension<ApiKeyIdentity>) -> Response {
    let listed = list_models_for_user(&state, &id.user_id, ListModelsOptions { available_only: true, force: false }).await;
    // Same catalog as the OpenAI surface; ids are always provider/upstream.
    let data: Vec<Value> = listed
        .models
        .iter()
        .map(|m| json!({ "id": m.id, "display_name": m.display_name, "type": "model" }))
        .collect();
    Json(json!({ "data": data })).into_response()
}

// ---------------------------------------------------------------------------------------
// Shared pieces
// ---------------------------------------------------------------------------------------

/// Anthropic-shaped answers for the two non-ok resolution outcomes, logged with the outcome's
/// own status/error code. Shared by messages and count_tokens (docs/api.md § Group endpoints).
fn resolution_failure(
    cx: &AppState,
    id: &ApiKeyIdentity,
    outcome: &RequestModelResolution,
    model_raw: &str,
    started: i64,
) -> Response {
    let group_not_found = matches!(outcome, RequestModelResolution::GroupNotFound { .. });
    spawn_log(
        cx,
        LogEntry {
            user_id: id.user_id.clone(),
            api_key_id: Some(id.api_key_id.clone()),
            provider: logging_provider_from_raw_model(model_raw),
            model: log_model(model_raw),
            status_code: if group_not_found { 404 } else { 400 },
            latency_ms: now_ms() - started,
            error_code: Some(if group_not_found { "group_not_found".into() } else { "invalid_model".into() }),
            ..Default::default()
        },
    );
    match outcome {
        RequestModelResolution::GroupNotFound { slug } => no_retry(ApiError::anthropic(
            StatusCode::NOT_FOUND,
            "not_found_error",
            &format!("unknown group endpoint \"{slug}\""),
        )),
        RequestModelResolution::InvalidModel { group_slug } => {
            let message = match group_slug {
                Some(slug) => format!(
                    "model must be one of this group endpoint's configured models (see GET /g/{slug}/anthropic/v1/models)"
                ),
                None => "model must be provider/model (e.g. claude-code/claude-opus-5, grok/grok-4.5)".to_string(),
            };
            no_retry(ApiError::anthropic(StatusCode::BAD_REQUEST, "invalid_request_error", &message))
        }
        RequestModelResolution::Ok(_) => unreachable!("resolution succeeded"),
    }
}

fn invalid_json() -> Response {
    ApiError::anthropic(StatusCode::BAD_REQUEST, "invalid_request_error", "Invalid JSON").into_response()
}

/// `anthropic-beta` / `anthropic-version` are forwarded when the client sent them; nothing else
/// of the client's header set reaches the adapter.
fn passthrough_headers(headers: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for name in ["anthropic-beta", "anthropic-version"] {
        if let Some(value) = headers.get(name) {
            out.insert(HeaderName::from_static(name), value.clone());
        }
    }
    out
}

fn set_header(headers: &mut HeaderMap, name: &'static str, value: &str) {
    if let Ok(value) = HeaderValue::from_str(value) {
        headers.insert(HeaderName::from_static(name), value);
    }
}

// ---------------------------------------------------------------------------------------
// POST /anthropic/v1/messages
// ---------------------------------------------------------------------------------------

async fn messages(
    State(state): State<AppState>,
    Extension(id): Extension<ApiKeyIdentity>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_anthropic_messages(&state, &id, None, &headers, body).await
}

pub async fn handle_anthropic_messages(
    cx: &AppState,
    id: &ApiKeyIdentity,
    slug: Option<&str>,
    headers: &HeaderMap,
    raw_body: Bytes,
) -> Response {
    let started = now_ms();
    let Ok(body) = read_proxy_json(&raw_body) else { return invalid_json() };
    let model_raw = model_string(&body);
    let resolved = match resolve_request_model(cx, &id.user_id, slug, &model_raw).await {
        Ok(RequestModelResolution::Ok(resolution)) => resolution,
        Ok(other) => return resolution_failure(cx, id, &other, &model_raw, started),
        Err(err) => return ApiError::from(err).into_response(),
    };

    // Grok sticky headers: forwarded if the client supplied them; never invented.
    let affinity = affinity_from(headers);
    let target_model = canonical_model_id(&resolved.primary.provider, &resolved.primary.upstream_model);

    // claude-code / custom anthropic-format: native Messages passthrough. grok and antigravity
    // also expose `messages`, but those paths convert to another wire format, so the loop guard
    // still applies below. Decided from the highest-priority resolved target only.
    if is_native_anthropic_passthrough(&resolved.primary.provider, resolved.primary.adapter.has_messages()) {
        // Only the model is normalized to the bare upstream id — strict cache_control
        // passthrough, no other body field is touched (dispatch does the model rewrite).
        return dispatch_anthropic_messages(
            cx,
            AnthropicDispatchOptions {
                user_id: id.user_id.clone(),
                api_key_id: Some(id.api_key_id.clone()),
                body,
                headers: passthrough_headers(headers),
                model: target_model,
                provider: Some(resolved.primary.provider.clone()),
                adapter: Some(resolved.primary.adapter.clone()),
                group_name: resolved.group_name.clone(),
                candidates: Some(resolved.candidates.clone()),
                strategy: Some(resolved.strategy.clone()),
                is_builtin: Some(resolved.primary.is_builtin),
                custom_provider: resolved.primary.custom_provider.clone(),
                ..Default::default()
            },
        )
        .await;
    }

    // Conversion ingress (grok Responses / codex / custom-openai). The loop guard applies here
    // — never on the native passthrough branch above.
    let messages: Vec<Value> = body.get("messages").and_then(Value::as_array).cloned().unwrap_or_default();
    let loop_detection = detect_anthropic_tool_loop(&messages);
    if loop_detection.tripped {
        spawn_log(
            cx,
            LogEntry {
                user_id: id.user_id.clone(),
                api_key_id: Some(id.api_key_id.clone()),
                provider: resolved.primary.provider.clone(),
                model: target_model,
                status_code: 400,
                latency_ms: now_ms() - started,
                error_code: Some("loop_detected".into()),
                group_name: resolved.group_name.clone(),
                ..Default::default()
            },
        );
        return no_retry(ApiError::anthropic(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            &loop_detected_message(&loop_detection),
        ));
    }

    // grok (Anthropic ↔ xAI Responses, encrypted reasoning) and antigravity (Anthropic ↔
    // Gemini) both convert inside their own `messages()`, so they dispatch through the Messages
    // path rather than the Chat Completions one.
    if CONVERTING_MESSAGES_PROVIDERS.contains(&resolved.primary.provider.as_str())
        && resolved.primary.adapter.has_messages()
    {
        let mut upstream_headers = passthrough_headers(headers);
        // Client-supplied isolation headers are already stripped: only the route sets them.
        if let Some(value) = &affinity.conv_id {
            set_header(&mut upstream_headers, "x-grok-conv-id", value);
        }
        if let Some(value) = &affinity.session_id {
            set_header(&mut upstream_headers, "x-grok-session-id", value);
        }
        if let Some(value) = &affinity.turn_idx {
            set_header(&mut upstream_headers, "x-grok-turn-idx", value);
        }
        set_header(&mut upstream_headers, "x-kano-api-key-id", &id.api_key_id);
        set_header(&mut upstream_headers, "x-kano-raw-model", &resolved.raw);

        return dispatch_anthropic_messages(
            cx,
            AnthropicDispatchOptions {
                user_id: id.user_id.clone(),
                api_key_id: Some(id.api_key_id.clone()),
                body,
                headers: upstream_headers,
                model: target_model,
                provider: Some(resolved.primary.provider.clone()),
                adapter: Some(resolved.primary.adapter.clone()),
                group_name: resolved.group_name.clone(),
                candidates: Some(resolved.candidates.clone()),
                strategy: Some(resolved.strategy.clone()),
                is_builtin: Some(resolved.primary.is_builtin),
                custom_provider: resolved.primary.custom_provider.clone(),
                ..Default::default()
            },
        )
        .await;
    }

    dispatch_anthropic_via_openai(
        cx,
        ViaOpenAiDispatchOptions {
            user_id: id.user_id.clone(),
            api_key_id: Some(id.api_key_id.clone()),
            provider: resolved.primary.provider.clone(),
            adapter: Some(resolved.primary.adapter.clone()),
            raw_model: resolved.raw.clone(),
            upstream_model: resolved.primary.upstream_model.clone(),
            body,
            affinity: Some(affinity),
            group_name: resolved.group_name.clone(),
            candidates: Some(resolved.candidates.clone()),
            strategy: Some(resolved.strategy.clone()),
            is_builtin: Some(resolved.primary.is_builtin),
            custom_provider: resolved.primary.custom_provider.clone(),
            ..Default::default()
        },
    )
    .await
}

// ---------------------------------------------------------------------------------------
// POST /anthropic/v1/messages/count_tokens
// ---------------------------------------------------------------------------------------

/// How count_tokens answers when the provider has no upstream counting endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalCountMode {
    /// The in-process `o200k_base` tokenizer (codex). The Worker reached it through the egress
    /// relay; this server owns it (docs/rust-server.md, providers/codex_count.rs).
    Local,
    /// The sentinel `{"input_tokens": 0}` (grok, url-less custom-openai).
    Stub,
}

/// `countTokensLocalMode(provider)`: `Local` = tokenizer count, `Stub` = sentinel zero, `None`
/// = a real upstream count exists and dispatch handles it (claude-code forwards natively,
/// antigravity calls `v1internal:countTokens`). A pure function, unit-tested below.
pub fn count_tokens_local_mode(provider: ProviderId) -> Option<LocalCountMode> {
    match provider {
        ProviderId::Codex => Some(LocalCountMode::Local),
        ProviderId::Grok => Some(LocalCountMode::Stub),
        _ => None,
    }
}

async fn count_tokens(
    State(state): State<AppState>,
    Extension(id): Extension<ApiKeyIdentity>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_anthropic_count_tokens(&state, &id, None, &headers, body).await
}

pub async fn handle_anthropic_count_tokens(
    cx: &AppState,
    id: &ApiKeyIdentity,
    slug: Option<&str>,
    headers: &HeaderMap,
    raw_body: Bytes,
) -> Response {
    let started = now_ms();
    let Ok(body) = read_proxy_json(&raw_body) else { return invalid_json() };
    let model_raw = model_string(&body);
    let resolved = match resolve_request_model(cx, &id.user_id, slug, &model_raw).await {
        Ok(RequestModelResolution::Ok(resolution)) => resolution,
        Ok(other) => return resolution_failure(cx, id, &other, &model_raw, started),
        Err(err) => return ApiError::from(err).into_response(),
    };

    // Providers with no upstream counting endpoint are answered locally — codex through the
    // tokenizer, grok and url-less custom-openai with the sentinel zero. Never a 400: a failed
    // count_tokens sends Claude Code into a parallel max_tokens:1 probe burst against the real
    // upstream (docs/api.md § count_tokens, measured 2026-08-22).
    let local_mode = if resolved.primary.is_builtin {
        ProviderId::parse(&resolved.primary.provider).and_then(count_tokens_local_mode)
    } else if resolved.primary.adapter.has_count_tokens() {
        None
    } else {
        Some(LocalCountMode::Stub)
    };
    if let Some(mode) = local_mode {
        let tokens = match mode {
            LocalCountMode::Local => count_anthropic_tokens(&Value::Object(body.clone())),
            LocalCountMode::Stub => None,
        };
        spawn_log(
            cx,
            LogEntry {
                user_id: id.user_id.clone(),
                api_key_id: Some(id.api_key_id.clone()),
                provider: resolved.primary.provider.clone(),
                model: canonical_model_id(&resolved.primary.provider, &resolved.primary.upstream_model),
                status_code: 200,
                latency_ms: now_ms() - started,
                error_code: if tokens.is_none() { Some("count_tokens_stub".into()) } else { None },
                group_name: resolved.group_name.clone(),
                ..Default::default()
            },
        );
        return Json(json!({ "input_tokens": tokens.unwrap_or(0) })).into_response();
    }

    // Native passthrough only, same as /v1/messages.
    dispatch_anthropic_messages(
        cx,
        AnthropicDispatchOptions {
            user_id: id.user_id.clone(),
            api_key_id: Some(id.api_key_id.clone()),
            body,
            headers: passthrough_headers(headers),
            model: canonical_model_id(&resolved.primary.provider, &resolved.primary.upstream_model),
            provider: Some(resolved.primary.provider.clone()),
            adapter: Some(resolved.primary.adapter.clone()),
            endpoint: AnthropicEndpoint::CountTokens,
            group_name: resolved.group_name.clone(),
            candidates: Some(resolved.candidates.clone()),
            strategy: Some(resolved.strategy.clone()),
            is_builtin: Some(resolved.primary.is_builtin),
            custom_provider: resolved.primary.custom_provider.clone(),
            ..Default::default()
        },
    )
    .await
}

// ---------------------------------------------------------------------------------------
// Group mounts (`/g/{slug}/anthropic/v1/…`)
// ---------------------------------------------------------------------------------------

pub(crate) async fn group_messages(
    State(state): State<AppState>,
    Extension(id): Extension<ApiKeyIdentity>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_anthropic_messages(&state, &id, Some(&slug), &headers, body).await
}

pub(crate) async fn group_count_tokens(
    State(state): State<AppState>,
    Extension(id): Extension<ApiKeyIdentity>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_anthropic_count_tokens(&state, &id, Some(&slug), &headers, body).await
}

#[cfg(test)]
mod tests {
    use super::super::resolve_request::test_support::{body_json, drain_sse, fixture, sse_events, Fixture};
    use super::*;
    use crate::db::test_support::skip_without_db;
    use axum::http::StatusCode;
    use serde_json::json;
    use tower::ServiceExt;

    const BASE_URL: &str = "https://upstream.example.com/v1";

    async fn custom_gateway(f: &Fixture) {
        f.custom_provider("mygw", "openai", BASE_URL).await;
    }

    fn user_message() -> serde_json::Value {
        json!([{ "role": "user", "content": "hi" }])
    }

    fn anthropic_ok() -> serde_json::Value {
        json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "text", "text": "hello" }],
            "usage": { "input_tokens": 9, "output_tokens": 2 }
        })
    }

    // ---- countTokensLocalMode (apps/api/tests/anthropic.test.ts) ----

    #[test]
    fn count_tokens_passes_claude_code_and_antigravity_through_to_their_real_upstream_counts() {
        assert_eq!(count_tokens_local_mode(ProviderId::ClaudeCode), None);
        assert_eq!(count_tokens_local_mode(ProviderId::Antigravity), None);
    }

    #[test]
    fn count_tokens_routes_codex_to_the_local_tokenizer() {
        assert_eq!(count_tokens_local_mode(ProviderId::Codex), Some(LocalCountMode::Local));
    }

    #[test]
    fn count_tokens_answers_grok_with_the_sentinel_stub_never_a_400() {
        assert_eq!(count_tokens_local_mode(ProviderId::Grok), Some(LocalCountMode::Stub));
    }

    #[test]
    fn only_a_true_anthropic_passthrough_skips_the_loop_guard() {
        assert!(is_native_anthropic_passthrough("claude-code", true));
        assert!(is_native_anthropic_passthrough("mygw", true));
        // grok and antigravity expose `messages()` but convert to another wire.
        assert!(!is_native_anthropic_passthrough("grok", true));
        assert!(!is_native_anthropic_passthrough("antigravity", true));
        assert!(!is_native_anthropic_passthrough("codex", false));
    }

    // ---- routes ----

    #[tokio::test]
    async fn models_answers_the_anthropic_envelope_for_the_same_catalog() {
        let Some(f) = fixture().await else { return skip_without_db() };

        let response = f.router().oneshot(f.get("/anthropic/v1/models")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await, json!({ "data": [] }));

        custom_gateway(&f).await;
        let response = f.router().oneshot(f.get("/anthropic/v1/models")).await.unwrap();
        let json = body_json(response).await;
        assert_eq!(json["data"][0], json!({ "id": "mygw/local-model", "display_name": "local-model", "type": "model" }));
    }

    #[tokio::test]
    async fn messages_passes_a_native_body_through_untouched_but_for_the_model_id() {
        let Some(f) = fixture().await else { return skip_without_db() };
        f.account("claude-code").await;
        f.mock.respond_json(StatusCode::OK, anthropic_ok());

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/anthropic/v1/messages")
            .header("authorization", format!("Bearer {}", f.key))
            .header("content-type", "application/json")
            .header("anthropic-beta", "client-beta-1")
            .header("anthropic-version", "2023-06-01")
            .body(axum::body::Body::from(
                json!({
                    "model": "claude-code/claude-opus-5",
                    "max_tokens": 16,
                    "system": [{ "type": "text", "text": "keep it short", "cache_control": { "type": "ephemeral" } }],
                    "messages": user_message()
                })
                .to_string(),
            ))
            .unwrap();
        let response = f.router().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await["content"][0]["text"], "hello");

        let calls = f.mock.requests();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].url.contains("/v1/messages"));
        let sent = calls[0].json();
        assert_eq!(sent["model"], "claude-opus-5");
        // cache_control is never rewritten on the passthrough path.
        let system = sent["system"].as_array().expect("system blocks");
        assert!(system.iter().any(|b| b["cache_control"]["type"] == "ephemeral"));
        // The client's own beta list rides along.
        assert!(calls[0].header("anthropic-beta").unwrap().contains("client-beta-1"));

        let rows = f.logs(1).await;
        assert_eq!(rows[0]["provider"], "claude-code");
        assert_eq!(rows[0]["model"], "claude-code/claude-opus-5");
    }

    #[tokio::test]
    async fn messages_converts_for_an_openai_format_provider() {
        let Some(f) = fixture().await else { return skip_without_db() };
        custom_gateway(&f).await;
        f.mock.respond_json(
            StatusCode::OK,
            json!({
                "id": "chatcmpl_1",
                "object": "chat.completion",
                "choices": [{ "index": 0, "message": { "role": "assistant", "content": "hello" }, "finish_reason": "stop" }],
                "usage": { "prompt_tokens": 4, "completion_tokens": 1 }
            }),
        );

        let response = f
            .router()
            .oneshot(f.post(
                "/anthropic/v1/messages",
                json!({ "model": "mygw/local-model", "max_tokens": 16, "messages": user_message() }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["type"], "message");
        assert_eq!(json["content"][0]["text"], "hello");
        assert_eq!(json["usage"]["input_tokens"], 4);

        let calls = f.mock.requests();
        assert_eq!(calls[0].url, format!("{BASE_URL}/chat/completions"));
        assert_eq!(calls[0].json()["model"], "local-model");
    }

    #[tokio::test]
    async fn a_streaming_conversion_answers_the_anthropic_event_sequence() {
        let Some(f) = fixture().await else { return skip_without_db() };
        custom_gateway(&f).await;
        f.mock.respond_sse(
            StatusCode::OK,
            "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n\
             data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1}}\n\n\
             data: [DONE]\n\n",
        );

        let response = f
            .router()
            .oneshot(f.post(
                "/anthropic/v1/messages",
                json!({ "model": "mygw/local-model", "max_tokens": 16, "stream": true, "messages": user_message() }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let text = drain_sse(response).await;
        let events = sse_events(&text);
        assert_eq!(events[0]["type"], "message_start");
        assert!(events.iter().any(|e| e["type"] == "content_block_delta" && e["delta"]["text"] == "hi"));
        assert_eq!(events.last().unwrap()["type"], "message_stop");
    }

    #[tokio::test]
    async fn the_loop_guard_runs_on_conversion_ingress_only() {
        let Some(f) = fixture().await else { return skip_without_db() };
        custom_gateway(&f).await;
        f.account("claude-code").await;

        let run: Vec<serde_json::Value> = (0..8)
            .flat_map(|i| {
                vec![
                    json!({ "role": "assistant", "content": [{ "type": "tool_use", "id": format!("toolu_{i}"), "name": "Read", "input": { "p": "/a" } }] }),
                    json!({ "role": "user", "content": [{ "type": "tool_result", "tool_use_id": format!("toolu_{i}"), "content": "r" }] }),
                ]
            })
            .collect();

        let response = f
            .router()
            .oneshot(f.post(
                "/anthropic/v1/messages",
                json!({ "model": "mygw/local-model", "max_tokens": 16, "messages": run }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()["x-should-retry"], "false");
        let json = body_json(response).await;
        assert_eq!(json["type"], "error");
        assert_eq!(json["error"]["type"], "invalid_request_error");
        assert!(json["error"]["message"].as_str().unwrap().contains("Read"));
        assert_eq!(f.mock.requests().len(), 0);
        let rows = f.logs(1).await;
        assert_eq!(rows[0]["error_code"], "loop_detected");

        // The identical history on the native passthrough path is the client's own business.
        f.mock.respond_json(StatusCode::OK, anthropic_ok());
        let response = f
            .router()
            .oneshot(f.post(
                "/anthropic/v1/messages",
                json!({ "model": "claude-code/claude-opus-5", "max_tokens": 16, "messages": run }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn resolution_failures_answer_in_the_anthropic_envelope() {
        let Some(f) = fixture().await else { return skip_without_db() };
        custom_gateway(&f).await;
        f.group("team", "fast", &["mygw/local-model"]).await;

        let response = f
            .router()
            .oneshot(f.post("/anthropic/v1/messages", json!({ "model": "bare", "messages": [] })))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()["x-should-retry"], "false");
        assert_eq!(
            body_json(response).await,
            json!({ "type": "error", "error": { "type": "invalid_request_error", "message": "model must be provider/model (e.g. claude-code/claude-opus-5, grok/grok-4.5)" } })
        );

        let response = f
            .router()
            .oneshot(f.post("/g/nope/anthropic/v1/messages", json!({ "model": "fast", "messages": [] })))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            body_json(response).await,
            json!({ "type": "error", "error": { "type": "not_found_error", "message": "unknown group endpoint \"nope\"" } })
        );

        let response = f
            .router()
            .oneshot(f.post("/g/team/anthropic/v1/messages", json!({ "model": "nope", "messages": [] })))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            body_json(response).await["error"]["message"],
            "model must be one of this group endpoint's configured models (see GET /g/team/anthropic/v1/models)"
        );

        let bad_json = axum::http::Request::builder()
            .method("POST")
            .uri("/anthropic/v1/messages")
            .header("authorization", format!("Bearer {}", f.key))
            .header("content-type", "application/json")
            .body(axum::body::Body::from("["))
            .unwrap();
        let response = f.router().oneshot(bad_json).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let json = body_json(response).await;
        assert_eq!(json["type"], "error");
        assert_eq!(json["error"]["message"], "Invalid JSON");
    }

    #[tokio::test]
    async fn a_group_anthropic_catalog_lists_its_own_names() {
        let Some(f) = fixture().await else { return skip_without_db() };
        custom_gateway(&f).await;
        f.group("team", "fast", &["mygw/local-model"]).await;

        let response = f.router().oneshot(f.get("/g/team/anthropic/v1/models")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            body_json(response).await,
            json!({ "data": [{ "id": "fast", "display_name": "fast", "type": "model" }] })
        );

        let response = f.router().oneshot(f.get("/g/nope/anthropic/v1/models")).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_json(response).await["error"]["type"], "not_found_error");
    }

    // ---- count_tokens (docs/api.md § count_tokens) ----

    fn count_body(model: &str) -> serde_json::Value {
        json!({ "model": model, "system": "you are a helpful assistant", "messages": [{ "role": "user", "content": "count these tokens please" }] })
    }

    #[tokio::test]
    async fn count_tokens_answers_grok_with_the_sentinel_zero_and_logs_the_stub() {
        let Some(f) = fixture().await else { return skip_without_db() };
        f.account("grok").await;

        let response = f
            .router()
            .oneshot(f.post("/anthropic/v1/messages/count_tokens", count_body("grok/grok-4.5")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await, json!({ "input_tokens": 0 }));
        assert_eq!(f.mock.requests().len(), 0, "the sentinel never calls upstream");

        let rows = f.logs(1).await;
        assert_eq!(rows[0]["status_code"], 200);
        assert_eq!(rows[0]["error_code"], "count_tokens_stub");
        assert_eq!(rows[0]["model"], "grok/grok-4.5");
    }

    #[tokio::test]
    async fn count_tokens_counts_codex_locally_without_touching_the_backend() {
        let Some(f) = fixture().await else { return skip_without_db() };
        f.account("codex").await;

        let response = f
            .router()
            .oneshot(f.post("/anthropic/v1/messages/count_tokens", count_body("codex/gpt-5.4")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let tokens = body_json(response).await["input_tokens"].as_u64().expect("a count");
        assert!(tokens > 0, "a real tokenizer count, not the sentinel");
        assert_eq!(f.mock.requests().len(), 0, "no ChatGPT backend call, no account acquired");

        let rows = f.logs(1).await;
        assert_eq!(rows[0]["status_code"], 200);
        assert_eq!(rows[0]["error_code"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn count_tokens_stubs_a_custom_openai_provider_with_no_count_url() {
        let Some(f) = fixture().await else { return skip_without_db() };
        custom_gateway(&f).await;

        let response = f
            .router()
            .oneshot(f.post("/anthropic/v1/messages/count_tokens", count_body("mygw/local-model")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await, json!({ "input_tokens": 0 }));
        assert_eq!(f.mock.requests().len(), 0);
        let rows = f.logs(1).await;
        assert_eq!(rows[0]["error_code"], "count_tokens_stub");
    }

    #[tokio::test]
    async fn count_tokens_forwards_natively_where_a_real_upstream_count_exists() {
        let Some(f) = fixture().await else { return skip_without_db() };
        f.account("claude-code").await;
        f.mock.respond_json(StatusCode::OK, json!({ "input_tokens": 42 }));

        let response = f
            .router()
            .oneshot(f.post("/anthropic/v1/messages/count_tokens", count_body("claude-code/claude-opus-5")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await, json!({ "input_tokens": 42 }));
        let calls = f.mock.requests();
        assert!(calls[0].url.ends_with("/v1/messages/count_tokens"), "{}", calls[0].url);
        assert_eq!(calls[0].json()["model"], "claude-opus-5");
    }

    #[tokio::test]
    async fn count_tokens_on_a_group_mount_follows_the_expanded_target() {
        let Some(f) = fixture().await else { return skip_without_db() };
        f.account("grok").await;
        f.group("team", "fast", &["grok/grok-4.5"]).await;

        let response = f
            .router()
            .oneshot(f.post("/g/team/anthropic/v1/messages/count_tokens", count_body("fast")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await, json!({ "input_tokens": 0 }));
        let rows = f.logs(1).await;
        assert_eq!(rows[0]["model"], "grok/grok-4.5");
        assert_eq!(rows[0]["group_name"], "team/fast");
    }
}

/// Provider-shape route cases (apps/api/tests/custom_provider_llm_routes.test.ts and
/// antigravity_llm_routes.test.ts § `/anthropic`): which branch of `handle_anthropic_messages`
/// each resolved provider lands on, observed from the upstream the route actually called.
#[cfg(test)]
mod provider_shape_tests {
    use super::super::resolve_request::test_support::{body_json, fixture};
    use crate::db::custom_providers::{insert_custom_provider, NewCustomProvider};
    use crate::db::test_support::skip_without_db;
    use axum::http::StatusCode;
    use serde_json::json;
    use tower::ServiceExt;

    #[tokio::test]
    async fn a_custom_anthropic_slug_is_a_native_passthrough_to_its_own_base_url() {
        let Some(f) = fixture().await else { return skip_without_db() };
        f.custom_provider("my-claude", "anthropic", "https://upstream.example.com").await;
        f.mock.respond_json(
            StatusCode::OK,
            json!({ "id": "msg_1", "type": "message", "role": "assistant", "content": [{ "type": "text", "text": "hi" }], "usage": {} }),
        );

        let response = f
            .router()
            .oneshot(f.post(
                "/anthropic/v1/messages",
                json!({ "model": "my-claude/claude-3", "max_tokens": 16, "messages": [{ "role": "user", "content": "hi" }] }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let calls = f.mock.requests();
        assert_eq!(calls[0].url, "https://upstream.example.com/v1/messages");
        assert_eq!(calls[0].json()["model"], "claude-3");
        // The stored key is sent as the Anthropic-style header, never as a client-visible value.
        assert!(calls[0].header("x-api-key").is_some());
    }

    #[tokio::test]
    async fn a_custom_openai_slug_takes_the_anthropic_to_openai_conversion_path() {
        let Some(f) = fixture().await else { return skip_without_db() };
        f.custom_provider("my-oa2", "openai", "https://upstream.example.com/v1").await;
        f.mock.respond_json(
            StatusCode::OK,
            json!({ "id": "x", "choices": [{ "index": 0, "message": { "role": "assistant", "content": "hi" }, "finish_reason": "stop" }] }),
        );

        let response = f
            .router()
            .oneshot(f.post(
                "/anthropic/v1/messages",
                json!({ "model": "my-oa2/gpt-4o", "max_tokens": 100, "messages": [{ "role": "user", "content": "hi" }] }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(f.mock.requests()[0].url, "https://upstream.example.com/v1/chat/completions");
        assert_eq!(body_json(response).await["type"], "message");
    }

    #[tokio::test]
    async fn a_configured_count_tokens_url_is_posted_to_verbatim() {
        let Some(f) = fixture().await else { return skip_without_db() };
        insert_custom_provider(
            f.state.pool(),
            NewCustomProvider {
                user_id: &f.user.id,
                slug: "my-oa4",
                name: "my-oa4",
                format: "openai",
                base_url: "https://upstream.example.com/v1",
                count_tokens_url: Some("https://count.example.com/anthropic/count_tokens"),
                models_mode: "manual",
                manual_models_json: Some("[\"gpt-4o\"]"),
            },
        )
        .await
        .unwrap();
        f.account("my-oa4").await;
        f.mock.respond_json(StatusCode::OK, json!({ "input_tokens": 9 }));

        let response = f
            .router()
            .oneshot(f.post(
                "/anthropic/v1/messages/count_tokens",
                json!({ "model": "my-oa4/gpt-4o", "messages": [{ "role": "user", "content": "hi" }] }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await, json!({ "input_tokens": 9 }));
        let calls = f.mock.requests();
        assert_eq!(calls[0].url, "https://count.example.com/anthropic/count_tokens");
        assert_eq!(calls[0].json()["model"], "gpt-4o");
    }

    #[tokio::test]
    async fn antigravity_messages_take_the_converting_path_not_the_native_passthrough() {
        let Some(f) = fixture().await else { return skip_without_db() };
        f.account_with_extra("antigravity", json!({ "project_id": "proj-42" }).as_object().unwrap().clone()).await;
        f.mock.respond_json(
            StatusCode::OK,
            json!({ "response": { "candidates": [{ "content": { "role": "model", "parts": [{ "text": "hi there" }] }, "finishReason": "STOP" }] } }),
        );

        let response = f
            .router()
            .oneshot(f.post(
                "/anthropic/v1/messages",
                json!({ "model": "antigravity/gemini-3-flash", "system": "be terse", "max_tokens": 64, "messages": [{ "role": "user", "content": "hi" }] }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        // A Gemini endpoint, not an Anthropic one: the body was converted, not passed through.
        let calls = f.mock.requests();
        assert!(calls[0].url.ends_with("/v1internal:generateContent"), "{}", calls[0].url);
        let json = body_json(response).await;
        assert_eq!(json["type"], "message");
        assert_eq!(json["model"], "antigravity/gemini-3-flash");
        assert_eq!(json["content"], json!([{ "type": "text", "text": "hi there" }]));
    }

    #[tokio::test]
    async fn antigravity_count_tokens_is_a_real_upstream_count_not_a_400() {
        let Some(f) = fixture().await else { return skip_without_db() };
        f.account_with_extra("antigravity", json!({ "project_id": "proj-42" }).as_object().unwrap().clone()).await;
        f.mock.respond_json(StatusCode::OK, json!({ "totalTokens": 77 }));

        let response = f
            .router()
            .oneshot(f.post(
                "/anthropic/v1/messages/count_tokens",
                json!({ "model": "antigravity/gemini-3-flash", "messages": [{ "role": "user", "content": "hi" }] }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await, json!({ "input_tokens": 77 }));
        assert!(f.mock.requests()[0].url.ends_with("/v1internal:countTokens"), "{}", f.mock.requests()[0].url);
    }
}
