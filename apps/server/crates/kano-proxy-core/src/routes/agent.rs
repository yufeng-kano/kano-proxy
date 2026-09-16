//! `/agent/v1`, the kano-proxy CLI's own
//! namespace (docs/cli.md § Server routes): device login, refresh-token rotation,
//! provider CRUD and the WebSocket connect that hands a verified socket to the
//! tunnel registry. Token-authenticated (never session), no CORS — there are no
//! browser callers.

use std::net::SocketAddr;

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::crypto::cli_tokens::{
    mint_access_token, new_refresh_token, normalize_pairing_code, sha256_hex_str, verify_access_token, CliTokenClaims,
};
use crate::crypto::timing_safe_equal;
use crate::crypto::token_crypto::encrypt_json;
use crate::db::accounts::{insert_account, NewAccount};
use crate::db::cli::{
    count_cli_devices, count_cli_providers, delete_cli_provider, find_cli_device_by_prev_refresh_hash,
    find_cli_device_by_refresh_hash, get_cli_device, get_cli_provider_by_id, get_cli_provider_by_slug,
    get_login_request, insert_cli_device, insert_cli_provider, insert_login_request, mark_login_request_used,
    record_login_code_attempt, rotate_cli_device_refresh_token, touch_cli_device_last_seen, CliDeviceRow,
    NewCliProvider, MAX_CLI_DEVICES_PER_USER, MAX_LOGIN_CODE_ATTEMPTS,
};
use crate::db::custom_providers::{count_custom_providers, get_custom_provider_by_slug, MAX_CUSTOM_PROVIDERS_PER_USER};
use crate::pool::StoredCredential;
use crate::tunnel::protocol::{validate_models_report, CliProviderFormat};
use crate::tunnel::registry::ConnectParams;
use crate::utils::custom_provider::{is_custom_provider_format, validate_name, validate_slug};
use crate::AppState;

use super::cli_shared::{list_cli_provider_items, remove_cli_provider};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/login/start", post(login_start))
        .route("/login/complete", post(login_complete))
        .route("/token", post(token))
        .route("/providers", get(list_providers).post(create_provider))
        .route("/providers/{id}", axum::routing::delete(delete_provider))
        .route("/connect/{providerId}", get(connect))
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn internal_error() -> Response {
    error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
}

fn unauthorized() -> Response {
    error(StatusCode::UNAUTHORIZED, "unauthorized")
}

fn invalid_json() -> Response {
    error(StatusCode::BAD_REQUEST, "invalid JSON")
}

fn parse_body(body: &[u8]) -> Option<Value> {
    serde_json::from_slice(body).ok()
}

fn trimmed_str<'a>(body: &'a Value, key: &str) -> &'a str {
    body.get(key).and_then(Value::as_str).map(str::trim).unwrap_or("")
}

/// The caller's address for the per-IP login budget. A deployment behind a proxy or
/// Cloudflare carries the real client in a header; a direct listener has only the
/// socket. Only the hash of this ever reaches storage (docs/cli.md § Security notes).
fn client_ip(parts: &Parts) -> String {
    let header = |name: &str| parts.headers.get(name).and_then(|v| v.to_str().ok());
    if let Some(forwarded) = header("x-forwarded-for") {
        if let Some(first) = forwarded.split(',').next().map(str::trim).filter(|s| !s.is_empty()) {
            return first.to_string();
        }
    }
    if let Some(ip) = header("cf-connecting-ip").map(str::trim).filter(|s| !s.is_empty()) {
        return ip.to_string();
    }
    match parts.extensions.get::<ConnectInfo<SocketAddr>>() {
        Some(ConnectInfo(addr)) => addr.ip().to_string(),
        None => "unknown".to_string(),
    }
}

async fn login_start(State(state): State<AppState>, parts: Parts, body: bytes::Bytes) -> Response {
    // Fail closed at the *first* request when device auth is unprovisioned —
    // otherwise the user walks the whole browser approval before /login/complete
    // can only 500, and the disabled endpoint still accumulates request rows.
    if state.config().cli_token_secret.is_none() {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "CLI_TOKEN_SECRET not configured");
    }
    let Some(body) = parse_body(&body) else { return invalid_json() };
    let device_name = trimmed_str(&body, "device_name").to_string();
    if let Some(message) = validate_name(&device_name) {
        return error(StatusCode::BAD_REQUEST, &format!("device_name: {message}"));
    }
    // The budget is enforced atomically inside the INSERT — a read-modify-write
    // counter here was bypassable by parallel batches from one address.
    let ip_hash = sha256_hex_str(&client_ip(&parts));
    match insert_login_request(state.pool(), &device_name, &ip_hash).await {
        Ok(Some(row)) => {
            let app_url = state.config().app_url.trim_end_matches('/');
            Json(json!({
                "request_id": row.id,
                "verify_url": format!("{app_url}/cli/authorize?request={}", row.id),
                "expires_at": row.expires_at,
            }))
            .into_response()
        }
        Ok(None) => error(StatusCode::TOO_MANY_REQUESTS, "too many login attempts — try again later"),
        Err(_) => internal_error(),
    }
}

async fn login_complete(State(state): State<AppState>, body: bytes::Bytes) -> Response {
    let Some(secret) = state.config().cli_token_secret.clone() else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "CLI_TOKEN_SECRET not configured");
    };
    let Some(body) = parse_body(&body) else { return invalid_json() };
    let request_id = body.get("request_id").and_then(Value::as_str).unwrap_or("").to_string();
    let code = body.get("code").and_then(Value::as_str).map(normalize_pairing_code).unwrap_or_default();
    if request_id.is_empty() || code.is_empty() {
        return error(StatusCode::BAD_REQUEST, "request_id and code are required");
    }

    let expired = || error(StatusCode::UNAUTHORIZED, "login request expired or already used — run init again");
    let Ok(row) = get_login_request(state.pool(), &request_id).await else {
        return internal_error();
    };
    let Some(row) = row else { return expired() };
    let live = crate::db::accounts::parse_iso_ms(&row.expires_at).is_some_and(|at| at >= state.now_ms());
    if row.used_at.is_some() || !live {
        return expired();
    }
    let (Some(user_id), Some(code_hash)) = (row.user_id.clone(), row.code_hash.clone()) else {
        return error(StatusCode::UNAUTHORIZED, "login request not approved yet");
    };
    if row.approved_at.is_none() {
        return error(StatusCode::UNAUTHORIZED, "login request not approved yet");
    }
    if row.attempts >= MAX_LOGIN_CODE_ATTEMPTS {
        return error(StatusCode::UNAUTHORIZED, "too many wrong codes — run init again");
    }
    if !timing_safe_equal(&sha256_hex_str(&code), &code_hash) {
        let _ = record_login_code_attempt(state.pool(), &request_id).await;
        return error(StatusCode::UNAUTHORIZED, "wrong code");
    }
    // Single-use, atomically: two racing completes cannot both mint a device.
    match mark_login_request_used(state.pool(), &request_id).await {
        Ok(true) => {}
        Ok(false) => return expired(),
        Err(_) => return internal_error(),
    }
    match count_cli_devices(state.pool(), &user_id).await {
        Ok(count) if count >= MAX_CLI_DEVICES_PER_USER => {
            return error(StatusCode::BAD_REQUEST, &format!("maximum of {MAX_CLI_DEVICES_PER_USER} devices reached"))
        }
        Err(_) => return internal_error(),
        Ok(_) => {}
    }

    let refresh_token = new_refresh_token();
    let Ok(device) =
        insert_cli_device(state.pool(), &user_id, &row.device_name, &sha256_hex_str(&refresh_token)).await
    else {
        return internal_error();
    };
    let access = mint_access_token(&secret, &user_id, &device.id, state.now_ms());
    Json(json!({
        "device_id": device.id,
        "refresh_token": refresh_token,
        "access_token": access.token,
        "expires_in": access.expires_in,
    }))
    .into_response()
}

async fn token(State(state): State<AppState>, body: bytes::Bytes) -> Response {
    let Some(secret) = state.config().cli_token_secret.clone() else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "CLI_TOKEN_SECRET not configured");
    };
    let Some(body) = parse_body(&body) else { return invalid_json() };
    let presented = body.get("refresh_token").and_then(Value::as_str).unwrap_or("");
    if presented.is_empty() {
        return error(StatusCode::BAD_REQUEST, "refresh_token is required");
    }
    let presented_hash = sha256_hex_str(presented);

    let Ok(device) = find_cli_device_by_refresh_hash(state.pool(), &presented_hash).await else {
        return internal_error();
    };
    if let Some(device) = device {
        if device.revoked_at.is_some() {
            return error(StatusCode::UNAUTHORIZED, "device_revoked");
        }
        let next = new_refresh_token();
        let rotated =
            rotate_cli_device_refresh_token(state.pool(), &device.id, &presented_hash, &sha256_hex_str(&next)).await;
        match rotated {
            // Lost the race to a concurrent presentation of the same token — that
            // sibling already rotated; this caller retries against its state file.
            Ok(false) => return error(StatusCode::UNAUTHORIZED, "invalid refresh token"),
            Err(_) => return internal_error(),
            Ok(true) => {}
        }
        let access = mint_access_token(&secret, &device.user_id, &device.id, state.now_ms());
        return Json(json!({
            "refresh_token": next,
            "access_token": access.token,
            "expires_in": access.expires_in,
        }))
        .into_response();
    }

    // A superseded token is treated as theft: revoke the whole device family
    // (docs/cli.md § Device auth). A token matching nothing is a plain 401.
    let Ok(stale) = find_cli_device_by_prev_refresh_hash(state.pool(), &presented_hash).await else {
        return internal_error();
    };
    if let Some(stale) = stale.filter(|d| d.revoked_at.is_none()) {
        if sqlx::query("UPDATE cli_devices SET revoked_at = $1 WHERE id = $2")
            .bind(crate::ids::now_iso())
            .bind(&stale.id)
            .execute(state.pool())
            .await
            .is_err()
        {
            return internal_error();
        }
        tracing::warn!(device_id = %stale.id, "refresh-token reuse revoked device");
        return error(StatusCode::UNAUTHORIZED, "device_revoked");
    }
    error(StatusCode::UNAUTHORIZED, "invalid refresh token")
}

/// Bearer access token → claims + non-revoked device row, or `None` (the caller
/// answers 401).
struct DeviceAuth {
    claims: CliTokenClaims,
    device: CliDeviceRow,
}

async fn authenticate_device(state: &AppState, headers: &HeaderMap) -> Option<DeviceAuth> {
    let secret = state.config().cli_token_secret.as_deref()?;
    let header = headers.get(axum::http::header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, value) = header.split_once(char::is_whitespace)?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let claims = verify_access_token(secret, value.trim(), state.now_ms())?;
    let device = get_cli_device(state.pool(), &claims.device_id).await.ok().flatten()?;
    if device.user_id != claims.user_id || device.revoked_at.is_some() {
        return None;
    }
    Some(DeviceAuth { claims, device })
}

async fn list_providers(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(auth) = authenticate_device(&state, &headers).await else { return unauthorized() };
    match list_cli_provider_items(&state, &auth.claims.user_id).await {
        Ok(providers) => Json(json!({ "providers": providers })).into_response(),
        Err(_) => internal_error(),
    }
}

/// A bounded model list off the wire: absent is `Ok(None)`, a bad shape or an
/// out-of-bounds entry is the caller's 400 (docs/cli.md § Model catalog).
fn models_field(body: &Value, key: &str) -> Result<Option<Option<String>>, String> {
    let Some(value) = body.get(key) else { return Ok(None) };
    let Some(items) = value.as_array() else {
        return Err(format!("{key} must be an array of model id strings"));
    };
    let mut models = Vec::with_capacity(items.len());
    for item in items {
        let Some(text) = item.as_str() else {
            return Err(format!("{key} must be an array of model id strings"));
        };
        models.push(text.to_string());
    }
    let Some(validated) = validate_models_report(&models) else {
        return Err(format!("{key} entries are out of bounds"));
    };
    Ok(Some(if validated.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&validated).expect("models serialize"))
    }))
}

async fn create_provider(State(state): State<AppState>, headers: HeaderMap, body: bytes::Bytes) -> Response {
    let Some(auth) = authenticate_device(&state, &headers).await else { return unauthorized() };
    let Some(token_key) = state.config().token_encryption_key.clone() else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "TOKEN_ENCRYPTION_KEY not configured");
    };
    let user_id = auth.claims.user_id.clone();
    let Some(body) = parse_body(&body) else { return invalid_json() };

    let slug = trimmed_str(&body, "slug").to_lowercase();
    if let Some(message) = validate_slug(&slug) {
        return error(StatusCode::BAD_REQUEST, &message);
    }
    let format = body.get("format").cloned().unwrap_or(Value::Null);
    if !is_custom_provider_format(&format) {
        return error(StatusCode::BAD_REQUEST, "format must be 'openai' or 'anthropic'");
    }
    let format = format.as_str().expect("checked above").to_string();
    let name = match trimmed_str(&body, "name") {
        "" => slug.clone(),
        value => value.to_string(),
    };
    if let Some(message) = validate_name(&name) {
        return error(StatusCode::BAD_REQUEST, &message);
    }

    // Expose whitelist and probe-failure manual seed share the report bounds; the
    // agent's first connect overwrites the seed.
    let model_filter_json = match models_field(&body, "expose") {
        Ok(value) => value.flatten(),
        Err(message) => return error(StatusCode::BAD_REQUEST, &message),
    };
    let models_json = match models_field(&body, "initial_models") {
        Ok(value) => value.flatten(),
        Err(message) => return error(StatusCode::BAD_REQUEST, &message),
    };

    // Shared slug namespace and shared 20-per-user cap with custom providers.
    let cap_reached = format!("maximum of {MAX_CUSTOM_PROVIDERS_PER_USER} providers reached (custom + CLI)");
    let (Ok(custom_count), Ok(cli_count)) = (
        count_custom_providers(state.pool(), &user_id).await,
        count_cli_providers(state.pool(), &user_id).await,
    ) else {
        return internal_error();
    };
    if custom_count + cli_count >= MAX_CUSTOM_PROVIDERS_PER_USER {
        return error(StatusCode::BAD_REQUEST, &cap_reached);
    }
    let taken_by_custom = format!("slug \"{slug}\" is already in use by a custom provider");
    match get_cli_provider_by_slug(state.pool(), &user_id, &slug).await {
        Ok(Some(_)) => return error(StatusCode::CONFLICT, &format!("slug \"{slug}\" is already in use")),
        Err(_) => return internal_error(),
        Ok(None) => {}
    }
    match get_custom_provider_by_slug(state.pool(), &user_id, &slug).await {
        Ok(Some(_)) => return error(StatusCode::CONFLICT, &taken_by_custom),
        Err(_) => return internal_error(),
        Ok(None) => {}
    }

    let inserted = insert_cli_provider(
        state.pool(),
        NewCliProvider {
            user_id: &user_id,
            device_id: Some(&auth.device.id),
            slug: &slug,
            name: &name,
            format: &format,
            models_json: models_json.as_deref(),
            model_filter_json: model_filter_json.as_deref(),
        },
    )
    .await;
    let row = match inserted {
        Ok(Some(row)) => row,
        Err(_) => return internal_error(),
        // The atomic guard refused — say which condition actually failed rather
        // than blaming a free slug for a cap that filled in the race window.
        Ok(None) => {
            return match get_custom_provider_by_slug(state.pool(), &user_id, &slug).await {
                Ok(Some(_)) => error(StatusCode::CONFLICT, &taken_by_custom),
                Err(_) => internal_error(),
                Ok(None) => error(StatusCode::BAD_REQUEST, &cap_reached),
            }
        }
    };

    // The internal pool-state row (docs/cli.md § Data model): a placeholder
    // credential that decrypts fine and authorizes nothing — the CLI injects the
    // local server's real key on its side of the tunnel. A failed account write
    // compensates by deleting the provider row, or a slug-reserving orphan would
    // answer no_upstream_account until manually deleted.
    let account = async {
        let encrypted = encrypt_json(Some(&token_key), &StoredCredential::default())?;
        insert_account(
            state.pool(),
            NewAccount {
                user_id: &user_id,
                provider: &slug,
                encrypted_payload: &encrypted,
                label: Some(&name),
                ..Default::default()
            },
        )
        .await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Err(error_value) = account {
        let _ = delete_cli_provider(state.pool(), &user_id, &row.id).await;
        tracing::error!(provider_id = %row.id, error = %error_value, "provider account insert failed");
        return error(StatusCode::INTERNAL_SERVER_ERROR, "could not create the provider — try again");
    }

    (
        StatusCode::CREATED,
        Json(json!({
            "id": row.id,
            "slug": row.slug,
            "name": row.name,
            "format": row.format,
            "created_at": row.created_at,
        })),
    )
        .into_response()
}

async fn delete_provider(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let Some(auth) = authenticate_device(&state, &headers).await else { return unauthorized() };
    let row = match get_cli_provider_by_id(state.pool(), &auth.claims.user_id, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return error(StatusCode::NOT_FOUND, "not found"),
        Err(_) => return internal_error(),
    };
    match remove_cli_provider(&state, &auth.claims.user_id, &row).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(_) => internal_error(),
    }
}

/// `GET /connect/:providerId` — the only WebSocket route. Everything the tunnel
/// trusts is verified here first: token signature and expiry, a device that is not
/// revoked, and a provider row belonging to the token's user. The registry never
/// sees a token, exactly as the Durable Object never did.
async fn connect(
    State(state): State<AppState>,
    Path(provider_id): Path<String>,
    parts: Parts,
    upgrade: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> Response {
    let is_websocket = parts
        .headers
        .get(axum::http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    if !is_websocket {
        return error(StatusCode::UPGRADE_REQUIRED, "expected websocket upgrade");
    }
    let Some(auth) = authenticate_device(&state, &parts.headers).await else { return unauthorized() };
    let row = match get_cli_provider_by_id(state.pool(), &auth.claims.user_id, &provider_id).await {
        Ok(Some(row)) => row,
        Ok(None) => return error(StatusCode::NOT_FOUND, "not found"),
        Err(_) => return internal_error(),
    };
    let Some(format) = CliProviderFormat::parse(&row.format) else {
        return error(StatusCode::BAD_REQUEST, "unsupported provider format");
    };
    let upgrade = match upgrade {
        Ok(upgrade) => upgrade,
        Err(rejection) => return rejection.into_response(),
    };

    let _ = touch_cli_device_last_seen(state.pool(), &auth.device.id).await;

    // The access token's `exp` drives the revocation close, as the DO's alarm did.
    let params = ConnectParams {
        user_id: auth.claims.user_id.clone(),
        provider_id: row.id.clone(),
        slug: row.slug.clone(),
        format,
        token_exp_ms: auth.claims.exp.saturating_mul(1000),
    };
    crate::tunnel::ws::connect(state.tunnels().clone(), params, upgrade)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use futures::StreamExt;
    use serde_json::{json, Value};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tower::ServiceExt;

    use super::*;
    use crate::auth::session::create_session;
    use crate::db::accounts::list_accounts;
    use crate::db::cli::{list_cli_devices, CliProviderRow};
    use crate::db::custom_providers::{insert_custom_provider, NewCustomProvider};
    use crate::db::test_support::{insert_user, skip_without_db, test_pool, test_router, test_state};
    use crate::db::users::UserRow;
    use crate::upstream::MockTransport;

    async fn body_json(response: Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    }

    fn json_req(method: &str, uri: &str, body: Option<Value>, headers: &[(&str, &str)]) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(uri).header(header::CONTENT_TYPE, "application/json");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        match body {
            Some(value) => builder.body(Body::from(value.to_string())).unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        }
    }

    async fn send(state: &AppState, req: Request<Body>) -> Response {
        test_router(state.clone()).oneshot(req).await.unwrap()
    }

    struct Tokens {
        access_token: String,
        refresh_token: String,
        device_id: String,
    }

    fn auth_headers(tokens: &Tokens) -> String {
        format!("Bearer {}", tokens.access_token)
    }

    /// The full login dance of agent_routes.test.ts: start → session approve →
    /// complete.
    async fn sign_in_device(state: &AppState, user: &UserRow, device_name: &str) -> Tokens {
        let start = send(state, json_req("POST", "/agent/v1/login/start", Some(json!({ "device_name": device_name })), &[])).await;
        assert_eq!(start.status(), StatusCode::OK);
        let start = body_json(start).await;
        let request_id = start["request_id"].as_str().unwrap().to_string();
        assert!(start["verify_url"]
            .as_str()
            .unwrap()
            .starts_with("https://app.example.com/cli/authorize?request="));

        let (_, cookie) = create_session(state, &user.id, false).await.unwrap();
        let cookie = cookie.split(';').next().unwrap().to_string();
        let approve = send(
            state,
            json_req(
                "POST",
                &format!("/api/cli/login-requests/{request_id}/approve"),
                None,
                &[("cookie", &cookie)],
            ),
        )
        .await;
        assert_eq!(approve.status(), StatusCode::OK);
        let code = body_json(approve).await["code"].as_str().unwrap().to_string();

        let complete = send(
            state,
            json_req("POST", "/agent/v1/login/complete", Some(json!({ "request_id": request_id, "code": code })), &[]),
        )
        .await;
        assert_eq!(complete.status(), StatusCode::OK);
        let tokens = body_json(complete).await;
        Tokens {
            access_token: tokens["access_token"].as_str().unwrap().to_string(),
            refresh_token: tokens["refresh_token"].as_str().unwrap().to_string(),
            device_id: tokens["device_id"].as_str().unwrap().to_string(),
        }
    }

    #[tokio::test]
    async fn login_start_fails_closed_without_a_cli_token_secret() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let mut config = crate::db::test_support::test_config();
        config.cli_token_secret = None;
        let state = AppState::builder(config, pool).transport(MockTransport::new()).build();
        let response =
            send(&state, json_req("POST", "/agent/v1/login/start", Some(json!({ "device_name": "box" })), &[])).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cli_login_requests").fetch_one(state.pool()).await.unwrap();
        assert_eq!(rows, 0, "a disabled endpoint accumulates no request rows");
    }

    #[tokio::test]
    async fn start_approve_complete_mints_a_device_with_tokens() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "login@example.com").await;
        let tokens = sign_in_device(&state, &user, "test-box").await;
        assert!(tokens.device_id.starts_with("clidev_"));
        assert!(tokens.refresh_token.starts_with("kpr_"));
        assert!(tokens.access_token.contains('.'));

        let device = &list_cli_devices(state.pool(), &user.id).await.unwrap()[0];
        assert_eq!(device.user_id, user.id);
        assert_eq!(device.name, "test-box");
        // Only the hash is stored, never the token.
        assert!(!device.refresh_token_hash.contains("kpr_"));
    }

    #[tokio::test]
    async fn a_wrong_code_dies_after_five_attempts() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "attempts@example.com").await;
        let start =
            body_json(send(&state, json_req("POST", "/agent/v1/login/start", Some(json!({ "device_name": "box" })), &[])).await)
                .await;
        let request_id = start["request_id"].as_str().unwrap().to_string();
        let (_, cookie) = create_session(&state, &user.id, false).await.unwrap();
        let cookie = cookie.split(';').next().unwrap().to_string();
        send(
            &state,
            json_req("POST", &format!("/api/cli/login-requests/{request_id}/approve"), None, &[("cookie", &cookie)]),
        )
        .await;

        let wrong = || json!({ "request_id": request_id, "code": "AAAA-AAAA" });
        for _ in 0..5 {
            let response = send(&state, json_req("POST", "/agent/v1/login/complete", Some(wrong()), &[])).await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        let response = send(&state, json_req("POST", "/agent/v1/login/complete", Some(wrong()), &[])).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(body_json(response).await["error"].as_str().unwrap().contains("too many wrong codes"));
    }

    #[tokio::test]
    async fn a_code_redeems_exactly_once() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "once@example.com").await;
        let start =
            body_json(send(&state, json_req("POST", "/agent/v1/login/start", Some(json!({ "device_name": "box" })), &[])).await)
                .await;
        let request_id = start["request_id"].as_str().unwrap().to_string();
        let (_, cookie) = create_session(&state, &user.id, false).await.unwrap();
        let cookie = cookie.split(';').next().unwrap().to_string();
        let approve = send(
            &state,
            json_req("POST", &format!("/api/cli/login-requests/{request_id}/approve"), None, &[("cookie", &cookie)]),
        )
        .await;
        let code = body_json(approve).await["code"].as_str().unwrap().to_string();

        let complete = || json!({ "request_id": request_id, "code": code });
        assert_eq!(
            send(&state, json_req("POST", "/agent/v1/login/complete", Some(complete()), &[])).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            send(&state, json_req("POST", "/agent/v1/login/complete", Some(complete()), &[])).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn login_starts_are_rate_limited_per_ip() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let ip = [("cf-connecting-ip", "203.0.113.9")];
        for _ in 0..10 {
            let response =
                send(&state, json_req("POST", "/agent/v1/login/start", Some(json!({ "device_name": "box" })), &ip)).await;
            assert_eq!(response.status(), StatusCode::OK);
        }
        let response =
            send(&state, json_req("POST", "/agent/v1/login/start", Some(json!({ "device_name": "box" })), &ip)).await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

        // The budget is per address: a different client is unaffected.
        let other = [("cf-connecting-ip", "198.51.100.4")];
        let response =
            send(&state, json_req("POST", "/agent/v1/login/start", Some(json!({ "device_name": "box" })), &other)).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// `x-forwarded-for` wins over the socket address, and only the first hop of a
    /// forwarding chain counts.
    #[test]
    fn the_client_ip_prefers_the_forwarded_headers() {
        let parts = |headers: Vec<(&str, &str)>| {
            let mut builder = Request::builder().uri("/agent/v1/login/start");
            for (name, value) in headers {
                builder = builder.header(name, value);
            }
            builder.body(Body::empty()).unwrap().into_parts().0
        };
        assert_eq!(client_ip(&parts(vec![("x-forwarded-for", "203.0.113.9, 10.0.0.1")])), "203.0.113.9");
        assert_eq!(client_ip(&parts(vec![("cf-connecting-ip", "203.0.113.7")])), "203.0.113.7");
        assert_eq!(
            client_ip(&parts(vec![("x-forwarded-for", "203.0.113.9"), ("cf-connecting-ip", "198.51.100.4")])),
            "203.0.113.9"
        );
        assert_eq!(client_ip(&parts(vec![])), "unknown");

        let mut with_peer = parts(vec![]);
        with_peer.extensions.insert(ConnectInfo("192.0.2.5:4242".parse::<SocketAddr>().unwrap()));
        assert_eq!(client_ip(&with_peer), "192.0.2.5");
    }

    #[tokio::test]
    async fn refresh_rotates_and_keeps_one_generation_of_history() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "rotate@example.com").await;
        let tokens = sign_in_device(&state, &user, "box").await;

        let response = send(
            &state,
            json_req("POST", "/agent/v1/token", Some(json!({ "refresh_token": tokens.refresh_token })), &[]),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let next = body_json(response).await;
        assert_ne!(next["refresh_token"].as_str().unwrap(), tokens.refresh_token);
        assert!(!next["access_token"].as_str().unwrap().is_empty());
        assert_eq!(next["expires_in"], 3600);
        let device = &list_cli_devices(state.pool(), &user.id).await.unwrap()[0];
        assert!(device.refresh_token_prev_hash.is_some());
    }

    #[tokio::test]
    async fn a_superseded_token_revokes_the_device() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "theft@example.com").await;
        let tokens = sign_in_device(&state, &user, "box").await;
        let refresh = || json!({ "refresh_token": tokens.refresh_token });
        send(&state, json_req("POST", "/agent/v1/token", Some(refresh()), &[])).await;

        let reuse = send(&state, json_req("POST", "/agent/v1/token", Some(refresh()), &[])).await;
        assert_eq!(reuse.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(reuse).await["error"], "device_revoked");
        assert!(list_cli_devices(state.pool(), &user.id).await.unwrap()[0].revoked_at.is_some());
    }

    #[tokio::test]
    async fn a_token_matching_nothing_revokes_nothing() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "garbage@example.com").await;
        sign_in_device(&state, &user, "box").await;
        let response =
            send(&state, json_req("POST", "/agent/v1/token", Some(json!({ "refresh_token": "kpr_garbage" })), &[])).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(list_cli_devices(state.pool(), &user.id).await.unwrap()[0].revoked_at.is_none());
    }

    #[tokio::test]
    async fn a_revoked_device_cannot_refresh() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "revoked@example.com").await;
        let tokens = sign_in_device(&state, &user, "box").await;
        crate::db::cli::revoke_cli_device(state.pool(), &user.id, &tokens.device_id).await.unwrap();
        let response = send(
            &state,
            json_req("POST", "/agent/v1/token", Some(json!({ "refresh_token": tokens.refresh_token })), &[]),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    async fn create(state: &AppState, tokens: &Tokens, body: Value) -> Response {
        send(state, json_req("POST", "/agent/v1/providers", Some(body), &[("authorization", &auth_headers(tokens))])).await
    }

    #[tokio::test]
    async fn creates_a_provider_with_its_internal_account_row() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "create@example.com").await;
        let tokens = sign_in_device(&state, &user, "test-box").await;

        let response = create(&state, &tokens, json!({ "slug": "my-mac", "format": "openai" })).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let created = body_json(response).await;
        assert_eq!(created["slug"], "my-mac");

        let row = get_cli_provider_by_id(state.pool(), &user.id, created["id"].as_str().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.device_id.as_deref(), Some(tokens.device_id.as_str()));
        let accounts = list_accounts(state.pool(), &user.id, "my-mac").await.unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].user_id, user.id);
    }

    #[tokio::test]
    async fn stores_the_expose_whitelist_and_initial_models_within_bounds() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "models@example.com").await;
        let tokens = sign_in_device(&state, &user, "box").await;

        let response = create(
            &state,
            &tokens,
            json!({ "slug": "box", "format": "anthropic", "expose": ["a", "b"], "initial_models": ["a", "b", "c"] }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let id = body_json(response).await["id"].as_str().unwrap().to_string();
        let row: CliProviderRow = get_cli_provider_by_id(state.pool(), &user.id, &id).await.unwrap().unwrap();
        assert_eq!(crate::db::cli::parse_cli_models(row.models_json.as_deref()), vec!["a", "b", "c"]);
        assert_eq!(crate::db::cli::parse_cli_models(row.model_filter_json.as_deref()), vec!["a", "b"]);

        let bad = create(&state, &tokens, json!({ "slug": "box2", "format": "openai", "expose": ["bad id"] })).await;
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
        let bad = create(&state, &tokens, json!({ "slug": "box3", "format": "openai", "initial_models": [1] })).await;
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn shares_the_slug_namespace_and_the_cap_with_custom_providers() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "cap@example.com").await;
        let tokens = sign_in_device(&state, &user, "box").await;
        insert_custom_provider(
            state.pool(),
            NewCustomProvider {
                user_id: &user.id,
                slug: "taken",
                name: "Taken",
                format: "openai",
                base_url: "https://u.example.com/v1",
                count_tokens_url: None,
                models_mode: "auto",
                manual_models_json: None,
            },
        )
        .await
        .unwrap()
        .unwrap();

        let conflict = create(&state, &tokens, json!({ "slug": "taken", "format": "openai" })).await;
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        // A reserved slug never reaches storage.
        let reserved = create(&state, &tokens, json!({ "slug": "claude-code", "format": "openai" })).await;
        assert_eq!(reserved.status(), StatusCode::BAD_REQUEST);

        for i in 0..19 {
            let response = create(&state, &tokens, json!({ "slug": format!("p-{i}"), "format": "openai" })).await;
            assert_eq!(response.status(), StatusCode::CREATED, "provider {i}");
        }
        let over = create(&state, &tokens, json!({ "slug": "one-more", "format": "openai" })).await;
        assert_eq!(over.status(), StatusCode::BAD_REQUEST);
        assert!(body_json(over).await["error"].as_str().unwrap().contains("maximum"));
    }

    /// The cap holds inside the INSERT even when the pre-check read was stale
    /// (the TypeScript race test, exercised through the db seam it guards).
    #[tokio::test]
    async fn the_shared_cap_holds_inside_the_insert() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "race@example.com").await;
        for i in 0..20 {
            insert_custom_provider(
                state.pool(),
                NewCustomProvider {
                    user_id: &user.id,
                    slug: &format!("raced-{i}"),
                    name: "Raced",
                    format: "openai",
                    base_url: "https://u.example.com/v1",
                    count_tokens_url: None,
                    models_mode: "auto",
                    manual_models_json: None,
                },
            )
            .await
            .unwrap();
        }
        let row = insert_cli_provider(
            state.pool(),
            NewCliProvider {
                user_id: &user.id,
                device_id: None,
                slug: "one-more",
                name: "one-more",
                format: "openai",
                models_json: None,
                model_filter_json: None,
            },
        )
        .await
        .unwrap();
        assert!(row.is_none());
    }

    #[tokio::test]
    async fn lists_providers_as_disconnected_without_a_socket() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "list@example.com").await;
        let tokens = sign_in_device(&state, &user, "test-box").await;
        create(&state, &tokens, json!({ "slug": "my-mac", "format": "openai" })).await;

        let response = send(
            &state,
            json_req("GET", "/agent/v1/providers", None, &[("authorization", &auth_headers(&tokens))]),
        )
        .await;
        let json = body_json(response).await;
        assert_eq!(json["providers"].as_array().unwrap().len(), 1);
        assert_eq!(json["providers"][0]["slug"], "my-mac");
        assert_eq!(json["providers"][0]["connected"], false);
        assert_eq!(json["providers"][0]["device_name"], "test-box");
    }

    #[tokio::test]
    async fn delete_removes_the_provider_and_its_account_rows() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "rm@example.com").await;
        let tokens = sign_in_device(&state, &user, "box").await;
        let created = body_json(create(&state, &tokens, json!({ "slug": "my-mac", "format": "openai" })).await).await;
        let id = created["id"].as_str().unwrap().to_string();

        let response = send(
            &state,
            json_req("DELETE", &format!("/agent/v1/providers/{id}"), None, &[("authorization", &auth_headers(&tokens))]),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(get_cli_provider_by_id(state.pool(), &user.id, &id).await.unwrap().is_none());
        assert!(list_accounts(state.pool(), &user.id, "my-mac").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_revoked_devices_still_valid_token_is_rejected() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "rev@example.com").await;
        let tokens = sign_in_device(&state, &user, "box").await;
        crate::db::cli::revoke_cli_device(state.pool(), &user.id, &tokens.device_id).await.unwrap();

        let response = send(
            &state,
            json_req("GET", "/agent/v1/providers", None, &[("authorization", &auth_headers(&tokens))]),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn the_agent_surface_rejects_a_missing_or_garbage_bearer_token() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        for headers in [vec![], vec![("authorization", "Bearer nonsense")], vec![("authorization", "Basic x")]] {
            let response = send(&state, json_req("GET", "/agent/v1/providers", None, &headers)).await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn connect_requires_a_websocket_upgrade() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "upgrade@example.com").await;
        let tokens = sign_in_device(&state, &user, "box").await;
        let created = body_json(create(&state, &tokens, json!({ "slug": "my-mac", "format": "openai" })).await).await;
        let id = created["id"].as_str().unwrap().to_string();

        let response = send(
            &state,
            json_req("GET", &format!("/agent/v1/connect/{id}"), None, &[("authorization", &auth_headers(&tokens))]),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
    }

    /// The upgrade path end to end: a real WebSocket client against a bound axum
    /// server, as tunnel/ws.rs drives it. Only an authenticated device with its own
    /// provider gets the `hello` frame.
    #[tokio::test]
    async fn the_connect_route_authenticates_before_it_upgrades() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = AppState::builder(crate::db::test_support::test_config(), pool.clone())
            .transport(MockTransport::new())
            .tunnels(super::super::cli_shared::tunnel_registry_for(pool))
            .build();
        let user = insert_user(state.pool(), "ws@example.com").await;
        let stranger = insert_user(state.pool(), "stranger@example.com").await;
        let tokens = sign_in_device(&state, &user, "box").await;
        let other = sign_in_device(&state, &stranger, "their-box").await;
        let created = body_json(create(&state, &tokens, json!({ "slug": "my-mac", "format": "openai" })).await).await;
        let provider_id = created["id"].as_str().unwrap().to_string();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = test_router(state.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        let dial = |provider: &str, bearer: Option<String>| {
            let mut request = format!("ws://{addr}/agent/v1/connect/{provider}")
                .into_client_request()
                .unwrap();
            if let Some(bearer) = bearer {
                request.headers_mut().insert("authorization", bearer.parse().unwrap());
            }
            tokio_tungstenite::connect_async(request)
        };

        // Another user's device never reaches the provider's socket.
        assert!(dial(&provider_id, Some(auth_headers(&other))).await.is_err(), "a foreign device is refused");
        assert!(dial(&provider_id, None).await.is_err(), "an unauthenticated dial is refused");
        assert!(dial("cliprov_missing", Some(auth_headers(&tokens))).await.is_err());

        let (mut client, _) = dial(&provider_id, Some(auth_headers(&tokens))).await.expect("the tunnel accepts");
        let hello = tokio::time::timeout(Duration::from_secs(5), client.next())
            .await
            .expect("a hello within 5s")
            .unwrap()
            .unwrap();
        let hello: Value = serde_json::from_str(hello.to_text().unwrap()).unwrap();
        assert_eq!(hello["t"], "hello");
        assert_eq!(hello["slug"], "my-mac");
        assert!(state.tunnels().is_connected(&provider_id), "the registry holds the live socket");

        // Connecting stamps the device's last_seen_at (the CLI's `status` read).
        let device = get_cli_device(state.pool(), &tokens.device_id).await.unwrap().unwrap();
        assert!(device.last_seen_at.is_some());

        // Deleting the provider closes the live socket.
        let row = get_cli_provider_by_id(state.pool(), &user.id, &provider_id).await.unwrap().unwrap();
        remove_cli_provider(&state, &user.id, &row).await.unwrap();
        assert!(!state.tunnels().is_connected(&provider_id));
    }
}
