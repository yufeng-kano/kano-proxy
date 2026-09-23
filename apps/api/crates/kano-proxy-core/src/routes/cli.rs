//! `/api/cli`, the session-authenticated
//! management surface for the web UI's CLI page and its authorize view
//! (docs/cli.md § Web UI, docs/auth.md § CLI devices and providers). Same
//! origin-locked CORS as every other `/api/*` route.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::auth::session::SessionUser;
use crate::crypto::cli_tokens::{new_pairing_code, sha256_hex_str};
use crate::db::accounts::{list_accounts, update_account_identity, AccountIdentity};
use crate::db::cli::{
    approve_login_request, delete_login_request, get_cli_provider_by_id, get_login_request, list_cli_devices,
    rename_cli_device, rename_cli_provider, revoke_cli_device, CliLoginRequestRow,
};
use crate::utils::custom_provider::validate_name;
use crate::AppState;

use super::cli_shared::{list_cli_provider_items, remove_cli_provider};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/devices", get(list_devices))
        .route("/devices/{id}", axum::routing::patch(rename_device).delete(revoke_device))
        .route("/providers", get(list_providers))
        .route("/providers/{id}", axum::routing::patch(rename_provider).delete(delete_provider))
        .route("/login-requests/{id}", get(read_login_request))
        .route("/login-requests/{id}/approve", post(approve))
        .route("/login-requests/{id}/deny", post(deny))
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn internal_error() -> Response {
    error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
}

fn not_found() -> Response {
    error(StatusCode::NOT_FOUND, "not found")
}

async fn list_devices(State(state): State<AppState>, session: SessionUser) -> Response {
    let Ok(rows) = list_cli_devices(state.pool(), &session.user.id).await else {
        return internal_error();
    };
    let devices: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.id,
                "name": row.name,
                "last_seen_at": row.last_seen_at,
                "created_at": row.created_at,
            })
        })
        .collect();
    Json(json!({ "devices": devices })).into_response()
}

/// Display name only — the refresh token and the providers it registered stay.
async fn rename_device(
    State(state): State<AppState>,
    session: SessionUser,
    Path(id): Path<String>,
    body: bytes::Bytes,
) -> Response {
    let Ok(body) = serde_json::from_slice::<Value>(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid JSON");
    };
    let name = body.get("name").and_then(Value::as_str).map(str::trim).unwrap_or("");
    if let Some(message) = validate_name(name) {
        return error(StatusCode::BAD_REQUEST, &message);
    }
    match rename_cli_device(state.pool(), &session.user.id, &id, name).await {
        Ok(true) => Json(json!({ "ok": true, "name": name })).into_response(),
        Ok(false) => not_found(),
        Err(_) => internal_error(),
    }
}

/// Revoke deletes the device row. Idempotent: a device that is already gone —
/// or was never the caller's — is still ok, and nothing about it leaks.
async fn revoke_device(State(state): State<AppState>, session: SessionUser, Path(id): Path<String>) -> Response {
    if revoke_cli_device(state.pool(), &session.user.id, &id).await.is_err() {
        return internal_error();
    }
    Json(json!({ "ok": true })).into_response()
}

async fn list_providers(State(state): State<AppState>, session: SessionUser) -> Response {
    match list_cli_provider_items(&state, &session.user.id).await {
        Ok(providers) => Json(json!({ "providers": providers })).into_response(),
        Err(_) => internal_error(),
    }
}

async fn rename_provider(
    State(state): State<AppState>,
    session: SessionUser,
    Path(id): Path<String>,
    body: bytes::Bytes,
) -> Response {
    let row = match get_cli_provider_by_id(state.pool(), &session.user.id, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return not_found(),
        Err(_) => return internal_error(),
    };
    let Ok(body) = serde_json::from_slice::<Value>(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid JSON");
    };
    let name = body.get("name").and_then(Value::as_str).map(str::trim).unwrap_or("");
    if let Some(message) = validate_name(name) {
        return error(StatusCode::BAD_REQUEST, &message);
    }
    if rename_cli_provider(state.pool(), &session.user.id, &row.id, name).await.is_err() {
        return internal_error();
    }
    // The internal pool-state row's label is what a group pin resolves its
    // account_label from — keep it in step or the Groups page shows the
    // creation-time name forever.
    let Ok(accounts) = list_accounts(state.pool(), &session.user.id, &row.slug).await else {
        return internal_error();
    };
    for account in accounts {
        if update_account_identity(
            state.pool(),
            &account.id,
            AccountIdentity { label: Some(name), account_meta_json: None },
        )
        .await
        .is_err()
        {
            return internal_error();
        }
    }
    Json(json!({ "ok": true, "name": name })).into_response()
}

async fn delete_provider(State(state): State<AppState>, session: SessionUser, Path(id): Path<String>) -> Response {
    let row = match get_cli_provider_by_id(state.pool(), &session.user.id, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return not_found(),
        Err(_) => return internal_error(),
    };
    match remove_cli_provider(&state, &session.user.id, &row).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(_) => internal_error(),
    }
}

/// A login request is readable only while it is live — an expired one is a 404, so
/// the authorize view never offers to approve a dead request.
fn live_request(state: &AppState, row: Option<CliLoginRequestRow>) -> Option<CliLoginRequestRow> {
    let row = row?;
    let expires = crate::db::accounts::parse_iso_ms(&row.expires_at)?;
    (expires >= state.now_ms()).then_some(row)
}

async fn read_login_request(State(state): State<AppState>, _session: SessionUser, Path(id): Path<String>) -> Response {
    let Ok(row) = get_login_request(state.pool(), &id).await else {
        return internal_error();
    };
    let Some(row) = live_request(&state, row) else {
        return not_found();
    };
    Json(json!({
        "id": row.id,
        "device_name": row.device_name,
        "expires_at": row.expires_at,
        "approved": row.approved_at.is_some(),
        "used": row.used_at.is_some(),
    }))
    .into_response()
}

async fn approve(State(state): State<AppState>, session: SessionUser, Path(id): Path<String>) -> Response {
    let Ok(row) = get_login_request(state.pool(), &id).await else {
        return internal_error();
    };
    let Some(row) = live_request(&state, row) else {
        return not_found();
    };
    if row.used_at.is_some() || row.approved_at.is_some() {
        return error(StatusCode::BAD_REQUEST, "already approved");
    }
    // The plaintext code exists exactly here and in this response — the row stores
    // only its hash, and a page refresh can never re-show it.
    let code = new_pairing_code();
    let hash = sha256_hex_str(&code.replacen('-', "", 1));
    match approve_login_request(state.pool(), &row.id, &session.user.id, &hash).await {
        Ok(true) => Json(json!({ "ok": true, "code": code })).into_response(),
        Ok(false) => error(StatusCode::BAD_REQUEST, "already approved"),
        Err(_) => internal_error(),
    }
}

async fn deny(State(state): State<AppState>, _session: SessionUser, Path(id): Path<String>) -> Response {
    let Ok(Some(row)) = get_login_request(state.pool(), &id).await else {
        return not_found();
    };
    if delete_login_request(state.pool(), &row.id).await.is_err() {
        return internal_error();
    }
    Json(json!({ "ok": true })).into_response()
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use serde_json::Value;
    use tower::ServiceExt;

    use crate::auth::session::create_session;
    use crate::db::accounts::{insert_account, list_accounts, NewAccount};
    use crate::db::cli::{
        get_cli_provider_by_id, get_login_request, insert_cli_device, insert_cli_provider, insert_login_request,
        list_cli_devices, NewCliProvider,
    };
    use crate::db::test_support::{insert_user, skip_without_db, test_pool, test_router, test_state};
    use crate::db::users::UserRow;
    use crate::upstream::MockTransport;
    use crate::AppState;

    async fn cookie_for(state: &AppState, user_id: &str) -> String {
        let (_, cookie) = create_session(state, user_id, false).await.unwrap();
        cookie.split(';').next().unwrap().to_string()
    }

    async fn body_json(response: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    }

    fn request(method: &str, uri: &str, cookie: &str, body: Option<Value>) -> Request<Body> {
        let builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::COOKIE, cookie)
            .header(header::CONTENT_TYPE, "application/json");
        match body {
            Some(value) => builder.body(Body::from(value.to_string())).unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        }
    }

    async fn send(state: &AppState, req: Request<Body>) -> axum::response::Response {
        test_router(state.clone()).oneshot(req).await.unwrap()
    }

    /// The seed of cli_routes.test.ts: one device, one provider and its internal
    /// pool-state account row.
    async fn seed_provider(state: &AppState, user: &UserRow) -> (String, String) {
        let device = insert_cli_device(state.pool(), &user.id, "my-mac", "hash").await.unwrap();
        let row = insert_cli_provider(
            state.pool(),
            NewCliProvider {
                user_id: &user.id,
                device_id: Some(&device.id),
                slug: "my-mac",
                name: "My Mac",
                format: "openai",
                models_json: Some(r#"["llama3"]"#),
                model_filter_json: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
        insert_account(
            state.pool(),
            NewAccount {
                user_id: &user.id,
                provider: "my-mac",
                encrypted_payload: "irrelevant",
                label: Some("My Mac"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        (device.id, row.id)
    }

    #[tokio::test]
    async fn lists_the_callers_devices_only() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "one@example.com").await;
        let other = insert_user(state.pool(), "two@example.com").await;
        let device = insert_cli_device(state.pool(), &user.id, "my-mac", "hash").await.unwrap();
        insert_cli_device(state.pool(), &other.id, "their-box", "hash-2").await.unwrap();

        let cookie = cookie_for(&state, &user.id).await;
        let response = send(&state, request("GET", "/api/cli/devices", &cookie, None)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["devices"].as_array().unwrap().len(), 1);
        assert_eq!(json["devices"][0]["id"], device.id);
        assert_eq!(json["devices"][0]["name"], "my-mac");
        assert!(json["devices"][0].get("revoked_at").is_none());
    }

    #[tokio::test]
    async fn revoke_deletes_the_device_and_never_a_foreign_one() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "owner@example.com").await;
        let other = insert_user(state.pool(), "other@example.com").await;
        let device = insert_cli_device(state.pool(), &user.id, "my-mac", "hash").await.unwrap();
        let uri = format!("/api/cli/devices/{}", device.id);

        let foreign = cookie_for(&state, &other.id).await;
        assert_eq!(send(&state, request("DELETE", &uri, &foreign, None)).await.status(), StatusCode::OK);
        assert_eq!(list_cli_devices(state.pool(), &user.id).await.unwrap().len(), 1, "a foreign revoke deletes nothing");

        let cookie = cookie_for(&state, &user.id).await;
        assert_eq!(send(&state, request("DELETE", &uri, &cookie, None)).await.status(), StatusCode::OK);
        assert!(list_cli_devices(state.pool(), &user.id).await.unwrap().is_empty(), "revoke keeps no row");
        // Second revoke stays ok.
        assert_eq!(send(&state, request("DELETE", &uri, &cookie, None)).await.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn renames_only_the_callers_device() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "renamer@example.com").await;
        let other = insert_user(state.pool(), "intruder@example.com").await;
        let device = insert_cli_device(state.pool(), &user.id, "my-mac", "hash").await.unwrap();
        let uri = format!("/api/cli/devices/{}", device.id);
        let body = || Some(serde_json::json!({ "name": "  Studio box  " }));

        let foreign = cookie_for(&state, &other.id).await;
        assert_eq!(send(&state, request("PATCH", &uri, &foreign, body())).await.status(), StatusCode::NOT_FOUND);

        let cookie = cookie_for(&state, &user.id).await;
        let response = send(&state, request("PATCH", &uri, &cookie, body())).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(list_cli_devices(state.pool(), &user.id).await.unwrap()[0].name, "Studio box");

        let blank = send(&state, request("PATCH", &uri, &cookie, Some(serde_json::json!({ "name": " " })))).await;
        assert_eq!(blank.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn lists_providers_with_the_device_name_and_stored_models() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "prov@example.com").await;
        seed_provider(&state, &user).await;

        let cookie = cookie_for(&state, &user.id).await;
        let json = body_json(send(&state, request("GET", "/api/cli/providers", &cookie, None)).await).await;
        let provider = &json["providers"][0];
        assert_eq!(provider["slug"], "my-mac");
        assert_eq!(provider["device_name"], "my-mac");
        assert_eq!(provider["connected"], false);
        assert_eq!(provider["models"], serde_json::json!(["llama3"]));
        assert_eq!(provider["models_reported"], 1);
    }

    #[tokio::test]
    async fn rename_keeps_the_internal_account_label_in_step() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "rename@example.com").await;
        let (_, provider_id) = seed_provider(&state, &user).await;
        let cookie = cookie_for(&state, &user.id).await;
        let uri = format!("/api/cli/providers/{provider_id}");

        let response =
            send(&state, request("PATCH", &uri, &cookie, Some(serde_json::json!({ "name": "Studio box" })))).await;
        assert_eq!(response.status(), StatusCode::OK);
        let row = get_cli_provider_by_id(state.pool(), &user.id, &provider_id).await.unwrap().unwrap();
        assert_eq!(row.name, "Studio box");
        assert_eq!(row.slug, "my-mac", "the slug is never renamed");
        let accounts = list_accounts(state.pool(), &user.id, "my-mac").await.unwrap();
        assert_eq!(accounts[0].label.as_deref(), Some("Studio box"));

        let bad = send(&state, request("PATCH", &uri, &cookie, Some(serde_json::json!({ "name": "" })))).await;
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_removes_the_provider_and_its_account_rows() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "delete@example.com").await;
        let (_, provider_id) = seed_provider(&state, &user).await;
        let cookie = cookie_for(&state, &user.id).await;

        let uri = format!("/api/cli/providers/{provider_id}");
        assert_eq!(send(&state, request("DELETE", &uri, &cookie, None)).await.status(), StatusCode::OK);
        assert!(get_cli_provider_by_id(state.pool(), &user.id, &provider_id).await.unwrap().is_none());
        assert!(list_accounts(state.pool(), &user.id, "my-mac").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn reads_a_pending_request_and_404s_expired_ones() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "read@example.com").await;
        let row = insert_login_request(state.pool(), "new-box", "ip").await.unwrap().unwrap();
        let cookie = cookie_for(&state, &user.id).await;
        let uri = format!("/api/cli/login-requests/{}", row.id);

        let json = body_json(send(&state, request("GET", &uri, &cookie, None)).await).await;
        assert_eq!(json["device_name"], "new-box");
        assert_eq!(json["approved"], false);
        assert_eq!(json["used"], false);

        sqlx::query("UPDATE cli_login_requests SET expires_at = $1 WHERE id = $2")
            .bind("2020-01-01T00:00:00.000Z")
            .bind(&row.id)
            .execute(state.pool())
            .await
            .unwrap();
        assert_eq!(send(&state, request("GET", &uri, &cookie, None)).await.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn approve_returns_the_code_once_and_stores_only_its_hash() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "approve@example.com").await;
        let row = insert_login_request(state.pool(), "new-box", "ip").await.unwrap().unwrap();
        let cookie = cookie_for(&state, &user.id).await;
        let uri = format!("/api/cli/login-requests/{}/approve", row.id);

        let json = body_json(send(&state, request("POST", &uri, &cookie, None)).await).await;
        let code = json["code"].as_str().unwrap().to_string();
        assert_eq!(code.len(), 9);
        assert_eq!(&code[4..5], "-");
        assert!(code.bytes().filter(|b| *b != b'-').all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()));
        let stored = get_login_request(state.pool(), &row.id).await.unwrap().unwrap();
        assert_eq!(stored.user_id.as_deref(), Some(user.id.as_str()));
        let hash = stored.code_hash.unwrap();
        assert!(!hash.contains(&code[..4]), "only the hash is stored");

        let again = send(&state, request("POST", &uri, &cookie, None)).await;
        assert_eq!(again.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn deny_deletes_the_request() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "deny@example.com").await;
        let row = insert_login_request(state.pool(), "new-box", "ip").await.unwrap().unwrap();
        let cookie = cookie_for(&state, &user.id).await;

        let uri = format!("/api/cli/login-requests/{}/deny", row.id);
        assert_eq!(send(&state, request("POST", &uri, &cookie, None)).await.status(), StatusCode::OK);
        assert!(get_login_request(state.pool(), &row.id).await.unwrap().is_none());
    }

    /// Every `/api/cli` route is session-authenticated: no cookie is the admin 401.
    #[tokio::test]
    async fn the_surface_requires_a_session() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        for (method, uri) in [
            ("GET", "/api/cli/devices"),
            ("PATCH", "/api/cli/devices/clidev_1"),
            ("DELETE", "/api/cli/devices/clidev_1"),
            ("GET", "/api/cli/providers"),
            ("PATCH", "/api/cli/providers/cliprov_1"),
            ("DELETE", "/api/cli/providers/cliprov_1"),
            ("GET", "/api/cli/login-requests/clireq_1"),
            ("POST", "/api/cli/login-requests/clireq_1/approve"),
            ("POST", "/api/cli/login-requests/clireq_1/deny"),
        ] {
            let response = send(&state, request(method, uri, "", None)).await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{method} {uri}");
        }
    }
}
