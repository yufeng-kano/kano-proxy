//! `/api/keys` — the admin's project API keys (apps/api/src/routes/keys.ts, docs/auth.md,
//! docs/pricing.md). The plaintext key is returned exactly once, at creation; every later read
//! shows only the stored display prefix, never the hash.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde_json::{json, Map, Value};

use crate::auth::session::SessionUser;
use crate::auth::spend_limit::{is_spend_limit_interval, key_window_spend, SpendKey};
use crate::db::keys::{create_key, delete_key, list_keys, update_key, ApiKeyRow, SpendLimitFields};
use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list).post(create))
        .route("/{id}", patch(update).delete(remove))
}

fn error(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "error": code }))).into_response()
}

/// Validated limit fields off a request body. `Ok(None)` means the body did not speak to
/// limits at all; an explicit `spend_limit: null` clears the limit. `Err(())` is a bad shape.
#[allow(clippy::result_unit_err)]
pub fn parse_limit_fields(body: &Map<String, Value>) -> Result<Option<SpendLimitFields>, ()> {
    let has_any = body.contains_key("spend_limit")
        || body.contains_key("spend_limit_interval")
        || body.contains_key("spend_limit_include_oauth");
    if !has_any {
        return Ok(None);
    }
    let spend_limit = match body.get("spend_limit") {
        None | Some(Value::Null) => None,
        // A limit is a positive, finite number of dollars; zero would mean "blocked", which is
        // what deleting the key is for.
        Some(Value::Number(n)) => match n.as_f64() {
            Some(v) if v.is_finite() && v > 0.0 => Some(v),
            _ => return Err(()),
        },
        Some(_) => return Err(()),
    };
    let interval = match body.get("spend_limit_interval") {
        None => "monthly".to_string(),
        Some(Value::String(s)) if is_spend_limit_interval(s) => s.clone(),
        Some(_) => return Err(()),
    };
    let include_oauth = match body.get("spend_limit_include_oauth") {
        None => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(()),
    };
    Ok(Some(SpendLimitFields { spend_limit, spend_limit_interval: interval, spend_limit_include_oauth: include_oauth }))
}

/// One key's wire shape — limit fields always present; `window_spend` is added by the list
/// route. `key_hash` never appears.
fn key_json(k: &ApiKeyRow) -> Value {
    json!({
        "id": k.id,
        "name": k.name,
        "key_prefix": k.key_prefix,
        "created_at": k.created_at,
        "last_used_at": k.last_used_at,
        "spend_limit": k.spend_limit,
        "spend_limit_interval": k.spend_limit_interval,
        "spend_limit_include_oauth": k.spend_limit_include_oauth == 1,
    })
}

async fn list(State(state): State<AppState>, session: SessionUser) -> Response {
    let Ok(keys) = list_keys(state.pool(), &session.user.id).await else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error");
    };
    let mut out = Vec::with_capacity(keys.len());
    for key in &keys {
        // Uncached window sums: the admin list is the surface where "how much has this key
        // spent" must be current, and it is a handful of keys at most.
        let spend = key_window_spend(state.pool(), &SpendKey::from(key), state.now_ms()).await;
        let mut value = key_json(key);
        value["window_spend"] = spend.map(Value::from).unwrap_or(Value::Null);
        out.push(value);
    }
    Json(json!({ "keys": out })).into_response()
}

/// A body that is absent or unparsable reads as `{}`, exactly as the TypeScript's
/// `c.req.json().catch(() => ({}))` did.
fn object_body(body: Option<Json<Value>>) -> Map<String, Value> {
    match body {
        Some(Json(Value::Object(map))) => map,
        _ => Map::new(),
    }
}

async fn create(State(state): State<AppState>, session: SessionUser, body: Option<Json<Value>>) -> Response {
    let body = object_body(body);
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("default")
        .to_string();
    let Ok(limits) = parse_limit_fields(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid_spend_limit");
    };
    match create_key(state.pool(), &session.user.id, &name, limits).await {
        Ok(created) => {
            let mut value = key_json(&created.row);
            // The plaintext is shown once and never stored.
            value["key"] = Value::String(created.plaintext);
            Json(value).into_response()
        }
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
    }
}

async fn update(
    State(state): State<AppState>,
    session: SessionUser,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let body = object_body(body);
    let name = match body.get("name") {
        None => None,
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Some(_) => return error(StatusCode::BAD_REQUEST, "invalid_name"),
    };
    let Ok(limits) = parse_limit_fields(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid_spend_limit");
    };
    match update_key(state.pool(), &session.user.id, &id, name.as_deref(), limits).await {
        Ok(false) => error(StatusCode::NOT_FOUND, "not found"),
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
        Ok(true) => match list_keys(state.pool(), &session.user.id).await {
            Ok(keys) => match keys.iter().find(|k| k.id == id) {
                Some(updated) => Json(key_json(updated)).into_response(),
                None => Json(json!({ "ok": true })).into_response(),
            },
            Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
        },
    }
}

async fn remove(State(state): State<AppState>, session: SessionUser, Path(id): Path<String>) -> Response {
    match delete_key(state.pool(), &session.user.id, &id).await {
        Ok(true) => Json(json!({ "ok": true })).into_response(),
        Ok(false) => error(StatusCode::NOT_FOUND, "not found"),
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::session::create_session;
    use crate::db::request_logs::{insert_request_log, RequestLogEntry};
    use crate::db::test_support::{insert_user, skip_without_db, test_pool, test_router, test_state};
    use crate::upstream::MockTransport;
    use axum::body::Body;
    use axum::http::{header, Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn body_json(response: Response) -> Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn signed_in(state: &AppState, email: &str) -> String {
        let user = insert_user(state.pool(), email).await;
        let (_, cookie) = create_session(state, &user.id, false).await.unwrap();
        cookie.split(';').next().unwrap().to_string()
    }

    fn json_request(method: &str, uri: &str, cookie: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::COOKIE, cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, "https://app.example.com")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[test]
    fn limit_fields_are_validated() {
        let parse = |v: Value| parse_limit_fields(v.as_object().unwrap());
        assert_eq!(parse(json!({})), Ok(None), "a body that says nothing about limits");
        assert_eq!(
            parse(json!({ "spend_limit": null })),
            Ok(Some(SpendLimitFields { spend_limit: None, spend_limit_interval: "monthly".into(), spend_limit_include_oauth: true })),
            "an explicit null clears the limit"
        );
        assert_eq!(
            parse(json!({ "spend_limit": 25, "spend_limit_interval": "weekly", "spend_limit_include_oauth": false })),
            Ok(Some(SpendLimitFields { spend_limit: Some(25.0), spend_limit_interval: "weekly".into(), spend_limit_include_oauth: false }))
        );
        // A non-positive, non-numeric or unknown-interval limit is refused rather than coerced.
        for bad in [
            json!({ "spend_limit": 0 }),
            json!({ "spend_limit": -3 }),
            json!({ "spend_limit": "ten" }),
            json!({ "spend_limit": 5, "spend_limit_interval": "hourly" }),
            json!({ "spend_limit": 5, "spend_limit_include_oauth": "yes" }),
        ] {
            assert_eq!(parse(bad.clone()), Err(()), "{bad} should be invalid");
        }
        // Naming only the interval still counts as speaking to limits.
        assert!(matches!(parse(json!({ "spend_limit_interval": "daily" })), Ok(Some(_))));
    }

    #[tokio::test]
    async fn every_route_requires_a_session() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        for (method, uri) in [("GET", "/api/keys"), ("POST", "/api/keys"), ("PATCH", "/api/keys/key_1"), ("DELETE", "/api/keys/key_1")] {
            let response = test_router(state.clone())
                .oneshot(Request::builder().method(method).uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{method} {uri}");
            assert_eq!(body_json(response).await, json!({ "error": "unauthorized" }));
        }
    }

    #[tokio::test]
    async fn create_returns_the_plaintext_once_and_echoes_the_limits() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let cookie = signed_in(&state, "keys@example.com").await;

        let response = test_router(state.clone())
            .oneshot(json_request(
                "POST",
                "/api/keys",
                &cookie,
                json!({ "name": "ci key", "spend_limit": 25, "spend_limit_interval": "weekly", "spend_limit_include_oauth": false }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["name"], "ci key");
        assert_eq!(json["spend_limit"], 25.0);
        assert_eq!(json["spend_limit_interval"], "weekly");
        assert_eq!(json["spend_limit_include_oauth"], false);
        let plaintext = json["key"].as_str().expect("the plaintext is returned once").to_string();
        assert!(plaintext.starts_with("sk-kano-proxy-"));
        assert!(json.get("key_hash").is_none());

        // The list never repeats the plaintext or the hash.
        let response = test_router(state.clone())
            .oneshot(Request::builder().uri("/api/keys").header(header::COOKIE, &cookie).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let json = body_json(response).await;
        assert_eq!(json["keys"].as_array().unwrap().len(), 1);
        assert!(json["keys"][0].get("key").is_none());
        assert!(json["keys"][0].get("key_hash").is_none());
        assert_eq!(json["keys"][0]["window_spend"], 0.0);
    }

    #[tokio::test]
    async fn create_rejects_a_bad_limit_and_writes_nothing() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let cookie = signed_in(&state, "bad@example.com").await;
        for body in [json!({ "spend_limit": 0 }), json!({ "spend_limit": "ten" }), json!({ "spend_limit": 5, "spend_limit_interval": "hourly" })] {
            let response = test_router(state.clone()).oneshot(json_request("POST", "/api/keys", &cookie, body)).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(body_json(response).await, json!({ "error": "invalid_spend_limit" }));
        }
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_keys").fetch_one(state.pool()).await.unwrap();
        assert_eq!(rows, 0);
    }

    #[tokio::test]
    async fn the_list_reports_the_current_window_spend() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "spend@example.com").await;
        let (_, cookie) = create_session(&state, &user.id, false).await.unwrap();
        let cookie = cookie.split(';').next().unwrap().to_string();
        let created = create_key(state.pool(), &user.id, "k", None).await.unwrap();
        for cost in [Some(3.2), None] {
            insert_request_log(
                state.pool(),
                &RequestLogEntry {
                    user_id: user.id.clone(),
                    api_key_id: Some(created.row.id.clone()),
                    provider: "claude-code".into(),
                    model: "m".into(),
                    status_code: 200,
                    latency_ms: 1,
                    cost,
                    started_at: crate::ids::now_iso(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }
        let response = test_router(state.clone())
            .oneshot(Request::builder().uri("/api/keys").header(header::COOKIE, &cookie).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let json = body_json(response).await;
        let spend = json["keys"][0]["window_spend"].as_f64().unwrap();
        assert!((spend - 3.2).abs() < 1e-9, "a NULL cost contributes nothing; got {spend}");
    }

    #[tokio::test]
    async fn patch_renames_clears_the_limit_and_never_crosses_users() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "patch@example.com").await;
        let (_, cookie) = create_session(&state, &user.id, false).await.unwrap();
        let cookie = cookie.split(';').next().unwrap().to_string();
        let limits = SpendLimitFields { spend_limit: Some(50.0), spend_limit_interval: "monthly".into(), spend_limit_include_oauth: true };
        let mine = create_key(state.pool(), &user.id, "mine", Some(limits)).await.unwrap().row;

        let response = test_router(state.clone())
            .oneshot(json_request("PATCH", &format!("/api/keys/{}", mine.id), &cookie, json!({ "name": "renamed", "spend_limit": null })))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["name"], "renamed");
        assert_eq!(json["spend_limit"], Value::Null);

        // An empty name is refused; another user's key is a 404, never an edit.
        let response = test_router(state.clone())
            .oneshot(json_request("PATCH", &format!("/api/keys/{}", mine.id), &cookie, json!({ "name": "   " })))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await, json!({ "error": "invalid_name" }));

        let other = insert_user(state.pool(), "other@example.com").await;
        let theirs = create_key(state.pool(), &other.id, "not yours", None).await.unwrap().row;
        let response = test_router(state.clone())
            .oneshot(json_request("PATCH", &format!("/api/keys/{}", theirs.id), &cookie, json!({ "name": "hijack" })))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(list_keys(state.pool(), &other.id).await.unwrap()[0].name, "not yours");

        // Delete is scoped the same way.
        let response = test_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/keys/{}", theirs.id))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let response = test_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/keys/{}", mine.id))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(list_keys(state.pool(), &user.id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_admin_cors_rule_locks_the_origin_to_the_app() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let cookie = signed_in(&state, "cors@example.com").await;

        let response = test_router(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/keys")
                    .header(header::COOKIE, &cookie)
                    .header(header::ORIGIN, "https://app.example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(), "https://app.example.com");
        assert_eq!(response.headers().get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS).unwrap(), "true");
        assert_eq!(response.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");

        // Another origin gets no allow-origin header at all, so the browser drops the body.
        let response = test_router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/keys")
                    .header(header::COOKIE, &cookie)
                    .header(header::ORIGIN, "https://evil.example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN), None);
    }
}
