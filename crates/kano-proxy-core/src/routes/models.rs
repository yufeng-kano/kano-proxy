//! `/api/models` — the admin Models page's catalog (apps/api/src/routes/models.ts,
//! docs/admin-ui.md § Models page).
//!
//! Models come from live upstream for the providers the user has bound; a provider with no
//! public list (Codex) simply contributes none. Nothing here is hard-coded: an unreachable
//! upstream reports its error and an empty list, never an invented model.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::auth::session::{is_secure_request, SessionUser};
use crate::catalog::models::{list_models_for_user, ListModelsOptions};
use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/", get(list))
}

/// The public LLM base host the page shows: the request's own origin (custom domain or local
/// server). The Worker read `new URL(c.req.url).origin`; an axum request carries the authority
/// in `Host` instead, and a request with neither falls back to `APP_URL`.
fn public_api_origin(state: &AppState, parts: &Parts) -> String {
    if let (Some(scheme), Some(authority)) = (parts.uri.scheme_str(), parts.uri.authority()) {
        return format!("{scheme}://{authority}");
    }
    let host = parts
        .headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty());
    match host {
        Some(host) => {
            let scheme = if is_secure_request(parts) { "https" } else { "http" };
            format!("{scheme}://{host}")
        }
        None => state.config().app_url.trim_end_matches('/').to_string(),
    }
}

async fn list(
    State(state): State<AppState>,
    session: SessionUser,
    Query(params): Query<HashMap<String, String>>,
    request: axum::extract::Request,
) -> Response {
    let (parts, _) = request.into_parts();
    let opts = ListModelsOptions {
        // `?refresh=true` bypasses the one-hour catalog cache.
        force: params.get("refresh").map(String::as_str) == Some("true"),
        // `?available=1` is the API-shape filter; every returned model is available, so it
        // narrows nothing today (catalog::models::ListModelsOptions).
        available_only: params.get("available").map(String::as_str) == Some("1"),
    };
    let catalog = list_models_for_user(&state, &session.user.id, opts).await;
    let origin = public_api_origin(&state, &parts);
    let providers: Vec<Value> = catalog
        .providers
        .iter()
        .map(|p| json!({ "provider": p.provider, "count": p.models.len(), "error": p.error, "cached": p.cached }))
        .collect();
    Json(json!({
        "object": "list",
        "data": catalog.models,
        "providers": providers,
        "openai_base": format!("{origin}/openai/v1"),
        "anthropic_base": format!("{origin}/anthropic"),
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::session::create_session;
    use crate::db::test_support::{insert_user, skip_without_db, test_pool, test_router, test_state, TEST_APP_URL};
    use crate::upstream::{MockTransport, UpstreamResponse};
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn body_json(response: Response) -> Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// A transport that answers every upstream attempt (the read-time price fetch) with a 500,
    /// so no test ever reaches the real network. Enough handlers are queued for the price
    /// refresh both sources make on every admin request in the test.
    fn offline() -> std::sync::Arc<MockTransport> {
        let mock = MockTransport::new();
        for _ in 0..64 {
            mock.expect(|_| {
                Ok(UpstreamResponse::from_bytes(StatusCode::INTERNAL_SERVER_ERROR, http::HeaderMap::new(), "offline"))
            });
        }
        mock
    }

    async fn signed_in(state: &AppState, email: &str) -> (String, String) {
        let user = insert_user(state.pool(), email).await;
        let (_, cookie) = create_session(state, &user.id, false).await.unwrap();
        (user.id, cookie.split(';').next().unwrap().to_string())
    }

    #[tokio::test]
    async fn requires_a_session() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let response =
            test_router(state).oneshot(Request::builder().uri("/api/models").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(response).await, json!({ "error": "unauthorized" }));
    }

    #[tokio::test]
    async fn an_unbound_user_gets_an_empty_catalog_and_the_public_bases() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let (_, cookie) = signed_in(&state, "models@example.com").await;
        let response = test_router(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/models")
                    .header(header::COOKIE, &cookie)
                    .header(header::HOST, "api.example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["object"], "list");
        // Nothing is bound, so nothing is listed — an empty catalog, never a fabricated one.
        assert_eq!(json["data"], json!([]));
        assert!(json["providers"].as_array().unwrap().iter().all(|p| p["count"] == 0));
        assert_eq!(json["openai_base"], "http://api.example.com/openai/v1");
        assert_eq!(json["anthropic_base"], "http://api.example.com/anthropic");
    }

    #[tokio::test]
    async fn a_cli_providers_reported_models_are_listed_without_any_fetch() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let mock = MockTransport::new();
        let state = test_state(pool, mock.clone());
        let (user_id, cookie) = signed_in(&state, "cli-models@example.com").await;
        sqlx::query(
            "INSERT INTO cli_providers (id, user_id, device_id, slug, name, format, models_json, sort_order, created_at, updated_at)
             VALUES ('cliprov_1', $1, NULL, 'my-mac', 'My Mac', 'openai', $2, 0, $3, $3)",
        )
        .bind(&user_id)
        .bind(r#"["llama3"]"#)
        .bind(crate::ids::now_iso())
        .execute(state.pool())
        .await
        .unwrap();

        let response = test_router(state)
            .oneshot(Request::builder().uri("/api/models").header(header::COOKIE, &cookie).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let json = body_json(response).await;
        let ids: Vec<&str> = json["data"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["my-mac/llama3"], "the stored agent report is the catalog, with no fetch");
        assert!(mock.requests().is_empty());
    }

    #[tokio::test]
    async fn model_groups_are_absent_from_the_shared_catalog() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let (user_id, cookie) = signed_in(&state, "groups-catalog@example.com").await;
        let group = crate::db::model_groups::insert_model_group(state.pool(), &user_id, "opus", "team", None).await.unwrap();
        crate::db::model_groups::replace_group_models(
            state.pool(),
            &user_id,
            &group.id,
            &[crate::db::model_groups::GroupModelInput {
                name: "opus".into(),
                targets: vec![crate::db::model_groups::GroupTarget {
                    model: "claude-code/claude-opus-5".into(),
                    account_id: None,
                }],
            }],
        )
        .await
        .unwrap();

        let response = test_router(state)
            .oneshot(Request::builder().uri("/api/models").header(header::COOKIE, &cookie).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let json = body_json(response).await;
        let models = json["data"].as_array().unwrap();
        // Since v4 each group is its own endpoint whose /models lists its names; the shared
        // catalog carries no group section and never expands a group into its targets.
        assert!(models.iter().all(|m| m["id"] != "opus"));
        assert!(models.iter().all(|m| m["provider"] != "group"));
        assert!(json["providers"].as_array().unwrap().iter().all(|p| p["provider"] != "group"));
        assert!(models.iter().all(|m| m["id"] != "claude-code/claude-opus-5"));
    }

    #[tokio::test]
    async fn the_origin_falls_back_to_app_url_without_a_host_header() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let (_, cookie) = signed_in(&state, "origin@example.com").await;
        let response = test_router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/models")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let json = body_json(response).await;
        assert_eq!(json["openai_base"], format!("{TEST_APP_URL}/openai/v1"));
    }
}
