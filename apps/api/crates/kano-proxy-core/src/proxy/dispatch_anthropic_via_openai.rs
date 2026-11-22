//! Anthropic requests served by an OpenAI-shaped upstream (docs/api.md § Anthropic
//! ingress for non-Claude providers).
//!
//! Anthropic Messages ingress for providers that speak OpenAI: convert → the OpenAI chat path
//! ([`dispatch_chat_completions`], including its candidate walk) → convert the response back.
//! cache_control is stripped on convert; Grok affinity ids are never invented here (client
//! headers travel through `req.affinity` only).

use std::time::Duration;

use axum::body::Body;
use axum::response::Response;
use http::{header, StatusCode};
use serde_json::{json, Map, Value};

use crate::pool::extension::HeaderAudience;
use crate::providers::types::{AffinityIds, ChatCompletionRequest};
use crate::proxy::dispatch::{
    body_stream, dispatch_chat_completions, is_event_stream, json_response, passthrough_stream_headers,
    safe_response_text, ChatDispatchOptions,
};
use crate::proxy::openai_anthropic::{
    anthropic_to_openai_chat_request, openai_sse_to_anthropic_stream, openai_to_anthropic_message,
    prompt_cache_key_from_anthropic_metadata, AnthropicToOpenAiChat,
};
use crate::proxy::sse::{stream_with_keepalive, StreamKeepaliveOpts, DEFAULT_KEEPALIVE_INTERVAL};
use crate::routing::types::RoutingCandidate;
use crate::utils::reasoning::parse_reasoning_effort;
use crate::AppState;

/// Options for [`dispatch_anthropic_via_openai`].
#[derive(Default)]
pub struct ViaOpenAiDispatchOptions {
    pub user_id: String,
    pub api_key_id: Option<String>,
    /// The primary target's builtin `ProviderId` or custom provider slug.
    pub provider: String,
    pub raw_model: String,
    pub upstream_model: String,
    pub body: Map<String, Value>,
    pub affinity: Option<AffinityIds>,
    pub idle_timeout: Option<Duration>,
    pub group_name: Option<String>,
    /// The route's resolved candidates, possibly cross-target for a group.
    pub candidates: Vec<RoutingCandidate>,
}

/// The converted request as an OpenAI Chat Completions body — what the custom-openai
/// passthrough adapter forwards verbatim, and a no-op for built-ins, which build their own body
/// from the named fields.
fn converted_raw_body(converted: &AnthropicToOpenAiChat) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("messages".into(), Value::Array(converted.messages.clone()));
    out.insert("stream".into(), Value::Bool(converted.stream));
    if let Some(v) = converted.max_tokens {
        out.insert("max_tokens".into(), json!(v));
    }
    if let Some(v) = converted.temperature {
        out.insert("temperature".into(), json!(v));
    }
    if let Some(v) = converted.top_p {
        out.insert("top_p".into(), json!(v));
    }
    if let Some(v) = &converted.stop {
        out.insert("stop".into(), json!(v));
    }
    if let Some(v) = &converted.tools {
        out.insert("tools".into(), v.clone());
    }
    if let Some(v) = &converted.tool_choice {
        out.insert("tool_choice".into(), v.clone());
    }
    if let Some(v) = &converted.response_format {
        out.insert("response_format".into(), v.clone());
    }
    if let Some(v) = &converted.reasoning_effort {
        out.insert("reasoning_effort".into(), v.clone());
    }
    out
}

pub async fn dispatch_anthropic_via_openai(cx: &AppState, opts: ViaOpenAiDispatchOptions) -> Response {
    let ViaOpenAiDispatchOptions {
        user_id,
        api_key_id,
        provider,
        raw_model,
        upstream_model,
        body,
        affinity,
        idle_timeout,
        group_name,
        candidates,
    } = opts;
    let converted = anthropic_to_openai_chat_request(&body);
    let effort = parse_reasoning_effort(converted.reasoning_effort.as_ref());
    if effort.is_invalid() {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({
                "type": "error",
                "error": { "type": "invalid_request_error", "message": "invalid reasoning_effort" }
            }),
        );
    }

    let raw_body = converted_raw_body(&converted);
    // The raw body holds its own copy; the named fields take the converted values.
    let AnthropicToOpenAiChat {
        messages,
        max_tokens,
        stream,
        tools,
        tool_choice,
        response_format,
        reasoning_effort: _,
        temperature,
        top_p,
        stop,
    } = converted;
    let openai_res = dispatch_chat_completions(
        cx,
        ChatDispatchOptions {
            user_id,
            api_key_id,
            provider,
            idle_timeout,
            group_name,
            candidates,
            attach_cost: false,
            req: ChatCompletionRequest {
                raw_model: raw_model.clone(),
                upstream_model,
                messages,
                stream: Some(stream).filter(|s| *s),
                max_tokens,
                tools,
                tool_choice,
                response_format,
                reasoning_effort: effort.effort(),
                temperature,
                top_p,
                stop,
                // Named field only — never added to the converted raw body, so the
                // custom-openai passthrough body carries only what the client converted.
                prompt_cache_key: prompt_cache_key_from_anthropic_metadata(&body),
                affinity,
                responses_body: None,
                raw_body,
            },
        },
    )
    .await;

    let ok = openai_res.status().is_success();
    let status = openai_res.status();
    let event_stream = is_event_stream(&openai_res);
    let (parts, body) = openai_res.into_parts();

    // Stream: OpenAI SSE → Anthropic SSE.
    if stream || event_stream {
        if !ok {
            let text = safe_response_text(body).await;
            return anthropic_error_from_openai_text(&text, status);
        }
        // Keepalive again on the converted stream: the upstream wrapper's comments are consumed
        // by the converter, and no Anthropic event is emitted until the first token — a long
        // reasoning turn would otherwise send zero bytes. With eager commit,
        // `dispatch_chat_completions` already returned 200 + SSE;
        // `openai_sse_to_anthropic_stream` converts OpenAI error lines to Anthropic error
        // events.
        let converted_stream = openai_sse_to_anthropic_stream(Box::pin(body_stream(body)), &raw_model);
        let stream =
            stream_with_keepalive(converted_stream, DEFAULT_KEEPALIVE_INTERVAL, StreamKeepaliveOpts::default());
        let mut res = Response::builder().status(status).body(Body::from_stream(stream)).expect("builds");
        *res.headers_mut() = passthrough_stream_headers(&parts.headers, HeaderAudience::Own);
        return res;
    }

    let text = safe_response_text(body).await;
    if !ok {
        return anthropic_error_from_openai_text(&text, status);
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(json)) => {
            let message = openai_to_anthropic_message(&json, &raw_model);
            json_response(StatusCode::OK, &Value::Object(message))
        }
        _ => {
            let content_type = parts
                .headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .filter(|v| !v.is_empty())
                .unwrap_or("application/json")
                .to_string();
            Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(text))
                .expect("builds")
        }
    }
}

fn anthropic_error_from_openai_text(text: &str, status: StatusCode) -> Response {
    let parsed = serde_json::from_str::<Value>(text).ok();
    let error = parsed.as_ref().and_then(|j| j.get("error"));
    let error_type = error
        .and_then(|e| e.get("type"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("api_error");
    let message = error
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(if text.is_empty() { "upstream error" } else { text });
    json_response(status, &json!({ "type": "error", "error": { "type": error_type, "message": message } }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{insert_user, skip_without_db, test_pool, test_state};
    use crate::pool::AcquiredAccount;
    use crate::providers::types::{AdapterError, CallExtras, DynAdapter, ProviderAdapter};
    use crate::proxy::dispatch::tests::{body_text, own_pool, seed_accounts};
    use crate::upstream::MockTransport;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    /// Captures exactly what the chat path handed the adapter.
    struct Capturing {
        seen: Arc<Mutex<Option<ChatCompletionRequest>>>,
    }

    #[async_trait]
    impl ProviderAdapter for Capturing {
        fn id(&self) -> &str {
            "custom-openai-test"
        }
        async fn chat_completions(
            &self,
            _: &AppState,
            _: &AcquiredAccount,
            req: &ChatCompletionRequest,
            _: &CallExtras,
        ) -> Result<Response, AdapterError> {
            *self.seen.lock().unwrap() = Some(req.clone());
            Ok(json_response(
                StatusCode::OK,
                &json!({ "choices": [{ "message": { "role": "assistant", "content": "hi" }, "finish_reason": "stop" }] }),
            ))
        }
    }

    async fn dispatch_with(body: Value) -> Option<(ChatCompletionRequest, Response)> {
        let pool = test_pool().await?;
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), &format!("via-{}@example.com", crate::ids::new_id(""))).await;
        seed_accounts(state.pool(), &user.id, "custom-openai-test", &["acc_custom"]).await;
        let seen = Arc::new(Mutex::new(None));
        let adapter: DynAdapter = Arc::new(Capturing { seen: seen.clone() });
        let res = dispatch_anthropic_via_openai(
            &state,
            ViaOpenAiDispatchOptions {
                user_id: user.id.clone(),
                provider: "custom-openai-test".into(),
                candidates: own_pool(&state, &user.id, "custom-openai-test", "some-model", adapter).await,
                raw_model: "custom-openai-test/some-model".into(),
                upstream_model: "some-model".into(),
                body: body.as_object().unwrap().clone(),
                ..Default::default()
            },
        )
        .await;
        let captured = seen.lock().unwrap().clone().expect("the adapter was called");
        Some((captured, res))
    }

    #[tokio::test]
    async fn sampling_fields_land_in_both_the_named_fields_and_the_converted_raw_body() {
        let Some((captured, _res)) = dispatch_with(json!({
            "model": "some-model",
            "max_tokens": 10,
            "temperature": 0.42,
            "top_p": 0.77,
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .await
        else {
            return skip_without_db();
        };
        assert_eq!(captured.temperature, Some(0.42));
        assert_eq!(captured.top_p, Some(0.77));
        assert_eq!(captured.raw_body.get("temperature"), Some(&json!(0.42)));
        assert_eq!(captured.raw_body.get("top_p"), Some(&json!(0.77)));
    }

    #[tokio::test]
    async fn metadata_user_id_lands_in_prompt_cache_key_but_never_in_the_converted_raw_body() {
        let session = "user_ab12_account_11111111-2222-3333-4444-555555555555_session_0e35a1af-fe45-49c8-b0cc-fb1c58b1b06e";
        let Some((captured, _)) = dispatch_with(json!({
            "model": "some-model",
            "max_tokens": 10,
            "metadata": { "user_id": session },
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .await
        else {
            return skip_without_db();
        };
        assert!(captured.prompt_cache_key.is_some());
        assert!(!captured.raw_body.contains_key("prompt_cache_key"));
    }

    #[tokio::test]
    async fn without_metadata_the_prompt_cache_key_stays_unset() {
        let Some((captured, _)) = dispatch_with(json!({
            "model": "some-model",
            "max_tokens": 10,
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .await
        else {
            return skip_without_db();
        };
        assert_eq!(captured.prompt_cache_key, None);
    }

    #[tokio::test]
    async fn a_successful_completion_comes_back_as_an_anthropic_message() {
        let Some((_, res)) = dispatch_with(json!({
            "model": "some-model",
            "max_tokens": 10,
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .await
        else {
            return skip_without_db();
        };
        assert_eq!(res.status(), StatusCode::OK);
        let body: Value = serde_json::from_str(&body_text(res).await).unwrap();
        assert_eq!(body["type"], "message");
        assert_eq!(body["model"], "custom-openai-test/some-model");
    }

    #[tokio::test]
    async fn an_invalid_reasoning_effort_is_rejected_before_the_pool_is_touched() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let res = dispatch_anthropic_via_openai(
            &state,
            ViaOpenAiDispatchOptions {
                user_id: "user_never".into(),
                provider: "custom-openai-test".into(),
                raw_model: "custom-openai-test/some-model".into(),
                upstream_model: "some-model".into(),
                body: json!({ "model": "some-model", "max_tokens": 10, "messages": [], "reasoning_effort": 7 })
                    .as_object()
                    .unwrap()
                    .clone(),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let body: Value = serde_json::from_str(&body_text(res).await).unwrap();
        assert_eq!(body["error"]["message"], "invalid reasoning_effort");
    }

    #[test]
    fn an_openai_error_body_maps_into_the_anthropic_envelope() {
        let res = anthropic_error_from_openai_text(
            r#"{"error":{"message":"limited","type":"rate_limit_error"}}"#,
            StatusCode::TOO_MANY_REQUESTS,
        );
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        let res = anthropic_error_from_openai_text("", StatusCode::BAD_GATEWAY);
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    }
}
