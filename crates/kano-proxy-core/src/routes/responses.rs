//! Port of apps/api/src/routes/responses.ts — `POST /openai/v1/responses` and its group mount
//! (docs/api.md § `POST /openai/v1/responses`).
//!
//! Native codex passthrough when every resolved candidate is codex; otherwise Responses ↔ Chat
//! conversion around the ordinary chat dispatch. Pre-dispatch rejections mirror
//! `handle_chat_completions` — same codes, same logging, same retry marker.

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use futures::TryStreamExt;
use http_body_util::BodyExt;
use serde_json::Value;

use crate::app::now_ms;
use crate::extensions::ApiKeyIdentity;
use crate::http::errors::ApiError;
use crate::logging::request_log::LogEntry;
use crate::providers::types::ChatCompletionRequest;
use crate::proxy::dispatch::{
    canonical_model_id, dispatch_chat_completions, is_event_stream, ChatDispatchOptions,
};
use crate::proxy::request_json::read_proxy_json;
use crate::proxy::responses_openai::{
    openai_sse_to_responses_stream, openai_to_responses_object, responses_to_chat_request,
    rewrite_openai_error_frames_to_responses, ResponsesOutputOptions,
};
use crate::proxy::sse::{stream_with_keepalive, StreamKeepaliveOpts, DEFAULT_KEEPALIVE_INTERVAL};
use crate::upstream::ByteStream;
use crate::utils::loop_guard::{detect_openai_tool_loop, loop_detected_message};
use crate::utils::model::logging_provider_from_raw_model;
use crate::utils::reasoning::parse_reasoning_effort;
use crate::AppState;

use super::openai::{affinity_from, log_model, model_string, no_retry, spawn_log, CHAT_MODEL_EXAMPLE};
use super::resolve_request::{resolve_request_model, RequestModelResolution};

/// What a pre-dispatch rejection records: the target it had resolved (or the raw string's own
/// provider guess when resolution itself failed).
struct RejectTarget {
    provider: String,
    model: String,
    group_name: Option<String>,
}

fn sse_response(status: StatusCode, body: Body) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(body)
        .expect("response builds")
}

/// An axum response body as the `Bytes`/`io::Error` stream the converters take.
fn byte_stream(body: Body) -> ByteStream {
    Box::pin(TryStreamExt::map_err(body.into_data_stream(), std::io::Error::other))
}

pub async fn handle_responses(
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

    let reject = |status: StatusCode, code: &str, message: &str, target: &RejectTarget| -> Response {
        spawn_log(
            cx,
            LogEntry {
                user_id: id.user_id.clone(),
                api_key_id: Some(id.api_key_id.clone()),
                provider: target.provider.clone(),
                model: log_model(&target.model),
                status_code: status.as_u16() as i32,
                latency_ms: now_ms() - started,
                error_code: Some(code.to_string()),
                group_name: target.group_name.clone(),
                ..Default::default()
            },
        );
        no_retry(ApiError::openai(status, message, "invalid_request_error", code))
    };

    let resolved = match resolve_request_model(cx, &id.user_id, slug, &model_raw).await {
        Ok(RequestModelResolution::Ok(resolution)) => resolution,
        Ok(outcome) => {
            let target = RejectTarget {
                provider: logging_provider_from_raw_model(&model_raw),
                model: model_raw.clone(),
                group_name: None,
            };
            return match outcome {
                RequestModelResolution::GroupNotFound { slug } => reject(
                    StatusCode::NOT_FOUND,
                    "group_not_found",
                    &format!("unknown group endpoint \"{slug}\""),
                    &target,
                ),
                RequestModelResolution::InvalidModel { group_slug } => reject(
                    StatusCode::BAD_REQUEST,
                    "invalid_model",
                    &super::openai::invalid_model_message(group_slug.as_deref(), CHAT_MODEL_EXAMPLE),
                    &target,
                ),
                RequestModelResolution::Ok(_) => unreachable!("resolution succeeded"),
            };
        }
        Err(err) => return ApiError::from(err).into_response(),
    };
    let target = RejectTarget {
        provider: resolved.primary.provider.clone(),
        model: canonical_model_id(&resolved.primary.provider, &resolved.primary.upstream_model),
        group_name: resolved.group_name.clone(),
    };

    let converted = match responses_to_chat_request(&body) {
        Ok(converted) => converted,
        Err(unsupported) => {
            return reject(StatusCode::BAD_REQUEST, "unsupported_field", &unsupported.message, &target)
        }
    };
    let chat = converted.chat;

    let effort = parse_reasoning_effort(chat.get("reasoning_effort"));
    if effort.is_invalid() {
        return ApiError::openai(
            StatusCode::BAD_REQUEST,
            "invalid reasoning.effort",
            "invalid_request_error",
            "invalid_reasoning",
        )
        .into_response();
    }

    let messages: Vec<Value> = chat.get("messages").and_then(Value::as_array).cloned().unwrap_or_default();
    let loop_detection = detect_openai_tool_loop(&messages);
    if loop_detection.tripped {
        return reject(StatusCode::BAD_REQUEST, "loop_detected", &loop_detected_message(&loop_detection), &target);
    }

    // Native only when nothing in the candidate list could answer in another wire: a mixed
    // group's failover would otherwise hand a Responses client a Chat stream (docs/api.md
    // "Native path — every candidate is codex").
    let native = !resolved.candidates.is_empty() && resolved.candidates.iter().all(|c| c.provider == "codex");
    let stream = chat.get("stream") == Some(&Value::Bool(true));

    let req = ChatCompletionRequest {
        model: model_raw.clone(),
        raw_model: model_raw.clone(),
        upstream_model: resolved.primary.upstream_model.clone(),
        messages,
        stream: Some(stream),
        max_tokens: chat.get("max_tokens").and_then(Value::as_f64).filter(|n| n.is_finite() && *n >= 0.0).map(|n| n as u64),
        tools: chat.get("tools").cloned(),
        tool_choice: chat.get("tool_choice").cloned(),
        response_format: chat.get("response_format").cloned(),
        reasoning_effort: effort.effort(),
        temperature: chat.get("temperature").and_then(Value::as_f64),
        top_p: chat.get("top_p").and_then(Value::as_f64),
        stop: None,
        prompt_cache_key: chat.get("prompt_cache_key").and_then(Value::as_str).map(str::to_string),
        affinity: Some(affinity_from(headers)),
        responses_body: if native { Some(body.clone()) } else { None },
        raw_body: chat,
    };

    let upstream = dispatch_chat_completions(
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
    .await;

    let event_stream = stream || is_event_stream(&upstream);
    let ok = upstream.status().is_success();

    if native {
        // The codex adapter already answered in Responses shape; only dispatch's own in-stream
        // error frames need translating.
        if event_stream && ok {
            let status = upstream.status();
            let converted = rewrite_openai_error_frames_to_responses(byte_stream(upstream.into_body()), model_raw);
            return sse_response(status, Body::from_stream(converted));
        }
        return upstream;
    }

    let out_opts = ResponsesOutputOptions { model: model_raw, tool_names: converted.tool_names };
    if event_stream {
        if !ok {
            return upstream;
        }
        // Keepalive again on the converted stream: the inner wrapper's comments are consumed by
        // the converter, and a long reasoning turn would otherwise send zero bytes until the
        // first token.
        let status = upstream.status();
        let converted = openai_sse_to_responses_stream(byte_stream(upstream.into_body()), out_opts);
        let kept = stream_with_keepalive(Box::pin(converted), DEFAULT_KEEPALIVE_INTERVAL, StreamKeepaliveOpts::default());
        return sse_response(status, Body::from_stream(kept));
    }

    let status = upstream.status();
    let upstream_headers = upstream.headers().clone();
    let bytes = match upstream.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => Bytes::new(),
    };
    if !ok {
        let mut response = Response::builder().status(status);
        let content_type = upstream_headers
            .get(header::CONTENT_TYPE)
            .cloned()
            .unwrap_or(HeaderValue::from_static("application/json"));
        response = response.header(header::CONTENT_TYPE, content_type);
        for name in ["x-should-retry", "retry-after"] {
            if let Some(value) = upstream_headers.get(name) {
                response = response.header(name, value.clone());
            }
        }
        return response.body(Body::from(bytes)).expect("response builds");
    }
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(json) => Json(openai_to_responses_object(&json, &out_opts)).into_response(),
        Err(_) => {
            let content_type = upstream_headers
                .get(header::CONTENT_TYPE)
                .cloned()
                .unwrap_or(HeaderValue::from_static("application/json"));
            Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(bytes))
                .expect("response builds")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::resolve_request::test_support::{body_json, drain_sse, fixture, sse_events, Fixture};
    use crate::db::test_support::skip_without_db;
    use axum::http::StatusCode;
    use serde_json::json;
    use tower::ServiceExt;

    const BASE_URL: &str = "https://upstream.example.com/v1";

    async fn custom_gateway(f: &Fixture) {
        f.custom_provider("mygw", "openai", BASE_URL).await;
    }

    /// The Codex CLI 0.150.1 request shape (captured 2026-09-04), model swapped per test.
    fn codex_cli_body(model: &str, extra: serde_json::Value) -> serde_json::Value {
        let mut body = json!({
            "model": model,
            "instructions": "You are Codex.",
            "input": [
                { "type": "message", "role": "developer", "content": [{ "type": "input_text", "text": "env" }] },
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "say hi" }] }
            ],
            "tools": [
                { "type": "function", "name": "exec_command", "strict": false, "parameters": { "type": "object", "properties": {} } },
                {
                    "type": "namespace",
                    "name": "multi_agent_v1",
                    "description": "Tools in the multi_agent_v1 namespace.",
                    "tools": [{ "type": "function", "name": "spawn_agent", "strict": false, "parameters": { "type": "object", "properties": {} } }]
                },
                { "type": "web_search", "external_web_access": false }
            ],
            "tool_choice": "auto",
            "parallel_tool_calls": true,
            "reasoning": { "summary": "auto" },
            "store": false,
            "stream": true,
            "include": ["reasoning.encrypted_content"],
            "prompt_cache_key": "01a06ce4-68a8-7892-bcf5-b64013c7f279",
            "client_metadata": { "session_id": "01a06ce4-68a8-7892-bcf5-b64013c7f279" }
        });
        for (key, value) in extra.as_object().cloned().unwrap_or_default() {
            body[key] = value;
        }
        body
    }

    const CHAT_SSE: &str = "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hello\"},\"finish_reason\":null}]}\n\n\
         data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":2}}\n\n\
         data: [DONE]\n\n";

    const RESPONSES_SSE: &str = "event: response.created\ndata: {\"type\":\"response.created\",\"sequence_number\":0,\"response\":{\"id\":\"resp_up\",\"status\":\"in_progress\",\"output\":[]}}\n\n\
         event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"sequence_number\":1,\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"delta\":\"hi\"}\n\n\
         event: response.completed\ndata: {\"type\":\"response.completed\",\"sequence_number\":2,\"response\":{\"id\":\"resp_up\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":7,\"output_tokens\":1,\"input_tokens_details\":{\"cached_tokens\":3}}}}\n\n";

    // ---- conversion path ----

    #[tokio::test]
    async fn a_codex_cli_request_converts_for_a_custom_provider_and_streams_responses_events_back() {
        let Some(f) = fixture().await else { return skip_without_db() };
        custom_gateway(&f).await;
        f.mock.respond_sse(StatusCode::OK, CHAT_SSE);

        let response = f
            .router()
            .oneshot(f.post("/openai/v1/responses", codex_cli_body("mygw/local-model", json!({}))))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers()[axum::http::header::CONTENT_TYPE].to_str().unwrap().contains("text/event-stream"));
        let text = drain_sse(response).await;

        let calls = f.mock.requests();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].url, format!("{BASE_URL}/chat/completions"));
        let sent = calls[0].json();
        assert_eq!(sent["model"], "local-model");
        assert_eq!(
            sent["messages"],
            json!([
                { "role": "system", "content": "You are Codex." },
                { "role": "system", "content": "env" },
                { "role": "user", "content": "say hi" }
            ])
        );
        let tool_names: Vec<&str> =
            sent["tools"].as_array().unwrap().iter().map(|t| t["function"]["name"].as_str().unwrap()).collect();
        assert_eq!(tool_names, ["exec_command", "multi_agent_v1__spawn_agent", "web_search"]);
        assert!(sent.get("client_metadata").is_none());
        assert!(sent.get("include").is_none());
        assert_eq!(sent["stream"], true);

        let events = sse_events(&text);
        assert_eq!(events[0]["type"], "response.created");
        assert!(events.iter().any(|e| e["type"] == "response.output_text.delta" && e["delta"] == "hello"));
        let completed = events.last().unwrap();
        assert_eq!(completed["type"], "response.completed");
        assert_eq!(completed["response"]["model"], "mygw/local-model");
        assert_eq!(completed["response"]["usage"], json!({ "input_tokens": 11, "output_tokens": 2, "total_tokens": 13 }));

        let rows = f.logs(1).await;
        assert_eq!(rows[0]["provider"], "mygw");
        assert_eq!(rows[0]["model"], "mygw/local-model");
        assert_eq!(rows[0]["prompt_tokens"], 11);
        assert_eq!(rows[0]["completion_tokens"], 2);
    }

    #[tokio::test]
    async fn a_non_stream_request_becomes_one_response_object() {
        let Some(f) = fixture().await else { return skip_without_db() };
        custom_gateway(&f).await;
        f.mock.respond_json(
            StatusCode::OK,
            json!({
                "id": "chatcmpl_1",
                "object": "chat.completion",
                "created": 1_700_000_000u64,
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{ "id": "call_1", "type": "function", "function": { "name": "multi_agent_v1__spawn_agent", "arguments": "{}" } }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": { "prompt_tokens": 5, "completion_tokens": 6 }
            }),
        );

        let response = f
            .router()
            .oneshot(f.post("/openai/v1/responses", codex_cli_body("mygw/local-model", json!({ "stream": false }))))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["object"], "response");
        assert_eq!(json["status"], "completed");
        let call = &json["output"][0];
        assert_eq!(call["type"], "function_call");
        assert_eq!(call["call_id"], "call_1");
        assert_eq!(call["name"], "spawn_agent");
        assert_eq!(call["namespace"], "multi_agent_v1");
        assert_eq!(call["arguments"], "{}");
        assert_eq!(json["usage"], json!({ "input_tokens": 5, "output_tokens": 6, "total_tokens": 11 }));
    }

    #[tokio::test]
    async fn previous_response_id_is_400_unsupported_field_with_no_upstream_call() {
        let Some(f) = fixture().await else { return skip_without_db() };
        custom_gateway(&f).await;

        let response = f
            .router()
            .oneshot(f.post(
                "/openai/v1/responses",
                codex_cli_body("mygw/local-model", json!({ "previous_response_id": "resp_old" })),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()["x-should-retry"], "false");
        let json = body_json(response).await;
        assert_eq!(json["error"]["code"], "unsupported_field");
        assert!(json["error"]["message"].as_str().unwrap().contains("previous_response_id"));
        assert_eq!(f.mock.requests().len(), 0);

        let rows = f.logs(1).await;
        assert_eq!(rows[0]["status_code"], 400);
        assert_eq!(rows[0]["error_code"], "unsupported_field");
    }

    #[tokio::test]
    async fn an_unresolvable_model_keeps_the_openai_envelope() {
        let Some(f) = fixture().await else { return skip_without_db() };

        let response = f
            .router()
            .oneshot(f.post("/openai/v1/responses", codex_cli_body("mygw/local-model", json!({ "stream": false }))))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await["error"]["code"], "invalid_model");

        // A group mount reports the missing endpoint as its own `group_not_found` code here.
        let response = f
            .router()
            .oneshot(f.post("/g/nope/openai/v1/responses", codex_cli_body("fast", json!({ "stream": false }))))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let json = body_json(response).await;
        assert_eq!(json["error"]["code"], "group_not_found");
        assert_eq!(json["error"]["message"], "unknown group endpoint \"nope\"");
        // Rows are written by spawned tasks, so match them by content rather than order.
        let rows = f.logs(2).await;
        let row = rows.iter().find(|r| r["error_code"] == "group_not_found").expect("a group_not_found row");
        assert_eq!(row["status_code"], 404);
    }

    #[tokio::test]
    async fn a_degenerate_tool_call_loop_is_rejected_on_the_responses_surface_too() {
        let Some(f) = fixture().await else { return skip_without_db() };
        custom_gateway(&f).await;
        let input: Vec<serde_json::Value> = (0..8)
            .flat_map(|i| {
                vec![
                    json!({ "type": "function_call", "call_id": format!("call_{i}"), "name": "Read", "arguments": "{\"p\":\"/a\"}" }),
                    json!({ "type": "function_call_output", "call_id": format!("call_{i}"), "output": "result" }),
                ]
            })
            .collect();

        let response = f
            .router()
            .oneshot(f.post(
                "/openai/v1/responses",
                json!({ "model": "mygw/local-model", "input": input, "stream": false }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()["x-should-retry"], "false");
        assert_eq!(body_json(response).await["error"]["code"], "loop_detected");
        assert_eq!(f.mock.requests().len(), 0);
        let rows = f.logs(1).await;
        assert_eq!(rows[0]["error_code"], "loop_detected");
        assert_eq!(rows[0]["model"], "mygw/local-model");
    }

    // ---- native codex path ----

    #[tokio::test]
    async fn the_native_path_forwards_the_cli_body_and_relays_the_upstream_sse_untouched() {
        let Some(f) = fixture().await else { return skip_without_db() };
        f.account("codex").await;
        f.mock.respond_sse(StatusCode::OK, RESPONSES_SSE);

        let response = f
            .router()
            .oneshot(f.post("/openai/v1/responses", codex_cli_body("codex/gpt-5.4", json!({}))))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let text = drain_sse(response).await;

        let calls = f.mock.requests();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].url, "https://chatgpt.com/backend-api/codex/responses");
        assert_eq!(calls[0].header("session_id"), Some("01a06ce4-68a8-7892-bcf5-b64013c7f279"));
        let sent = calls[0].json();
        assert_eq!(sent["model"], "gpt-5.4");
        assert_eq!(sent["store"], false);
        assert_eq!(sent["stream"], true);
        assert_eq!(sent["instructions"], "You are Codex.");
        // Hosted web_search, the namespace group and client_metadata ride through as sent.
        assert_eq!(sent["tools"], codex_cli_body("codex/gpt-5.4", json!({}))["tools"]);
        assert_eq!(sent["client_metadata"], json!({ "session_id": "01a06ce4-68a8-7892-bcf5-b64013c7f279" }));
        assert_eq!(sent["input"], codex_cli_body("codex/gpt-5.4", json!({}))["input"]);
        assert_eq!(sent["reasoning"], json!({ "summary": "auto" }));

        // Byte-for-byte relay: the upstream's own event lines, ids and sequence numbers.
        assert!(text.contains(RESPONSES_SSE.split("\n\n").next().unwrap()));
        assert!(text.contains("\"sequence_number\":2"));

        let rows = f.logs(1).await;
        assert_eq!(rows[0]["provider"], "codex");
        assert_eq!(rows[0]["model"], "codex/gpt-5.4");
        assert_eq!(rows[0]["prompt_tokens"], 7);
        assert_eq!(rows[0]["completion_tokens"], 1);
        assert_eq!(rows[0]["cache_read_input_tokens"], 3);
        assert_eq!(rows[0]["error_code"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn an_in_stream_error_frame_becomes_response_failed() {
        let Some(f) = fixture().await else { return skip_without_db() };
        f.account("codex").await;
        f.mock.respond_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": { "message": "backend exploded", "type": "server_error" } }),
        );

        let response = f
            .router()
            .oneshot(f.post("/openai/v1/responses", codex_cli_body("codex/gpt-5.4", json!({}))))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let events = sse_events(&drain_sse(response).await);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "response.failed");
        assert_eq!(
            events[0]["response"]["error"],
            json!({ "code": "upstream_error", "message": "backend exploded" })
        );
    }

    #[tokio::test]
    async fn a_native_non_stream_turn_is_collected_into_the_completed_object() {
        let Some(f) = fixture().await else { return skip_without_db() };
        f.account("codex").await;
        f.mock.respond_sse(StatusCode::OK, RESPONSES_SSE);

        let response = f
            .router()
            .oneshot(f.post("/openai/v1/responses", codex_cli_body("codex/gpt-5.4", json!({ "stream": false }))))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["id"], "resp_up");
        assert_eq!(json["status"], "completed");
        assert_eq!(
            json["usage"],
            json!({ "input_tokens": 7, "output_tokens": 1, "input_tokens_details": { "cached_tokens": 3 } })
        );
        let rows = f.logs(1).await;
        assert_eq!(rows[0]["prompt_tokens"], 7);
        assert_eq!(rows[0]["cache_read_input_tokens"], 3);
    }

    // ---- group mounts ----

    #[tokio::test]
    async fn an_all_codex_group_uses_the_native_path() {
        let Some(f) = fixture().await else { return skip_without_db() };
        f.account("codex").await;
        f.group("team", "coder", &["codex/gpt-5.4"]).await;
        f.mock.respond_sse(StatusCode::OK, RESPONSES_SSE);

        let response = f
            .router()
            .oneshot(f.post("/g/team/openai/v1/responses", codex_cli_body("coder", json!({}))))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        drain_sse(response).await;

        let calls = f.mock.requests();
        assert_eq!(calls[0].url, "https://chatgpt.com/backend-api/codex/responses");
        assert!(calls[0].json().get("client_metadata").is_some());
        let rows = f.logs(1).await;
        assert_eq!(rows[0]["model"], "codex/gpt-5.4");
        assert_eq!(rows[0]["group_name"], "team/coder");
    }

    #[tokio::test]
    async fn a_group_that_mixes_codex_with_another_provider_falls_back_to_conversion() {
        let Some(f) = fixture().await else { return skip_without_db() };
        f.account("codex").await;
        custom_gateway(&f).await;
        f.group("team", "mixed", &["codex/gpt-5.4", "mygw/local-model"]).await;
        f.mock.respond_sse(StatusCode::OK, RESPONSES_SSE);

        let response = f
            .router()
            .oneshot(f.post("/g/team/openai/v1/responses", codex_cli_body("mixed", json!({}))))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let text = drain_sse(response).await;

        // Codex still answered first, but through the Chat adapter: its body is the
        // proxy-built one, and the client gets the proxy's converted Responses events.
        let calls = f.mock.requests();
        assert_eq!(calls[0].url, "https://chatgpt.com/backend-api/codex/responses");
        let sent = calls[0].json();
        assert!(sent.get("client_metadata").is_none());
        let tool_names: Vec<&str> =
            sent["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(tool_names, ["exec_command", "multi_agent_v1__spawn_agent", "web_search"]);
        let events = sse_events(&text);
        assert_eq!(events[0]["type"], "response.created");
        assert_eq!(events.last().unwrap()["response"]["model"], "mixed");
    }
}
