//! Port of apps/api/src/routes/openai.ts — the OpenAI-shaped surface (docs/api.md § OpenAI
//! surface): `POST /openai/v1/chat/completions`, `POST /openai/v1/responses`,
//! `POST /openai/v1/audio/transcriptions` and `GET /openai/v1/models`.
//!
//! The POST handlers are shared with the group mounts (`/g/{slug}/openai/v1/…`): resolution
//! branches on the mount's `slug`, everything past resolution is identical
//! (docs/api.md § Group endpoints).

use axum::extract::{Multipart, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use bytes::Bytes;
use serde_json::{json, Map, Value};

use crate::app::now_ms;
use crate::catalog::models::{list_models_for_user, ListModelsOptions};
use crate::extensions::ApiKeyIdentity;
use crate::http::errors::ApiError;
use crate::logging::request_log::{log_request, LogEntry};
use crate::providers::types::{AffinityIds, AudioForm, ChatCompletionRequest};
use crate::proxy::dispatch::{canonical_model_id, dispatch_chat_completions, ChatDispatchOptions};
use crate::proxy::dispatch_audio::{dispatch_audio_transcriptions, AudioDispatchOptions};
use crate::proxy::request_json::read_proxy_json;
use crate::utils::audio::{scan_audio_parts, SUPPORTED_AUDIO_FORMATS};
use crate::utils::loop_guard::{detect_openai_tool_loop, loop_detected_message};
use crate::utils::model::logging_provider_from_raw_model;
use crate::utils::reasoning::parse_reasoning_effort;
use crate::AppState;

use super::anthropic::is_native_anthropic_passthrough;
use super::resolve_request::{resolve_request_model, RequestModelResolution};
use super::responses::handle_responses;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/models", get(models))
        .route("/chat/completions", post(chat_completions))
        .route("/responses", post(responses))
        .route("/audio/transcriptions", post(audio_transcriptions))
}

// ---------------------------------------------------------------------------------------
// Shared helpers for the OpenAI-shaped surface (also used by responses.rs)
// ---------------------------------------------------------------------------------------

/// `c.executionCtx.waitUntil(logRequest(...))`: the row outlives the response.
pub(crate) fn spawn_log(cx: &AppState, entry: LogEntry) {
    let cx = cx.clone();
    tokio::spawn(async move { log_request(&cx, entry).await });
}

/// `String(body.model ?? "")`.
pub(crate) fn model_string(body: &Map<String, Value>) -> String {
    match body.get("model") {
        Some(Value::String(s)) => s.clone(),
        None | Some(Value::Null) => String::new(),
        Some(other) => other.to_string(),
    }
}

/// `model.slice(0, 200)` — `request_logs.model` is a display column, never a key.
pub(crate) fn log_model(model_raw: &str) -> String {
    model_raw.chars().take(200).collect()
}

/// Terminal client errors carry `x-should-retry: false` so a gateway client does not burn its
/// retry budget on them (docs/api.md § `x-should-retry` marker).
pub(crate) fn no_retry(error: ApiError) -> Response {
    error.should_retry(false).into_response()
}

/// The OpenAI-surface answer for a non-`ok` resolution, logged with the outcome's own status
/// and error code. Shared by chat completions, responses and audio transcriptions; `example`
/// is the shared base's `provider/model` hint for the `invalid_model` message.
pub(crate) fn openai_resolution_failure(
    cx: &AppState,
    id: &ApiKeyIdentity,
    outcome: &RequestModelResolution,
    model_raw: &str,
    started: i64,
    shared_base_example: &str,
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
        RequestModelResolution::GroupNotFound { slug } => no_retry(ApiError::openai(
            StatusCode::NOT_FOUND,
            &format!("unknown group endpoint \"{slug}\""),
            "invalid_request_error",
            "not_found",
        )),
        RequestModelResolution::InvalidModel { group_slug } => no_retry(ApiError::openai(
            StatusCode::BAD_REQUEST,
            &invalid_model_message(group_slug.as_deref(), shared_base_example),
            "invalid_request_error",
            "invalid_model",
        )),
        RequestModelResolution::Ok(_) => unreachable!("resolution succeeded"),
    }
}

/// The shared bases' `invalid_model` hints, per endpoint (apps/api/src/routes/openai.ts).
pub(crate) const CHAT_MODEL_EXAMPLE: &str = "model must be provider/model (e.g. claude-code/claude-opus-5)";
pub(crate) const AUDIO_MODEL_EXAMPLE: &str =
    "model must be provider/model (e.g. openrouter/openai/whisper-large-v3-turbo)";

pub(crate) fn invalid_model_message(group_slug: Option<&str>, shared_base: &str) -> String {
    match group_slug {
        Some(slug) => format!(
            "model must be one of this group endpoint's configured models (see GET /g/{slug}/openai/v1/models)"
        ),
        None => shared_base.to_string(),
    }
}

/// The three optional grok stickiness headers, forwarded only when the client sent them.
pub(crate) fn affinity_from(headers: &HeaderMap) -> AffinityIds {
    let read = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
    AffinityIds { conv_id: read("x-grok-conv-id"), session_id: read("x-grok-session-id"), turn_idx: read("x-grok-turn-idx") }
}

fn number_as_u64(value: Option<&Value>) -> Option<u64> {
    let n = value?.as_f64()?;
    if n.is_finite() && n >= 0.0 {
        Some(n as u64)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------------------
// GET /openai/v1/models
// ---------------------------------------------------------------------------------------

async fn models(State(state): State<AppState>, Extension(id): Extension<ApiKeyIdentity>) -> Response {
    // Live upstream models for providers the key owner has accounts for; never fabricated —
    // a provider with nothing usable simply contributes nothing (docs/api.md § GET /models).
    let listed = list_models_for_user(&state, &id.user_id, ListModelsOptions { available_only: true, force: false }).await;
    let data: Vec<Value> = listed
        .models
        .iter()
        .map(|m| json!({ "id": m.id, "object": "model", "owned_by": m.owned_by, "display_name": m.display_name }))
        .collect();
    Json(json!({ "object": "list", "data": data })).into_response()
}

// ---------------------------------------------------------------------------------------
// POST /openai/v1/chat/completions
// ---------------------------------------------------------------------------------------

async fn chat_completions(
    State(state): State<AppState>,
    Extension(id): Extension<ApiKeyIdentity>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_chat_completions(&state, &id, None, &headers, body).await
}

async fn responses(
    State(state): State<AppState>,
    Extension(id): Extension<ApiKeyIdentity>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_responses(&state, &id, None, &headers, body).await
}

/// Shared by the shared base and the group mounts (docs/api.md § Group endpoints).
pub async fn handle_chat_completions(
    cx: &AppState,
    id: &ApiKeyIdentity,
    slug: Option<&str>,
    headers: &HeaderMap,
    raw_body: Bytes,
) -> Response {
    let started = now_ms();
    let Ok(body) = read_proxy_json(&raw_body) else {
        return ApiError::openai(StatusCode::BAD_REQUEST, "Invalid JSON", "invalid_request_error", "invalid_request")
            .into_response();
    };
    let model_raw = model_string(&body);
    let resolved = match resolve_request_model(cx, &id.user_id, slug, &model_raw).await {
        Ok(RequestModelResolution::Ok(resolution)) => resolution,
        Ok(other) => return openai_resolution_failure(cx, id, &other, &model_raw, started, CHAT_MODEL_EXAMPLE),
        Err(err) => return ApiError::from(err).into_response(),
    };

    let effort = parse_reasoning_effort(body.get("reasoning_effort"));
    if effort.is_invalid() {
        return ApiError::openai(
            StatusCode::BAD_REQUEST,
            "invalid reasoning_effort",
            "invalid_request_error",
            "invalid_reasoning",
        )
        .into_response();
    }

    let messages: Vec<Value> = body.get("messages").and_then(Value::as_array).cloned().unwrap_or_default();
    let target_model = canonical_model_id(&resolved.primary.provider, &resolved.primary.upstream_model);

    // Loop guard applies on conversion ingress (grok / codex / antigravity / custom-openai);
    // never on native Anthropic passthrough adapters (docs/api.md § Degenerate tool-call loop
    // guard). Decided from the highest-priority resolved target only — a structural property.
    if !is_native_anthropic_passthrough(&resolved.primary.provider, resolved.primary.adapter.has_messages()) {
        let loop_detection = detect_openai_tool_loop(&messages);
        if loop_detection.tripped {
            spawn_log(
                cx,
                LogEntry {
                    user_id: id.user_id.clone(),
                    api_key_id: Some(id.api_key_id.clone()),
                    provider: resolved.primary.provider.clone(),
                    model: target_model.clone(),
                    status_code: 400,
                    latency_ms: now_ms() - started,
                    error_code: Some("loop_detected".into()),
                    group_name: resolved.group_name.clone(),
                    ..Default::default()
                },
            );
            return no_retry(ApiError::openai(
                StatusCode::BAD_REQUEST,
                &loop_detected_message(&loop_detection),
                "invalid_request_error",
                "loop_detected",
            ));
        }
    }

    // Audio input (docs/api.md § Audio input), decided from the highest-priority target for
    // the same reason: an adapter whose upstream wire has no audio content part must fail the
    // request rather than drop the part and answer as if the client had sent silence.
    let audio = scan_audio_parts(&messages);
    if audio.present {
        let rejection = match resolved.primary.adapter.audio_input() {
            None => Some((
                "unsupported_modality",
                format!(
                    "audio input is not supported by \"{}\" — its upstream message format has no audio content part",
                    resolved.primary.provider
                ),
            )),
            Some(crate::providers::types::AudioInput::Convert) if !audio.convertible => Some((
                "unsupported_audio_format",
                format!(
                    "input_audio part is not convertible: needs base64 input_audio.data plus a format of {SUPPORTED_AUDIO_FORMATS}, or a data: URL carrying its own mime"
                ),
            )),
            Some(_) => None,
        };
        if let Some((code, message)) = rejection {
            spawn_log(
                cx,
                LogEntry {
                    user_id: id.user_id.clone(),
                    api_key_id: Some(id.api_key_id.clone()),
                    provider: resolved.primary.provider.clone(),
                    model: target_model.clone(),
                    status_code: 400,
                    latency_ms: now_ms() - started,
                    error_code: Some(code.into()),
                    group_name: resolved.group_name.clone(),
                    ..Default::default()
                },
            );
            return no_retry(ApiError::openai(StatusCode::BAD_REQUEST, &message, "invalid_request_error", code));
        }
    }

    // temperature / top_p: numeric values read verbatim. Built-in adapters decide per-provider
    // what to do with them; a custom-openai provider forwards them through `raw_body`.
    let stop: Vec<String> = match body.get("stop") {
        Some(Value::Array(items)) => {
            items.iter().filter_map(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string).collect()
        }
        Some(Value::String(s)) if !s.is_empty() => vec![s.clone()],
        _ => Vec::new(),
    };
    let max_tokens = number_as_u64(body.get("max_tokens")).or_else(|| number_as_u64(body.get("max_completion_tokens")));

    let req = ChatCompletionRequest {
        model: model_raw.clone(),
        raw_model: model_raw,
        upstream_model: resolved.primary.upstream_model.clone(),
        messages,
        stream: Some(body.get("stream").and_then(Value::as_bool).unwrap_or(false)),
        max_tokens,
        tools: body.get("tools").cloned(),
        tool_choice: body.get("tool_choice").cloned(),
        response_format: body.get("response_format").cloned(),
        reasoning_effort: effort.effort(),
        temperature: body.get("temperature").and_then(Value::as_f64),
        top_p: body.get("top_p").and_then(Value::as_f64),
        stop: if stop.is_empty() { None } else { Some(stop) },
        prompt_cache_key: body.get("prompt_cache_key").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
        affinity: Some(affinity_from(headers)),
        responses_body: None,
        // Raw client body for the custom-openai passthrough adapter; ignored by built-ins,
        // which build their upstream body from named fields.
        raw_body: body,
    };

    dispatch_chat_completions(
        cx,
        ChatDispatchOptions {
            user_id: id.user_id.clone(),
            api_key_id: Some(id.api_key_id.clone()),
            provider: resolved.primary.provider.clone(),
            adapter: Some(resolved.primary.adapter.clone()),
            req,
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
// POST /openai/v1/audio/transcriptions
// ---------------------------------------------------------------------------------------

async fn audio_transcriptions(
    State(state): State<AppState>,
    Extension(id): Extension<ApiKeyIdentity>,
    multipart: Result<Multipart, axum::extract::multipart::MultipartRejection>,
) -> Response {
    handle_audio_transcriptions(&state, &id, None, multipart).await
}

pub async fn handle_audio_transcriptions(
    cx: &AppState,
    id: &ApiKeyIdentity,
    slug: Option<&str>,
    multipart: Result<Multipart, axum::extract::multipart::MultipartRejection>,
) -> Response {
    let started = now_ms();
    let Ok(multipart) = multipart else {
        return ApiError::openai(
            StatusCode::BAD_REQUEST,
            "Invalid multipart/form-data request",
            "invalid_request_error",
            "invalid_request",
        )
        .into_response();
    };
    // `file` is the only required part the reader can fail on; a missing one is reported below
    // with the TypeScript's own message, so the parse failure and the missing part stay apart.
    let mut fields: Vec<(String, String)> = Vec::new();
    let form = match read_audio_form_parts(multipart, &mut fields).await {
        Ok(form) => form,
        Err(()) => {
            return ApiError::openai(
                StatusCode::BAD_REQUEST,
                "Invalid multipart/form-data request",
                "invalid_request_error",
                "invalid_request",
            )
            .into_response()
        }
    };

    let model_raw = fields.iter().find(|(k, _)| k == "model").map(|(_, v)| v.trim().to_string()).unwrap_or_default();
    if model_raw.is_empty() {
        spawn_log(
            cx,
            LogEntry {
                user_id: id.user_id.clone(),
                api_key_id: Some(id.api_key_id.clone()),
                provider: "unknown".into(),
                model: String::new(),
                status_code: 400,
                latency_ms: now_ms() - started,
                error_code: Some("invalid_model".into()),
                ..Default::default()
            },
        );
        return no_retry(ApiError::openai(
            StatusCode::BAD_REQUEST,
            "Missing required parameter: 'model'",
            "invalid_request_error",
            "invalid_model",
        ));
    }

    let Some(form) = form else {
        spawn_log(
            cx,
            LogEntry {
                user_id: id.user_id.clone(),
                api_key_id: Some(id.api_key_id.clone()),
                provider: logging_provider_from_raw_model(&model_raw),
                model: log_model(&model_raw),
                status_code: 400,
                latency_ms: now_ms() - started,
                error_code: Some("invalid_request".into()),
                ..Default::default()
            },
        );
        return no_retry(ApiError::openai(
            StatusCode::BAD_REQUEST,
            "Missing required parameter: 'file'",
            "invalid_request_error",
            "invalid_request",
        ));
    };

    let resolved = match resolve_request_model(cx, &id.user_id, slug, &model_raw).await {
        Ok(RequestModelResolution::Ok(resolution)) => resolution,
        // Same envelopes as chat completions, with this endpoint's own `provider/model` example.
        Ok(other) => return openai_resolution_failure(cx, id, &other, &model_raw, started, AUDIO_MODEL_EXAMPLE),
        Err(err) => return ApiError::from(err).into_response(),
    };

    if !resolved.primary.adapter.has_audio_transcriptions() {
        spawn_log(
            cx,
            LogEntry {
                user_id: id.user_id.clone(),
                api_key_id: Some(id.api_key_id.clone()),
                provider: resolved.primary.provider.clone(),
                model: canonical_model_id(&resolved.primary.provider, &resolved.primary.upstream_model),
                status_code: 400,
                latency_ms: now_ms() - started,
                error_code: Some("unsupported_modality".into()),
                group_name: resolved.group_name.clone(),
                ..Default::default()
            },
        );
        return no_retry(ApiError::openai(
            StatusCode::BAD_REQUEST,
            &format!(
                "audio transcription is not supported by \"{}\" — only custom OpenAI-format providers support the audio/transcriptions endpoint",
                resolved.primary.provider
            ),
            "invalid_request_error",
            "unsupported_modality",
        ));
    }

    dispatch_audio_transcriptions(
        cx,
        AudioDispatchOptions {
            user_id: id.user_id.clone(),
            api_key_id: Some(id.api_key_id.clone()),
            provider: resolved.primary.provider.clone(),
            adapter: Some(resolved.primary.adapter.clone()),
            form,
            raw_model: model_raw,
            upstream_model: resolved.primary.upstream_model.clone(),
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

/// Drains the multipart body, collecting text fields into `fields` and returning the `file`
/// part when the client sent one. `Err(())` is a malformed body.
async fn read_audio_form_parts(
    mut multipart: Multipart,
    fields: &mut Vec<(String, String)>,
) -> Result<Option<AudioForm>, ()> {
    let mut file: Option<(String, String, Bytes)> = None;
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(_) => return Err(()),
        };
        let name = field.name().unwrap_or_default().to_string();
        if name == "file" {
            let file_name = field.file_name().unwrap_or("audio").to_string();
            let content_type = field.content_type().unwrap_or("application/octet-stream").to_string();
            let bytes = field.bytes().await.map_err(|_| ())?;
            file = Some((file_name, content_type, bytes));
        } else {
            let value = field.text().await.map_err(|_| ())?;
            fields.push((name, value));
        }
    }
    Ok(file.map(|(file_name, file_content_type, bytes)| AudioForm {
        fields: fields.clone(),
        file_name,
        file_content_type,
        file: bytes,
    }))
}

// ---------------------------------------------------------------------------------------
// Group mounts (`/g/{slug}/openai/v1/…`) — same handlers, slug-scoped resolution
// ---------------------------------------------------------------------------------------

pub(crate) async fn group_chat_completions(
    State(state): State<AppState>,
    Extension(id): Extension<ApiKeyIdentity>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_chat_completions(&state, &id, Some(&slug), &headers, body).await
}

pub(crate) async fn group_responses(
    State(state): State<AppState>,
    Extension(id): Extension<ApiKeyIdentity>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_responses(&state, &id, Some(&slug), &headers, body).await
}

pub(crate) async fn group_audio_transcriptions(
    State(state): State<AppState>,
    Extension(id): Extension<ApiKeyIdentity>,
    Path(slug): Path<String>,
    multipart: Result<Multipart, axum::extract::multipart::MultipartRejection>,
) -> Response {
    handle_audio_transcriptions(&state, &id, Some(&slug), multipart).await
}

/// Permissive CORS for the LLM surfaces (`application.ts`: `app.use("/openai/*", cors())`).
/// Never credentialed — these clients authenticate with a project API key, never the session
/// cookie, so a wildcard origin can never expose an admin response.
pub fn llm_cors() -> tower_http::cors::CorsLayer {
    tower_http::cors::CorsLayer::new()
        .allow_origin(tower_http::cors::Any)
        .allow_methods(tower_http::cors::Any)
        .allow_headers(tower_http::cors::Any)
}

/// The whole LLM surface: the two shared bases and the group mounts, behind the API-key
/// middleware (which also runs the edition's request policy) and permissive CORS — the order
/// `application.ts` composes.
pub fn llm_routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .nest("/openai/v1", routes())
        .nest("/anthropic", super::anthropic::routes())
        .nest("/g", super::group_endpoints::routes())
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), crate::auth::api_key_auth::api_key_auth))
        .layer(llm_cors())
}

#[cfg(test)]
mod tests {
    use super::super::resolve_request::test_support::{body_json, body_text, drain_sse, fixture, sse_events, Fixture};
    use crate::db::test_support::skip_without_db;
    use crate::upstream::{UpstreamResponse, UpstreamTransport};
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use serde_json::json;
    use tower::ServiceExt;

    const BASE_URL: &str = "https://upstream.example.com/v1";

    /// One assistant tool call plus its result — the loop guard's "unit".
    fn tool_unit(id: &str) -> Vec<serde_json::Value> {
        vec![
            json!({
                "role": "assistant",
                "tool_calls": [{ "id": id, "type": "function", "function": { "name": "Read", "arguments": "{\"p\":\"/a\"}" } }]
            }),
            json!({ "role": "tool", "tool_call_id": id, "content": "result" }),
        ]
    }

    fn tool_run(n: usize) -> Vec<serde_json::Value> {
        (0..n).flat_map(|i| tool_unit(&format!("call_{i}"))).collect()
    }

    fn chat_ok() -> serde_json::Value {
        json!({
            "id": "chatcmpl_1",
            "object": "chat.completion",
            "choices": [{ "index": 0, "message": { "role": "assistant", "content": "hello" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 11, "completion_tokens": 2 }
        })
    }

    async fn custom_gateway(f: &Fixture) {
        f.custom_provider("mygw", "openai", BASE_URL).await;
    }

    #[tokio::test]
    async fn models_lists_the_bound_catalog_and_fabricates_nothing() {
        let Some(f) = fixture().await else { return skip_without_db() };

        // No bound accounts at all: an empty list, never an invented catalog.
        let response = f.router().oneshot(f.get("/openai/v1/models")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await, json!({ "object": "list", "data": [] }));

        custom_gateway(&f).await;
        let response = f.router().oneshot(f.get("/openai/v1/models")).await.unwrap();
        let json = body_json(response).await;
        assert_eq!(json["object"], "list");
        assert_eq!(json["data"][0]["id"], "mygw/local-model");
        assert_eq!(json["data"][0]["object"], "model");
    }

    #[tokio::test]
    async fn chat_completions_reaches_the_resolved_upstream_and_logs_one_row() {
        let Some(f) = fixture().await else { return skip_without_db() };
        custom_gateway(&f).await;
        f.mock.respond_json(StatusCode::OK, chat_ok());

        let response = f
            .router()
            .oneshot(f.post(
                "/openai/v1/chat/completions",
                json!({ "model": "mygw/local-model", "messages": [{ "role": "user", "content": "hi" }], "max_tokens": 16 }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await["choices"][0]["message"]["content"], "hello");

        let calls = f.mock.requests();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].url, format!("{BASE_URL}/chat/completions"));
        // The upstream sees the bare model id; the client's own string is only echoed back.
        assert_eq!(calls[0].json()["model"], "local-model");

        let rows = f.logs(1).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["provider"], "mygw");
        assert_eq!(rows[0]["model"], "mygw/local-model");
        assert_eq!(rows[0]["status_code"], 200);
        assert_eq!(rows[0]["prompt_tokens"], 11);
        assert_eq!(rows[0]["error_code"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn a_streaming_chat_request_is_answered_as_sse() {
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
                "/openai/v1/chat/completions",
                json!({ "model": "mygw/local-model", "messages": [{ "role": "user", "content": "hi" }], "stream": true }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers()[axum::http::header::CONTENT_TYPE].to_str().unwrap().contains("text/event-stream"));
        let text = drain_sse(response).await;
        let events = sse_events(&text);
        assert_eq!(events[0]["choices"][0]["delta"]["content"], "hi");
        assert!(text.contains("[DONE]"));

        let rows = f.logs(1).await;
        assert_eq!(rows[0]["completion_tokens"], 1);
    }

    #[tokio::test]
    async fn an_unresolvable_model_is_400_invalid_model_marked_unretryable_and_logged() {
        let Some(f) = fixture().await else { return skip_without_db() };

        let response = f
            .router()
            .oneshot(f.post("/openai/v1/chat/completions", json!({ "model": "bare-model", "messages": [] })))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()["x-should-retry"], "false");
        let json = body_json(response).await;
        assert_eq!(json["error"]["code"], "invalid_model");
        assert_eq!(json["error"]["message"], super::CHAT_MODEL_EXAMPLE);

        let rows = f.logs(1).await;
        assert_eq!(rows[0]["status_code"], 400);
        assert_eq!(rows[0]["error_code"], "invalid_model");
        assert_eq!(rows[0]["model"], "bare-model");
        assert_eq!(f.mock.requests().len(), 0);
    }

    #[tokio::test]
    async fn a_group_mount_answers_404_for_an_unknown_slug_and_dispatches_a_known_one() {
        let Some(f) = fixture().await else { return skip_without_db() };
        custom_gateway(&f).await;
        f.group("team", "fast", &["mygw/local-model"]).await;

        let response = f
            .router()
            .oneshot(f.post("/g/nope/openai/v1/chat/completions", json!({ "model": "fast", "messages": [] })))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers()["x-should-retry"], "false");
        let json = body_json(response).await;
        assert_eq!(json["error"]["code"], "not_found");
        assert_eq!(json["error"]["message"], "unknown group endpoint \"nope\"");

        // A known slug with a name it does not define is invalid_model, pointing at the group.
        let response = f
            .router()
            .oneshot(f.post(
                "/g/team/openai/v1/chat/completions",
                json!({ "model": "mygw/local-model", "messages": [] }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            body_json(response).await["error"]["message"],
            "model must be one of this group endpoint's configured models (see GET /g/team/openai/v1/models)"
        );

        f.mock.respond_json(StatusCode::OK, chat_ok());
        let response = f
            .router()
            .oneshot(f.post(
                "/g/team/openai/v1/chat/completions",
                json!({ "model": "fast", "messages": [{ "role": "user", "content": "hi" }] }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        // The row stores the expanded canonical target plus the group alias.
        let rows = f.logs(3).await;
        let dispatched = rows.iter().find(|r| r["status_code"] == 200).expect("a dispatched row");
        assert_eq!(dispatched["model"], "mygw/local-model");
        assert_eq!(dispatched["group_name"], "team/fast");
    }

    #[tokio::test]
    async fn a_group_catalog_lists_exactly_its_model_names() {
        let Some(f) = fixture().await else { return skip_without_db() };
        custom_gateway(&f).await;
        f.group("team", "fast", &["mygw/local-model"]).await;

        let response = f.router().oneshot(f.get("/g/team/openai/v1/models")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            body_json(response).await,
            json!({ "object": "list", "data": [{ "id": "fast", "object": "model", "owned_by": "group", "display_name": "fast" }] })
        );

        let response = f.router().oneshot(f.get("/g/nope/openai/v1/models")).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_json(response).await["error"]["code"], "not_found");
    }

    #[tokio::test]
    async fn a_degenerate_tool_call_loop_is_refused_before_any_upstream_call() {
        let Some(f) = fixture().await else { return skip_without_db() };
        custom_gateway(&f).await;

        let response = f
            .router()
            .oneshot(f.post("/openai/v1/chat/completions", json!({ "model": "mygw/local-model", "messages": tool_run(8) })))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()["x-should-retry"], "false");
        assert_eq!(body_json(response).await["error"]["code"], "loop_detected");
        assert_eq!(f.mock.requests().len(), 0);
        let rows = f.logs(1).await;
        assert_eq!(rows[0]["error_code"], "loop_detected");
        assert_eq!(rows[0]["model"], "mygw/local-model");

        // One repetition short of the threshold still dispatches.
        f.mock.respond_json(StatusCode::OK, chat_ok());
        let response = f
            .router()
            .oneshot(f.post("/openai/v1/chat/completions", json!({ "model": "mygw/local-model", "messages": tool_run(7) })))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_provider_with_no_bound_account_is_400_no_upstream_account() {
        let Some(f) = fixture().await else { return skip_without_db() };

        let response = f
            .router()
            .oneshot(f.post(
                "/openai/v1/chat/completions",
                json!({ "model": "claude-code/claude-opus-5", "messages": [{ "role": "user", "content": "hi" }] }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()["x-should-retry"], "false");
        assert_eq!(body_json(response).await["error"]["code"], "no_upstream_account");
        assert_eq!(f.mock.requests().len(), 0);
    }

    #[tokio::test]
    async fn an_unusable_body_or_reasoning_effort_is_400_before_resolution_work() {
        let Some(f) = fixture().await else { return skip_without_db() };
        custom_gateway(&f).await;

        let bad_json = axum::http::Request::builder()
            .method("POST")
            .uri("/openai/v1/chat/completions")
            .header("authorization", format!("Bearer {}", f.key))
            .header("content-type", "application/json")
            .body(axum::body::Body::from("not json"))
            .unwrap();
        let response = f.router().oneshot(bad_json).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await["error"]["code"], "invalid_request");

        let response = f
            .router()
            .oneshot(f.post(
                "/openai/v1/chat/completions",
                json!({ "model": "mygw/local-model", "messages": [], "reasoning_effort": "turbo" }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await["error"]["code"], "invalid_reasoning");
        assert_eq!(f.mock.requests().len(), 0);
    }

    fn audio_messages(input_audio: serde_json::Value) -> serde_json::Value {
        json!([{ "role": "user", "content": [{ "type": "text", "text": "what word is this" }, { "type": "input_audio", "input_audio": input_audio }] }])
    }

    #[tokio::test]
    async fn an_audio_part_is_refused_by_a_wire_that_has_none_and_by_an_unnameable_format() {
        let Some(f) = fixture().await else { return skip_without_db() };
        f.account("claude-code").await;
        f.account("antigravity").await;

        // claude-code builds an Anthropic body: no audio content part at all.
        let response = f
            .router()
            .oneshot(f.post(
                "/openai/v1/chat/completions",
                json!({ "model": "claude-code/claude-opus-5", "messages": audio_messages(json!({ "data": "UklGRiQ", "format": "wav" })) }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()["x-should-retry"], "false");
        let json = body_json(response).await;
        assert_eq!(json["error"]["code"], "unsupported_modality");
        assert!(json["error"]["message"].as_str().unwrap().contains("claude-code"));

        // antigravity converts, so an unrecognized format is refused rather than guessed.
        let response = f
            .router()
            .oneshot(f.post(
                "/openai/v1/chat/completions",
                json!({ "model": "antigravity/gemini-3-flash", "messages": audio_messages(json!({ "data": "UklGRiQ", "format": "wma" })) }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await["error"]["code"], "unsupported_audio_format");
        assert_eq!(f.mock.requests().len(), 0);

        // Rows are written by spawned tasks, so match them by content rather than order.
        let rows = f.logs(2).await;
        let by_code = |code: &str| rows.iter().find(|r| r["error_code"] == code).unwrap_or_else(|| panic!("no {code} row"));
        assert_eq!(by_code("unsupported_modality")["provider"], "claude-code");
        assert_eq!(by_code("unsupported_audio_format")["provider"], "antigravity");
    }

    #[tokio::test]
    async fn audio_transcriptions_requires_model_and_file_then_forwards_the_form() {
        let Some(f) = fixture().await else { return skip_without_db() };
        custom_gateway(&f).await;

        let response = f
            .router()
            .oneshot(f.multipart("/openai/v1/audio/transcriptions", &[], Some(("clip.mp3", b"ID3"))))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let json = body_json(response).await;
        assert_eq!(json["error"]["code"], "invalid_model");
        assert_eq!(json["error"]["message"], "Missing required parameter: 'model'");

        let response = f
            .router()
            .oneshot(f.multipart("/openai/v1/audio/transcriptions", &[("model", "mygw/whisper-1")], None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let json = body_json(response).await;
        assert_eq!(json["error"]["code"], "invalid_request");
        assert_eq!(json["error"]["message"], "Missing required parameter: 'file'");

        f.mock.expect(|_| {
            Ok(UpstreamResponse::json(StatusCode::OK, &json!({ "text": "hello there" })))
        });
        let response = f
            .router()
            .oneshot(f.multipart(
                "/openai/v1/audio/transcriptions",
                &[("model", "mygw/whisper-1"), ("language", "en")],
                Some(("clip.mp3", b"ID3")),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await["text"], "hello there");
        let calls = f.mock.requests();
        assert_eq!(calls[0].url, format!("{BASE_URL}/audio/transcriptions"));
        let sent = String::from_utf8_lossy(calls[0].body.as_deref().unwrap()).to_string();
        // The client's own fields ride along; `model` is rewritten to the bare upstream id.
        assert!(sent.contains("whisper-1"));
        assert!(!sent.contains("mygw/whisper-1"));
        assert!(sent.contains("name=\"language\""));
        assert!(sent.contains("filename=\"clip.mp3\""));

        // Rows are written by spawned tasks, so match them by content rather than order.
        let rows = f.logs(3).await;
        assert!(rows.iter().any(|r| r["error_code"] == "invalid_model"));
        assert!(rows.iter().any(|r| r["error_code"] == "invalid_request"));
        assert!(rows.iter().any(|r| r["status_code"] == 200));
    }

    #[tokio::test]
    async fn audio_transcriptions_refuses_a_builtin_subscription_provider() {
        let Some(f) = fixture().await else { return skip_without_db() };
        f.account("claude-code").await;

        let response = f
            .router()
            .oneshot(f.multipart(
                "/openai/v1/audio/transcriptions",
                &[("model", "claude-code/claude-opus-5")],
                Some(("clip.mp3", b"ID3")),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()["x-should-retry"], "false");
        let json = body_json(response).await;
        assert_eq!(json["error"]["code"], "unsupported_modality");
        assert!(json["error"]["message"].as_str().unwrap().contains("only custom OpenAI-format providers"));
        assert_eq!(f.mock.requests().len(), 0);
        let rows = f.logs(1).await;
        assert_eq!(rows[0]["error_code"], "unsupported_modality");

        // An unresolvable model carries this endpoint's own `provider/model` example.
        let response = f
            .router()
            .oneshot(f.multipart("/openai/v1/audio/transcriptions", &[("model", "bare")], Some(("clip.mp3", b"ID3"))))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await["error"]["message"], super::AUDIO_MODEL_EXAMPLE);
    }

    #[tokio::test]
    async fn a_malformed_multipart_body_is_400_invalid_request() {
        let Some(f) = fixture().await else { return skip_without_db() };
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/openai/v1/audio/transcriptions")
            .header("authorization", format!("Bearer {}", f.key))
            .header("content-type", "application/json")
            .body(axum::body::Body::from("{}"))
            .unwrap();
        let response = f.router().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let json = body_json(response).await;
        assert_eq!(json["error"]["code"], "invalid_request");
        assert_eq!(json["error"]["message"], "Invalid multipart/form-data request");
    }

    #[tokio::test]
    async fn a_key_over_its_spend_limit_is_refused_on_the_model_surfaces_but_not_on_models() {
        let Some(f) = fixture().await else { return skip_without_db() };
        let limits = crate::db::keys::SpendLimitFields {
            spend_limit: Some(1.0),
            spend_limit_interval: "monthly".into(),
            spend_limit_include_oauth: true,
        };
        let (key_row, plaintext) =
            crate::db::test_support::insert_api_key_with_limit(f.state.pool(), &f.user.id, limits).await;
        crate::db::request_logs::insert_request_log(
            f.state.pool(),
            &crate::db::request_logs::RequestLogEntry {
                user_id: f.user.id.clone(),
                api_key_id: Some(key_row.id.clone()),
                provider: "mygw".into(),
                model: "mygw/local-model".into(),
                status_code: 200,
                latency_ms: 1,
                cost: Some(5.0),
                started_at: crate::ids::now_iso(),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let limited = |path: &str, method: &str| {
            axum::http::Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {plaintext}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from("{}"))
                .unwrap()
        };

        let response = f.router().oneshot(limited("/openai/v1/chat/completions", "POST")).await.unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()["x-should-retry"], "false");
        assert_eq!(body_json(response).await["error"]["code"], "spend_limit_exceeded");

        // The Anthropic mount answers the same refusal in its own envelope.
        let response = f.router().oneshot(limited("/anthropic/v1/messages", "POST")).await.unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body_json(response).await["error"]["type"], "rate_limit_error");

        // GET /models stays free so a blocked client can still read its catalog.
        let response = f.router().oneshot(limited("/openai/v1/models", "GET")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn the_cors_on_the_llm_surfaces_is_permissive_and_never_credentialed() {
        let Some(f) = fixture().await else { return skip_without_db() };
        let request = axum::http::Request::builder()
            .uri("/openai/v1/models")
            .header("origin", "https://someone-elses-page.example")
            .header("authorization", format!("Bearer {}", f.key))
            .body(axum::body::Body::empty())
            .unwrap();
        let response = f.router().oneshot(request).await.unwrap();
        assert_eq!(response.headers()["access-control-allow-origin"], "*");
        assert!(response.headers().get("access-control-allow-credentials").is_none());
        let _ = body_text(response).await;
    }

    #[tokio::test]
    async fn audio_transcriptions_rejects_an_anonymous_request_and_a_custom_anthropic_target() {
        let Some(f) = fixture().await else { return skip_without_db() };
        f.custom_provider("my-claude", "anthropic", "https://upstream.example.com").await;

        let anonymous = axum::http::Request::builder()
            .method("POST")
            .uri("/openai/v1/audio/transcriptions")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = f.router().oneshot(anonymous).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(response).await["error"]["code"], "invalid_api_key");

        // An anthropic-format custom provider has no transcription wire either.
        let response = f
            .router()
            .oneshot(f.multipart(
                "/openai/v1/audio/transcriptions",
                &[("model", "my-claude/whisper-1")],
                Some(("clip.mp3", b"ID3")),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await["error"]["code"], "unsupported_modality");
        assert_eq!(f.mock.requests().len(), 0);
    }

    #[tokio::test]
    async fn audio_transcriptions_pass_a_subtitle_or_plain_text_body_through_unchanged() {
        let Some(f) = fixture().await else { return skip_without_db() };
        custom_gateway(&f).await;
        f.mock.expect(|_| {
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(axum::http::header::CONTENT_TYPE, axum::http::HeaderValue::from_static("text/plain"));
            Ok(UpstreamResponse::from_bytes(StatusCode::OK, headers, "1\n00:00:00,000 --> 00:00:01,000\nhello\n"))
        });

        let response = f
            .router()
            .oneshot(f.multipart(
                "/openai/v1/audio/transcriptions",
                &[("model", "mygw/whisper-1"), ("response_format", "srt")],
                Some(("clip.mp3", b"ID3")),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[axum::http::header::CONTENT_TYPE], "text/plain");
        assert!(body_text(response).await.contains("00:00:00,000 --> 00:00:01,000"));
    }

    /// Records the calls it saw and answers 429 without running the route, so the test can
    /// prove the middleware order the TypeScript relied on: authenticate, then policy, then
    /// handler (apps/api/tests/application.test.ts).
    struct RecordingPolicy {
        seen: std::sync::Mutex<Vec<(String, Option<crate::extensions::ApiKeyIdentity>)>>,
    }

    #[async_trait::async_trait]
    impl crate::extensions::RequestPolicy for RecordingPolicy {
        async fn handle(
            &self,
            _cx: crate::AppState,
            req: axum::extract::Request,
            _next: axum::middleware::Next,
        ) -> axum::response::Response {
            let identity = req.extensions().get::<crate::extensions::ApiKeyIdentity>().cloned();
            self.seen.lock().unwrap().push((req.uri().path().to_string(), identity));
            (StatusCode::TOO_MANY_REQUESTS, axum::Json(json!({ "policy": true }))).into_response()
        }
    }

    #[tokio::test]
    async fn the_request_policy_runs_after_authentication_on_every_post_model_surface() {
        let Some(f) = fixture().await else { return skip_without_db() };
        let policy = std::sync::Arc::new(RecordingPolicy { seen: std::sync::Mutex::new(Vec::new()) });
        let state = crate::AppState::builder(crate::db::test_support::test_config(), f.state.pool().clone())
            .transport(f.mock.clone() as std::sync::Arc<dyn UpstreamTransport>)
            .request_policy(Some(policy.clone()))
            .build();
        let router = || crate::build_router(state.clone(), crate::extensions::Extensions::default());

        let paths = [
            "/openai/v1/chat/completions",
            "/openai/v1/responses",
            "/openai/v1/audio/transcriptions",
            "/anthropic/v1/messages",
            "/anthropic/v1/messages/count_tokens",
            "/g/test/openai/v1/chat/completions",
            "/g/test/openai/v1/responses",
            "/g/test/openai/v1/audio/transcriptions",
            "/g/test/anthropic/v1/messages",
            "/g/test/anthropic/v1/messages/count_tokens",
        ];
        for path in paths {
            // Unauthenticated: 401 before the policy ever sees the request.
            let response = router()
                .oneshot(
                    axum::http::Request::builder()
                        .method("POST")
                        .uri(path)
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
            assert!(policy.seen.lock().unwrap().is_empty(), "{path} reached the policy unauthenticated");

            let response = router()
                .oneshot(
                    axum::http::Request::builder()
                        .method("POST")
                        .uri(path)
                        .header("authorization", format!("Bearer {}", f.key))
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS, "{path}");
            assert_eq!(body_json(response).await, json!({ "policy": true }));
            let seen = policy.seen.lock().unwrap().drain(..).collect::<Vec<_>>();
            assert_eq!(seen.len(), 1, "{path}");
            assert_eq!(seen[0].0, path);
            // The identity is already established when the policy runs.
            assert_eq!(seen[0].1.as_ref().map(|i| i.user_id.clone()), Some(f.user.id.clone()));
        }
        // The policy answered without ever reaching a route, so no upstream was touched.
        assert_eq!(f.mock.requests().len(), 0);
    }
}
