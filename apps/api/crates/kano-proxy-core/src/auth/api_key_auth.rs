//! Project API-key authentication for every model-request surface
//! (docs/auth.md). Clients present `Authorization: Bearer
//! <key>` or `x-api-key`; keys are stored hashed, so the lookup is by hex SHA-256 and the
//! plaintext is never compared or logged.

use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::crypto::keys::{extract_bearer, hash_api_key};
use crate::db::keys::{find_key_by_hash, touch_key, ApiKeyRow};
use crate::db::request_logs::{insert_request_log, RequestLogEntry};
use crate::extensions::ApiKeyIdentity;
use crate::http::{ApiError, Surface};
use crate::AppState;

use super::spend_limit::{key_window_spend_cached, SpendKey};

/// The error envelope is surface-shaped, not provider-shaped: an Anthropic client on
/// `/anthropic` (shared base or a group endpoint's anthropic mount) expects the Anthropic
/// shape even for an auth failure.
fn unauthorized(surface: Surface, message: &str) -> Response {
    match surface {
        Surface::OpenAI => ApiError::openai(StatusCode::UNAUTHORIZED, message, "invalid_request_error", "invalid_api_key"),
        Surface::Anthropic => ApiError::anthropic(StatusCode::UNAUTHORIZED, "authentication_error", message),
    }
    .into_response()
}

fn spend_limit_reached(surface: Surface, message: &str) -> Response {
    let error = match surface {
        Surface::OpenAI => ApiError::openai(StatusCode::TOO_MANY_REQUESTS, message, "rate_limit_error", "spend_limit_exceeded"),
        Surface::Anthropic => ApiError::anthropic(StatusCode::TOO_MANY_REQUESTS, "rate_limit_error", message),
    };
    error.header(axum::http::header::HeaderName::from_static("x-should-retry"), HeaderValue::from_static("false")).into_response()
}

/// Resolves the `sk-kano-proxy-` key, gates it on its spend limit, then hands the request to
/// the edition's request policy (when one is installed) or straight to the route.
pub async fn api_key_auth(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let surface = Surface::from_path(req.uri().path());
    let bearer = extract_bearer(req.headers().get(axum::http::header::AUTHORIZATION).and_then(|v| v.to_str().ok()));
    let x_api_key = req.headers().get("x-api-key").and_then(|v| v.to_str().ok()).map(str::to_string);
    let Some(raw) = bearer.or(x_api_key) else {
        return unauthorized(surface, "Missing API key");
    };
    let Ok(Some(row)) = find_key_by_hash(state.pool(), &hash_api_key(&raw)).await else {
        return unauthorized(surface, "Invalid API key");
    };

    if let Some(response) = spend_gate(&state, &row, req.method(), surface).await {
        return response;
    }

    req.extensions_mut().insert(ApiKeyIdentity { user_id: row.user_id.clone(), api_key_id: row.id.clone() });
    let pool = state.pool().clone();
    let key_id = row.id.clone();
    tokio::spawn(async move {
        if let Err(err) = touch_key(&pool, &key_id).await {
            tracing::warn!(error = %err, "last_used_at not stamped");
        }
    });

    match state.request_policy().cloned() {
        Some(policy) => policy.handle(state.clone(), req, next).await,
        None => next.run(req).await,
    }
}

/// Spend-limit gate (docs/pricing.md): POST surfaces only — `GET /models` stays free so a
/// blocked client can still see its catalog. An unknown window sum is a storage failure and
/// fails open; a real at-or-over read fails closed.
async fn spend_gate(state: &AppState, row: &ApiKeyRow, method: &Method, surface: Surface) -> Option<Response> {
    let limit = row.spend_limit?;
    if method != Method::POST {
        return None;
    }
    let spend = key_window_spend_cached(state.pool(), &SpendKey::from(row), state.now_ms()).await?;
    if spend < limit {
        return None;
    }
    let message = format!("Spend limit reached for this API key (${limit} per {})", row.spend_limit_interval);
    let entry = RequestLogEntry {
        user_id: row.user_id.clone(),
        api_key_id: Some(row.id.clone()),
        provider: "unknown".into(),
        model: String::new(),
        status_code: 429,
        latency_ms: 0,
        error_code: Some("spend_limit_exceeded".into()),
        api_key_name: Some(row.name.clone()),
        started_at: crate::ids::now_iso(),
        ..Default::default()
    };
    let pool = state.pool().clone();
    tokio::spawn(async move {
        if let Err(err) = insert_request_log(&pool, &entry).await {
            tracing::warn!(error = %err, "spend-limit refusal not logged");
        }
    });
    Some(spend_limit_reached(surface, &message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::keys::SpendLimitFields;
    use crate::db::test_support::{insert_api_key, insert_api_key_with_limit, insert_user, skip_without_db, test_pool, test_state};
    use crate::upstream::MockTransport;
    use axum::body::Body;
    use axum::routing::{get, post};
    use axum::Router;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// A router shaped like the model surfaces: the middleware in front, a handler that
    /// reports the identity it was given.
    fn router(state: AppState) -> Router {
        async fn echo(req: Request) -> Response {
            let id = req.extensions().get::<ApiKeyIdentity>().cloned();
            axum::Json(serde_json::json!({
                "owner": id.as_ref().map(|i| i.user_id.clone()),
                "key": id.map(|i| i.api_key_id),
            }))
            .into_response()
        }
        Router::new()
            .route("/openai/v1/chat/completions", post(echo))
            .route("/openai/v1/models", get(echo))
            .route("/anthropic/v1/messages", post(echo))
            .route_layer(axum::middleware::from_fn_with_state(state.clone(), api_key_auth))
            .with_state(state)
    }

    async fn body_json(response: Response) -> serde_json::Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn post_request(path: &str, header: (&str, String)) -> Request<Body> {
        Request::builder().method("POST").uri(path).header(header.0, header.1).body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn a_valid_key_reaches_the_route_as_an_identity() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "auth@example.com").await;
        let (key, plaintext) = insert_api_key(state.pool(), &user.id).await;

        let response = router(state.clone())
            .oneshot(post_request("/openai/v1/chat/completions", ("authorization", format!("Bearer {plaintext}"))))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await, serde_json::json!({ "owner": user.id, "key": key.id }));

        // x-api-key is accepted on the Anthropic surface exactly the same way.
        let response = router(state.clone())
            .oneshot(post_request("/anthropic/v1/messages", ("x-api-key", plaintext.clone())))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_missing_or_unknown_key_is_401_in_the_surface_shape() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());

        let response = router(state.clone())
            .oneshot(Request::builder().method("POST").uri("/openai/v1/chat/completions").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            body_json(response).await,
            serde_json::json!({ "error": { "message": "Missing API key", "type": "invalid_request_error", "code": "invalid_api_key" } })
        );

        let response = router(state.clone())
            .oneshot(post_request("/openai/v1/chat/completions", ("authorization", "Bearer sk-kano-proxy-nope".into())))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(response).await["error"]["message"], "Invalid API key");

        let response = router(state)
            .oneshot(Request::builder().method("POST").uri("/anthropic/v1/messages").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            body_json(response).await,
            serde_json::json!({ "type": "error", "error": { "type": "authentication_error", "message": "Missing API key" } })
        );
    }

    async fn seed_spend(state: &AppState, user_id: &str, key_id: &str, cost: f64) {
        insert_request_log(
            state.pool(),
            &RequestLogEntry {
                user_id: user_id.into(),
                api_key_id: Some(key_id.into()),
                provider: "claude-code".into(),
                model: "m".into(),
                status_code: 200,
                latency_ms: 1,
                cost: Some(cost),
                started_at: crate::ids::now_iso(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    fn limits(limit: f64) -> SpendLimitFields {
        SpendLimitFields { spend_limit: Some(limit), spend_limit_interval: "monthly".into(), spend_limit_include_oauth: true }
    }

    #[tokio::test]
    async fn a_post_at_or_over_the_limit_is_429_and_logged_once() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "limit@example.com").await;
        let (key, plaintext) = insert_api_key_with_limit(state.pool(), &user.id, limits(2.0)).await;
        seed_spend(&state, &user.id, &key.id, 2.5).await;

        let response = router(state.clone())
            .oneshot(post_request("/openai/v1/chat/completions", ("authorization", format!("Bearer {plaintext}"))))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers().get("x-should-retry").unwrap(), "false");
        let json = body_json(response).await;
        assert_eq!(json["error"]["code"], "spend_limit_exceeded");
        assert_eq!(json["error"]["type"], "rate_limit_error");
        assert_eq!(json["error"]["message"], "Spend limit reached for this API key ($2 per monthly)");

        // The refusal is recorded as exactly one row, with no cost attached.
        for _ in 0..50 {
            let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_logs WHERE error_code = 'spend_limit_exceeded'")
                .fetch_one(state.pool())
                .await
                .unwrap();
            if rows == 1 {
                let cost: Option<f64> =
                    sqlx::query_scalar("SELECT cost FROM request_logs WHERE error_code = 'spend_limit_exceeded'")
                        .fetch_one(state.pool())
                        .await
                        .unwrap();
                assert_eq!(cost, None);
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("the spend-limit refusal was never logged");
    }

    #[tokio::test]
    async fn the_gate_is_post_only_and_never_fires_without_a_limit() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "gate@example.com").await;
        let (limited, limited_key) = insert_api_key_with_limit(state.pool(), &user.id, limits(1.0)).await;
        seed_spend(&state, &user.id, &limited.id, 5.0).await;
        let (unlimited, unlimited_key) = insert_api_key(state.pool(), &user.id).await;
        seed_spend(&state, &user.id, &unlimited.id, 10_000.0).await;

        // GET stays free even when the key is over its limit.
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/openai/v1/models")
                    .header("authorization", format!("Bearer {limited_key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // A key with no limit is never summed, whatever it spent.
        let response = router(state)
            .oneshot(post_request("/openai/v1/chat/completions", ("authorization", format!("Bearer {unlimited_key}"))))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn an_unreadable_window_sum_fails_open() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "open@example.com").await;
        let (key, plaintext) = insert_api_key_with_limit(state.pool(), &user.id, limits(1.0)).await;
        seed_spend(&state, &user.id, &key.id, 5.0).await;
        sqlx::query("ALTER TABLE request_logs RENAME TO request_logs_hidden").execute(state.pool()).await.unwrap();

        let response = router(state.clone())
            .oneshot(post_request("/openai/v1/chat/completions", ("authorization", format!("Bearer {plaintext}"))))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "an infrastructure hiccup must not block the proxy");
        sqlx::query("ALTER TABLE request_logs_hidden RENAME TO request_logs").execute(state.pool()).await.unwrap();
    }
}
