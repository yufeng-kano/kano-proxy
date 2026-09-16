//! `/api/custom-providers`, the
//! session-authenticated CRUD behind the admin's BYO endpoints
//! (docs/providers.md § Custom providers).
//!
//! Every user-supplied URL passes the SSRF/loop guard on create, update and test; the stored
//! API key is returned only as a non-secret mask, never in plaintext.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::auth::session::SessionUser;
use crate::crypto::token_crypto::{decrypt_json, encrypt_json};
use crate::db::accounts::{
    delete_accounts_for_provider, insert_account, list_accounts, update_account_payload, AccountIdentity, AccountRow,
    NewAccount,
};
use crate::db::cli::{count_cli_providers, get_cli_provider_by_slug};
use crate::db::custom_providers::{
    count_custom_providers, delete_custom_provider, get_custom_provider_by_id, get_custom_provider_by_slug,
    insert_custom_provider, list_custom_providers, reorder_custom_providers, update_custom_provider_fields,
    CustomProviderPatch, CustomProviderRow, NewCustomProvider,
};
use crate::pool::bench::{bench_until_from_row, clear_bench};
use crate::pool::StoredCredential;
use crate::utils::custom_provider::{
    is_custom_provider_format, is_models_mode, mask_api_key, parse_manual_models, validate_api_key,
    validate_base_url_length, validate_manual_models, validate_name, CustomProviderFormat,
    MAX_CUSTOM_PROVIDERS_PER_USER,
};
use crate::utils::upstream_url::{validate_upstream_base_url, UpstreamUrlCheckOpts};
use crate::upstream::UpstreamRequest;
use crate::AppState;

/// The test-connection probe never waits longer than this for the models endpoint.
const TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list).post(create))
        .route("/order", put(reorder))
        .route("/test", post(test_connection))
        .route("/{id}", put(update).delete(remove))
        .route("/{id}/unpause", post(unpause))
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

fn bad_request(message: &str) -> Response {
    error(StatusCode::BAD_REQUEST, message)
}

fn invalid_json() -> Response {
    bad_request("invalid JSON")
}

/// `try { await c.req.json() } catch { return 400 "invalid JSON" }`, the guard every writing
/// handler here opened with.
fn parse_body(bytes: &Bytes) -> Option<Value> {
    serde_json::from_slice(bytes).ok()
}

/// Bare lowercase hostnames for the SSRF/loop guard's "own host" check.
fn hosts_from_request(state: &AppState, headers: &HeaderMap) -> UpstreamUrlCheckOpts {
    let header = headers
        .get(axum::http::header::HOST)
        .or_else(|| headers.get("x-forwarded-host"))
        .and_then(|v| v.to_str().ok());
    let request_host = header.map(|h| h.split(':').next().unwrap_or(h).to_ascii_lowercase());
    let app_url_host = url::Url::parse(&state.config().app_url).ok().and_then(|u| u.host_str().map(str::to_lowercase));
    UpstreamUrlCheckOpts { request_host, app_url_host, field_name: None }
}

fn with_field(opts: &UpstreamUrlCheckOpts, field: &str) -> UpstreamUrlCheckOpts {
    UpstreamUrlCheckOpts { field_name: Some(field.to_string()), ..opts.clone() }
}

/// Only two states surfaced for custom cards — no standby/unusable nuance.
fn compute_status(accounts: &[AccountRow], now_ms: i64) -> &'static str {
    if accounts.iter().any(|a| bench_until_from_row(a, now_ms).is_none()) {
        "active"
    } else {
        "benched"
    }
}

fn key_mask_from_account(row: Option<&AccountRow>) -> Option<String> {
    let raw = row?.account_meta_json.as_deref()?;
    let meta: Value = serde_json::from_str(raw).ok()?;
    meta.get("key_mask").and_then(Value::as_str).map(str::to_string)
}

async fn to_list_item(state: &AppState, user_id: &str, row: &CustomProviderRow) -> Result<Value, sqlx::Error> {
    let accounts = list_accounts(state.pool(), user_id, &row.slug).await?;
    Ok(json!({
        "id": row.id,
        "slug": row.slug,
        "name": row.name,
        "format": row.format,
        "base_url": row.base_url,
        "count_tokens_url": row.count_tokens_url,
        "models_mode": row.models_mode,
        "manual_models": parse_manual_models(row.manual_models_json.as_deref()),
        "sort_order": row.sort_order,
        "key_mask": key_mask_from_account(accounts.first()),
        // The provider's single upstream_accounts row (its stored API key) — lets the Groups
        // picker pin a target to this endpoint's key like any other account (docs/auth.md).
        // null if the account row is somehow missing.
        "account_id": accounts.first().map(|a| a.id.clone()),
        "status": compute_status(&accounts, state.now_ms()),
        "created_at": row.created_at,
        "updated_at": row.updated_at,
    }))
}

async fn list_items(state: &AppState, user_id: &str, rows: &[CustomProviderRow]) -> Result<Vec<Value>, sqlx::Error> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(to_list_item(state, user_id, row).await?);
    }
    Ok(out)
}

// ------------------------------------------------------------------------------- GET /

async fn list(State(state): State<AppState>, session: SessionUser) -> Response {
    let Ok(rows) = list_custom_providers(state.pool(), &session.user.id).await else {
        return internal_error();
    };
    match list_items(&state, &session.user.id, &rows).await {
        Ok(providers) => Json(json!({ "providers": providers })).into_response(),
        Err(_) => internal_error(),
    }
}

// ------------------------------------------------------------------------------ POST /

async fn create(
    State(state): State<AppState>,
    session: SessionUser,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if state.config().token_encryption_key.is_none() {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "TOKEN_ENCRYPTION_KEY not configured");
    }
    let Some(Value::Object(body)) = parse_body(&body) else { return invalid_json() };

    let text = |key: &str| body.get(key).and_then(Value::as_str).unwrap_or("");
    let name = text("name").trim().to_string();
    let slug = text("slug").trim().to_lowercase();
    let base_url_raw = text("base_url").trim().to_string();
    let count_tokens_url_raw = text("count_tokens_url").trim().to_string();
    let api_key = text("api_key").to_string();
    let models_mode = body.get("models_mode").cloned().unwrap_or(json!("auto"));
    let format_value = body.get("format").cloned().unwrap_or(Value::Null);

    if !is_custom_provider_format(&format_value) {
        return bad_request("format must be 'openai' or 'anthropic'");
    }
    let format = format_value.as_str().expect("checked above").to_string();
    if let Some(err) = crate::utils::custom_provider::validate_slug(&slug) {
        return bad_request(&err);
    }
    if let Some(err) = validate_name(&name) {
        return bad_request(&err);
    }
    if let Some(err) = validate_api_key(&api_key) {
        return bad_request(&err);
    }
    if !is_models_mode(&models_mode) {
        return bad_request("models_mode must be 'auto' or 'manual'");
    }
    let manual_models = match validate_manual_models(body.get("manual_models")) {
        Ok(models) => models,
        Err(err) => return bad_request(&err),
    };
    if let Some(err) = validate_base_url_length(&base_url_raw, "base_url") {
        return bad_request(&err);
    }

    let opts = hosts_from_request(&state, &headers);
    let base_url = match validate_upstream_base_url(&base_url_raw, &opts) {
        Ok(url) => url,
        Err(err) => return bad_request(&err),
    };

    // An anthropic-format provider already derives count_tokens from base_url — setting the
    // field there is rejected rather than silently stored.
    let mut count_tokens_url: Option<String> = None;
    if !count_tokens_url_raw.is_empty() {
        if format != "openai" {
            return bad_request("count_tokens_url is only supported when format is \"openai\"");
        }
        if let Some(err) = validate_base_url_length(&count_tokens_url_raw, "count_tokens_url") {
            return bad_request(&err);
        }
        match validate_upstream_base_url(&count_tokens_url_raw, &with_field(&opts, "count_tokens_url")) {
            Ok(url) => count_tokens_url = Some(url),
            Err(err) => return bad_request(&err),
        }
    }

    // The 20-per-user provider budget and slug namespace are shared with CLI providers — both
    // kinds resolve from the same `<slug>/<model>` position (docs/cli.md § Data model).
    let (Ok(custom_count), Ok(cli_count)) = (
        count_custom_providers(state.pool(), &session.user.id).await,
        count_cli_providers(state.pool(), &session.user.id).await,
    ) else {
        return internal_error();
    };
    let cap_message =
        format!("maximum of {MAX_CUSTOM_PROVIDERS_PER_USER} providers reached (custom + CLI)");
    if custom_count + cli_count >= MAX_CUSTOM_PROVIDERS_PER_USER as i64 {
        return bad_request(&cap_message);
    }

    match get_custom_provider_by_slug(state.pool(), &session.user.id, &slug).await {
        Err(_) => return internal_error(),
        Ok(Some(_)) => return error(StatusCode::CONFLICT, &format!("slug \"{slug}\" is already in use")),
        Ok(None) => {}
    }
    let cli_taken = match get_cli_provider_by_slug(state.pool(), &session.user.id, &slug).await {
        Ok(row) => row.is_some(),
        Err(_) => return internal_error(),
    };
    if cli_taken {
        return error(StatusCode::CONFLICT, &format!("slug \"{slug}\" is already in use by a CLI provider"));
    }

    let manual_models_json = (!manual_models.is_empty()).then(|| json!(manual_models).to_string());
    let inserted = insert_custom_provider(
        state.pool(),
        NewCustomProvider {
            user_id: &session.user.id,
            slug: &slug,
            name: &name,
            format: &format,
            base_url: &base_url,
            count_tokens_url: count_tokens_url.as_deref(),
            models_mode: models_mode.as_str().unwrap_or("auto"),
            manual_models_json: manual_models_json.as_deref(),
        },
    )
    .await;
    let row = match inserted {
        Err(_) => return internal_error(),
        Ok(Some(row)) => row,
        // The atomic guard refused — distinguish a raced CLI slug from a raced cap.
        Ok(None) => {
            let raced_cli = matches!(get_cli_provider_by_slug(state.pool(), &session.user.id, &slug).await, Ok(Some(_)));
            return if raced_cli {
                error(StatusCode::CONFLICT, &format!("slug \"{slug}\" is already in use by a CLI provider"))
            } else {
                bad_request(&cap_message)
            };
        }
    };

    let credential = StoredCredential { access_token: api_key.clone(), ..Default::default() };
    let Ok(encrypted) = encrypt_json(state.config().token_encryption_key.as_deref(), &credential) else {
        return internal_error();
    };
    let meta = json!({ "key_mask": mask_api_key(&api_key) }).to_string();
    if insert_account(
        state.pool(),
        NewAccount {
            user_id: &session.user.id,
            provider: &slug,
            encrypted_payload: &encrypted,
            label: Some(&name),
            external_account_id: None,
            account_meta_json: Some(&meta),
            priority: None,
        },
    )
    .await
    .is_err()
    {
        return internal_error();
    }

    match to_list_item(&state, &session.user.id, &row).await {
        Ok(item) => (StatusCode::CREATED, Json(item)).into_response(),
        Err(_) => internal_error(),
    }
}

// -------------------------------------------------------------------------- PUT /order

async fn reorder(State(state): State<AppState>, session: SessionUser, body: Bytes) -> Response {
    let Some(body) = parse_body(&body) else { return invalid_json() };
    let Value::Object(body) = body else { return bad_request("body must be an object") };
    let Some(Value::Array(ids)) = body.get("ids") else {
        return bad_request("ids must be an array of strings");
    };
    if ids.iter().any(|id| !id.is_string()) {
        return bad_request("ids must be an array of strings");
    }
    let ids: Vec<String> = ids.iter().map(|id| id.as_str().unwrap_or_default().to_string()).collect();

    let Ok(rows) = list_custom_providers(state.pool(), &session.user.id).await else {
        return internal_error();
    };
    let expected: std::collections::HashSet<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    let received: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
    if received.len() != ids.len() || received.len() != expected.len() || ids.iter().any(|id| !expected.contains(id.as_str()))
    {
        return bad_request("ids must list every custom provider exactly once");
    }

    if reorder_custom_providers(state.pool(), &session.user.id, &ids).await.is_err() {
        return internal_error();
    }
    let Ok(updated) = list_custom_providers(state.pool(), &session.user.id).await else {
        return internal_error();
    };
    match list_items(&state, &session.user.id, &updated).await {
        Ok(providers) => Json(json!({ "providers": providers })).into_response(),
        Err(_) => internal_error(),
    }
}

// ---------------------------------------------------------------------------- PUT /:id

async fn update(
    State(state): State<AppState>,
    session: SessionUser,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let existing = match get_custom_provider_by_id(state.pool(), &session.user.id, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return not_found(),
        Err(_) => return internal_error(),
    };
    let Some(Value::Object(body)) = parse_body(&body) else { return invalid_json() };

    if body.get("slug").is_some_and(|v| v.as_str() != Some(existing.slug.as_str())) {
        return bad_request("slug is immutable");
    }
    if body.get("format").is_some_and(|v| v.as_str() != Some(existing.format.as_str())) {
        return bad_request("format is immutable");
    }

    let mut name: Option<String> = None;
    if let Some(value) = body.get("name") {
        let trimmed = value.as_str().unwrap_or("").trim().to_string();
        if let Some(err) = validate_name(&trimmed) {
            return bad_request(&err);
        }
        name = Some(trimmed);
    }

    let opts = hosts_from_request(&state, &headers);
    let mut base_url: Option<String> = None;
    if let Some(value) = body.get("base_url") {
        let raw = value.as_str().unwrap_or("").trim().to_string();
        if let Some(err) = validate_base_url_length(&raw, "base_url") {
            return bad_request(&err);
        }
        match validate_upstream_base_url(&raw, &opts) {
            Ok(url) => base_url = Some(url),
            Err(err) => return bad_request(&err),
        }
    }

    // Omitted keeps the stored value; "" or null clears it (the only way back to
    // "unsupported"); a non-empty value validates and replaces it. Checked against the stored
    // (immutable) format, not any format in this body.
    let mut count_tokens_url: Option<Option<String>> = None;
    if let Some(value) = body.get("count_tokens_url") {
        let raw = value.as_str().unwrap_or("").trim().to_string();
        if raw.is_empty() {
            count_tokens_url = Some(None);
        } else {
            if existing.format != "openai" {
                return bad_request("count_tokens_url is only supported when format is \"openai\"");
            }
            if let Some(err) = validate_base_url_length(&raw, "count_tokens_url") {
                return bad_request(&err);
            }
            match validate_upstream_base_url(&raw, &with_field(&opts, "count_tokens_url")) {
                Ok(url) => count_tokens_url = Some(Some(url)),
                Err(err) => return bad_request(&err),
            }
        }
    }

    let mut models_mode: Option<String> = None;
    if let Some(value) = body.get("models_mode") {
        if !is_models_mode(value) {
            return bad_request("models_mode must be 'auto' or 'manual'");
        }
        models_mode = value.as_str().map(str::to_string);
    }

    let mut manual_models_json: Option<String> = None;
    if body.get("manual_models").is_some() {
        match validate_manual_models(body.get("manual_models")) {
            Ok(models) => manual_models_json = Some(json!(models).to_string()),
            Err(err) => return bad_request(&err),
        }
    }

    // Blank/omitted api_key means "keep the stored key" — never echoed back, so the admin UI
    // cannot round-trip it, and a no-op edit must not error.
    let mut api_key: Option<String> = None;
    if let Some(value) = body.get("api_key").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        if let Some(err) = validate_api_key(value) {
            return bad_request(&err);
        }
        api_key = Some(value.to_string());
    }

    if update_custom_provider_fields(
        state.pool(),
        &id,
        CustomProviderPatch {
            name: name.as_deref(),
            base_url: base_url.as_deref(),
            count_tokens_url: count_tokens_url.as_ref().map(|v| v.as_deref()),
            models_mode: models_mode.as_deref(),
            manual_models_json: manual_models_json.as_deref(),
        },
    )
    .await
    .is_err()
    {
        return internal_error();
    }

    if let Some(api_key) = api_key {
        if state.config().token_encryption_key.is_none() {
            return error(StatusCode::INTERNAL_SERVER_ERROR, "TOKEN_ENCRYPTION_KEY not configured");
        }
        let Ok(rows) = list_accounts(state.pool(), &session.user.id, &existing.slug).await else {
            return internal_error();
        };
        let credential = StoredCredential { access_token: api_key.clone(), ..Default::default() };
        let Ok(encrypted) = encrypt_json(state.config().token_encryption_key.as_deref(), &credential) else {
            return internal_error();
        };
        let meta = json!({ "key_mask": mask_api_key(&api_key) }).to_string();
        let wrote = match rows.first() {
            Some(row) => update_account_payload(
                state.pool(),
                &row.id,
                &encrypted,
                Some(AccountIdentity { label: None, account_meta_json: Some(&meta) }),
            )
            .await
            .is_ok(),
            None => insert_account(
                state.pool(),
                NewAccount {
                    user_id: &session.user.id,
                    provider: &existing.slug,
                    encrypted_payload: &encrypted,
                    label: Some(name.as_deref().unwrap_or(&existing.name)),
                    external_account_id: None,
                    account_meta_json: Some(&meta),
                    priority: None,
                },
            )
            .await
            .is_ok(),
        };
        if !wrote {
            return internal_error();
        }
    }

    let updated = match get_custom_provider_by_id(state.pool(), &session.user.id, &id).await {
        Ok(row) => row.unwrap_or(existing),
        Err(_) => return internal_error(),
    };
    match to_list_item(&state, &session.user.id, &updated).await {
        Ok(item) => Json(item).into_response(),
        Err(_) => internal_error(),
    }
}

// ------------------------------------------------------------------------- DELETE /:id

async fn remove(State(state): State<AppState>, session: SessionUser, Path(id): Path<String>) -> Response {
    let existing = match get_custom_provider_by_id(state.pool(), &session.user.id, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return not_found(),
        Err(_) => return internal_error(),
    };

    // Accounts first: if this crashes partway, a leftover account row without a provider row is
    // unreachable dead data — the reverse order would leave it silently acquirable by the pool.
    let Ok(removed) = delete_accounts_for_provider(state.pool(), &session.user.id, &existing.slug).await else {
        return internal_error();
    };
    if delete_custom_provider(state.pool(), &session.user.id, &id).await.is_err() {
        return internal_error();
    }
    for account in &removed {
        // Best-effort: the rows are already gone, so a failed bench clear changes nothing.
        let _ = clear_bench(state.pool(), &session.user.id, &existing.slug, &account.id).await;
    }
    Json(json!({ "ok": true })).into_response()
}

// -------------------------------------------------------------------- POST /:id/unpause

async fn unpause(State(state): State<AppState>, session: SessionUser, Path(id): Path<String>) -> Response {
    let existing = match get_custom_provider_by_id(state.pool(), &session.user.id, &id).await {
        Ok(Some(row)) => row,
        Ok(None) => return not_found(),
        Err(_) => return internal_error(),
    };
    let Ok(accounts) = list_accounts(state.pool(), &session.user.id, &existing.slug).await else {
        return internal_error();
    };
    for account in &accounts {
        if clear_bench(state.pool(), &session.user.id, &existing.slug, &account.id).await.is_err() {
            return internal_error();
        }
    }
    Json(json!({ "ok": true })).into_response()
}

// ----------------------------------------------------------------------------- POST /test

/// GET the models endpoint for a format and map the outcome. Never echoes the key or body.
pub async fn test_custom_provider_connection(
    state: &AppState,
    format: CustomProviderFormat,
    base_url: &str,
    api_key: &str,
) -> Value {
    let url = match format {
        CustomProviderFormat::Anthropic => format!("{base_url}/v1/models"),
        CustomProviderFormat::Openai => format!("{base_url}/models"),
    };
    let request = match format {
        CustomProviderFormat::Anthropic => {
            UpstreamRequest::get(url).header("x-api-key", api_key).header("anthropic-version", "2023-06-01")
        }
        CustomProviderFormat::Openai => UpstreamRequest::get(url).header("authorization", &format!("Bearer {api_key}")),
    }
    .timeout(TEST_TIMEOUT);

    let Ok(response) = state.transport().send(request).await else {
        return json!({ "ok": false, "error": "unreachable/timeout" });
    };
    let status = response.status;
    if status == StatusCode::NOT_FOUND {
        return json!({
            "ok": true,
            "models_count": Value::Null,
            "note": "reachable; no models endpoint — use manual model ids",
        });
    }
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return json!({ "ok": false, "error": format!("auth rejected ({})", status.as_u16()) });
    }
    if !status.is_success() {
        return json!({ "ok": false, "error": format!("HTTP {}", status.as_u16()) });
    }
    let json_body = response.json_value().await.unwrap_or(Value::Null);
    let ids: Vec<String> = json_body
        .get("data")
        .and_then(Value::as_array)
        .map(|models| {
            models.iter().filter_map(|m| m.get("id").and_then(Value::as_str).filter(|s| !s.is_empty())).map(str::to_string).collect()
        })
        .unwrap_or_default();
    json!({ "ok": true, "models_count": ids.len(), "sample": ids.iter().take(5).collect::<Vec<_>>() })
}

async fn test_connection(
    State(state): State<AppState>,
    session: SessionUser,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(Value::Object(body)) = parse_body(&body) else { return invalid_json() };

    let format;
    let base_url;
    let api_key;
    match body.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(id) => {
            let row = match get_custom_provider_by_id(state.pool(), &session.user.id, id).await {
                Ok(Some(row)) => row,
                Ok(None) => return not_found(),
                Err(_) => return internal_error(),
            };
            let Some(parsed) = CustomProviderFormat::parse(&row.format) else { return internal_error() };
            format = parsed;
            base_url = body
                .get("base_url")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(&row.base_url)
                .to_string();
            let Ok(accounts) = list_accounts(state.pool(), &session.user.id, &row.slug).await else {
                return internal_error();
            };
            let Some(account) = accounts.first() else {
                return Json(json!({ "ok": false, "error": "no stored key for this provider" })).into_response();
            };
            if state.config().token_encryption_key.is_none() {
                return error(StatusCode::INTERNAL_SERVER_ERROR, "TOKEN_ENCRYPTION_KEY not configured");
            }
            let credential: StoredCredential =
                match decrypt_json(state.config().token_encryption_key.as_deref(), &account.encrypted_payload) {
                    Ok(credential) => credential,
                    Err(_) => return internal_error(),
                };
            api_key = credential.access_token;
        }
        None => {
            let format_value = body.get("format").cloned().unwrap_or(Value::Null);
            if !is_custom_provider_format(&format_value) {
                return bad_request("format must be 'openai' or 'anthropic'");
            }
            format = CustomProviderFormat::parse(format_value.as_str().unwrap_or_default()).expect("checked above");
            base_url = body.get("base_url").and_then(Value::as_str).unwrap_or("").trim().to_string();
            api_key = body.get("api_key").and_then(Value::as_str).unwrap_or("").to_string();
            if api_key.is_empty() {
                return bad_request("api_key is required");
            }
        }
    }

    let opts = hosts_from_request(&state, &headers);
    let checked = match validate_upstream_base_url(&base_url, &opts) {
        Ok(url) => url,
        Err(err) => return bad_request(&err),
    };
    Json(test_custom_provider_connection(&state, format, &checked, &api_key).await).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::session::create_session;
    use crate::db::test_support::{insert_user, skip_without_db, test_pool, test_router, test_state, TEST_TOKEN_KEY};
    use crate::upstream::{MockTransport, TransportError, UpstreamResponse};
    use axum::body::Body;
    use axum::http::{header, Request};
    use http_body_util::BodyExt;
    use sqlx::PgPool;
    use tower::ServiceExt;

    const API_KEY: &str = "sk-upstream-secret-key-value";

    async fn body_json(response: Response) -> Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn signed_in(state: &AppState, email: &str) -> (crate::db::users::UserRow, String) {
        let user = insert_user(state.pool(), email).await;
        let (_, cookie) = create_session(state, &user.id, false).await.unwrap();
        (user, cookie.split(';').next().unwrap().to_string())
    }

    fn request(method: &str, uri: &str, cookie: Option<&str>, body: Option<Value>) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, "app.example.com")
            .header(header::ORIGIN, "https://app.example.com")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        builder.body(body.map(|b| Body::from(b.to_string())).unwrap_or_else(Body::empty)).unwrap()
    }

    async fn call(state: &AppState, req: Request<Body>) -> Response {
        test_router(state.clone()).oneshot(req).await.unwrap()
    }

    fn valid_create_body() -> Value {
        json!({
            "name": "My Endpoint",
            "slug": "my-endpoint",
            "format": "openai",
            "base_url": "https://upstream.example.com/v1",
            "api_key": API_KEY,
        })
    }

    fn with(overrides: Value) -> Value {
        let mut body = valid_create_body();
        for (k, v) in overrides.as_object().unwrap() {
            body[k] = v.clone();
        }
        body
    }

    async fn create_provider(state: &AppState, cookie: &str, overrides: Value) -> Response {
        call(state, request("POST", "/api/custom-providers", Some(cookie), Some(with(overrides)))).await
    }

    async fn created(state: &AppState, cookie: &str, overrides: Value) -> Value {
        let res = create_provider(state, cookie, overrides).await;
        assert_eq!(res.status(), StatusCode::CREATED);
        body_json(res).await
    }

    async fn listed(state: &AppState, cookie: &str) -> Value {
        body_json(call(state, request("GET", "/api/custom-providers", Some(cookie), None)).await).await
    }

    async fn stored_accounts(pool: &PgPool, user_id: &str, slug: &str) -> Vec<AccountRow> {
        list_accounts(pool, user_id, slug).await.unwrap()
    }

    // --------------------------------------------------------------- POST / (create)

    #[tokio::test]
    async fn create_returns_201_with_a_masked_key_and_never_the_raw_key() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool.clone(), MockTransport::new());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;

        let anon = call(&state, request("POST", "/api/custom-providers", None, None)).await;
        assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);

        let json = created(&state, &cookie, json!({})).await;
        assert_eq!(json["slug"], "my-endpoint");
        assert_eq!(json["name"], "My Endpoint");
        assert_eq!(json["format"], "openai");
        assert_eq!(json["base_url"], "https://upstream.example.com/v1");
        assert_eq!(json["models_mode"], "auto");
        assert_eq!(json["manual_models"], json!([]));
        assert_eq!(json["status"], "active");
        assert_eq!(json["key_mask"], "sk-ups…alue");
        assert_eq!(json["count_tokens_url"], Value::Null);
        assert!(!json.to_string().contains(API_KEY));
        // docs/auth.md: account_id is the provider's single upstream_accounts row (its stored
        // API key) — the Groups picker pins targets to it.
        let accounts = stored_accounts(&pool, &user.id, "my-endpoint").await;
        assert_eq!(json["account_id"], accounts[0].id);

        // The key is stored encrypted and decrypts back to the original value.
        let credential: StoredCredential = decrypt_json(Some(TEST_TOKEN_KEY), &accounts[0].encrypted_payload).unwrap();
        assert_eq!(credential.access_token, API_KEY);
    }

    #[tokio::test]
    async fn account_id_is_null_when_the_account_row_is_missing() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool.clone(), MockTransport::new());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        created(&state, &cookie, json!({})).await;

        // Simulate the account row going missing without the provider row itself being deleted
        // (should never happen via the normal API, but the field must degrade to null).
        delete_accounts_for_provider(&pool, &user.id, "my-endpoint").await.unwrap();
        assert_eq!(listed(&state, &cookie).await["providers"][0]["account_id"], Value::Null);
    }

    #[tokio::test]
    async fn create_refuses_every_invalid_field() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool.clone(), MockTransport::new());
        let (_, cookie) = signed_in(&state, "user_1@example.com").await;

        for bad in [
            json!({ "format": "openrouter" }),
            json!({ "slug": "grok" }),
            json!({ "slug": "a" }),
            json!({ "name": "" }),
            json!({ "api_key": "" }),
            json!({ "models_mode": "live" }),
            json!({ "manual_models": (0..101).map(|i| format!("m{i}")).collect::<Vec<_>>() }),
            json!({ "base_url": format!("https://upstream.example.com/{}", "a".repeat(290)) }),
            json!({ "base_url": "http://upstream.example.com/v1" }),
            // The SSRF/loop guard also refuses this deploy's own APP_URL host.
            json!({ "base_url": "https://app.example.com/v1" }),
        ] {
            let res = create_provider(&state, &cookie, bad.clone()).await;
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{bad} should be refused");
        }
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM custom_providers").fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0, "a refused create writes nothing");
    }

    #[tokio::test]
    async fn create_stores_manual_models_and_appends_new_providers_last() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool.clone(), MockTransport::new());
        let (_, cookie) = signed_in(&state, "user_1@example.com").await;

        let manual =
            created(&state, &cookie, json!({ "models_mode": "manual", "manual_models": ["model-a", "model-b"] })).await;
        assert_eq!(manual["manual_models"], json!(["model-a", "model-b"]));
        assert_eq!(manual["sort_order"], 0);

        let second = created(&state, &cookie, json!({ "slug": "second-endpoint", "name": "Second" })).await;
        assert_eq!(second["sort_order"], 1);
        let json = listed(&state, &cookie).await;
        let slugs: Vec<&str> =
            json["providers"].as_array().unwrap().iter().map(|p| p["slug"].as_str().unwrap()).collect();
        assert_eq!(slugs, vec!["my-endpoint", "second-endpoint"]);
    }

    #[tokio::test]
    async fn a_slug_is_unique_per_user_and_the_cap_is_shared_with_cli_providers() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool.clone(), MockTransport::new());
        let (_, cookie) = signed_in(&state, "user_1@example.com").await;
        created(&state, &cookie, json!({})).await;

        let duplicate = create_provider(&state, &cookie, json!({ "name": "Second" })).await;
        assert_eq!(duplicate.status(), StatusCode::CONFLICT);

        // The same slug is free for a different user.
        let (_, other_cookie) = signed_in(&state, "user_2@example.com").await;
        assert_eq!(create_provider(&state, &other_cookie, json!({})).await.status(), StatusCode::CREATED);

        // The 20-provider budget counts custom rows.
        for i in 0..19 {
            created(&state, &cookie, json!({ "slug": format!("existing-{i}"), "name": format!("Existing {i}") })).await;
        }
        let capped = create_provider(&state, &cookie, json!({ "slug": "one-too-many", "name": "Too many" })).await;
        assert_eq!(capped.status(), StatusCode::BAD_REQUEST);
    }

    // ------------------------------------------------------------------ GET / (list)

    #[tokio::test]
    async fn the_list_is_per_user_masked_and_reports_the_bench_state() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool.clone(), MockTransport::new());

        let anon = call(&state, request("GET", "/api/custom-providers", None, None)).await;
        assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);

        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        created(&state, &cookie, json!({})).await;
        let json = listed(&state, &cookie).await;
        assert_eq!(json["providers"].as_array().unwrap().len(), 1);
        assert_eq!(json["providers"][0]["slug"], "my-endpoint");
        assert_eq!(json["providers"][0]["status"], "active");
        assert_eq!(json["providers"][0]["key_mask"], "sk-ups…alue");
        assert!(!json.to_string().contains(API_KEY));

        let account_id = stored_accounts(&pool, &user.id, "my-endpoint").await[0].id.clone();
        crate::pool::bench::mark_benched(&pool, &user.id, "my-endpoint", &account_id, None, None, state.now_ms())
            .await
            .unwrap();
        assert_eq!(listed(&state, &cookie).await["providers"][0]["status"], "benched");

        // Another user sees none of it.
        let (_, other) = signed_in(&state, "user_2@example.com").await;
        assert_eq!(listed(&state, &other).await["providers"], json!([]));
    }

    #[tokio::test]
    async fn legacy_rows_with_a_zero_sort_order_fall_back_to_created_at() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool.clone(), MockTransport::new());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        for (id, slug, created_at) in [
            ("cprov_later", "later-endpoint", "2026-01-02T00:00:00.000Z"),
            ("cprov_earlier", "earlier-endpoint", "2026-01-01T00:00:00.000Z"),
        ] {
            sqlx::query(
                "INSERT INTO custom_providers (id, user_id, slug, name, format, base_url, count_tokens_url,
                 models_mode, manual_models_json, sort_order, created_at, updated_at)
                 VALUES ($1, $2, $3, $3, 'openai', 'https://x.example.com/v1', NULL, 'auto', NULL, 0, $4, $4)",
            )
            .bind(id)
            .bind(&user.id)
            .bind(slug)
            .bind(created_at)
            .execute(&pool)
            .await
            .unwrap();
        }

        let json = listed(&state, &cookie).await;
        let slugs: Vec<&str> =
            json["providers"].as_array().unwrap().iter().map(|p| p["slug"].as_str().unwrap()).collect();
        assert_eq!(slugs, vec!["earlier-endpoint", "later-endpoint"]);
    }

    // ------------------------------------------------------------------ PUT /order

    #[tokio::test]
    async fn the_order_write_persists_the_requested_order_and_returns_the_list_shape() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool.clone(), MockTransport::new());

        let anon = call(&state, request("PUT", "/api/custom-providers/order", None, Some(json!({ "ids": [] })))).await;
        assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);

        let (_, cookie) = signed_in(&state, "user_1@example.com").await;
        let first = created(&state, &cookie, json!({ "slug": "first-endpoint", "name": "first" })).await["id"]
            .as_str()
            .unwrap()
            .to_string();
        let second = created(&state, &cookie, json!({ "slug": "second-endpoint", "name": "second" })).await["id"]
            .as_str()
            .unwrap()
            .to_string();

        let res = call(
            &state,
            request("PUT", "/api/custom-providers/order", Some(&cookie), Some(json!({ "ids": [second, first] }))),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let json = body_json(res).await;
        let ids: Vec<&str> = json["providers"].as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec![second.as_str(), first.as_str()]);
        let orders: Vec<i64> =
            json["providers"].as_array().unwrap().iter().map(|p| p["sort_order"].as_i64().unwrap()).collect();
        assert_eq!(orders, vec![0, 1]);
        let slugs: Vec<String> = listed(&state, &cookie).await["providers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["slug"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(slugs, vec!["second-endpoint".to_string(), "first-endpoint".to_string()]);
    }

    #[tokio::test]
    async fn a_malformed_id_list_is_refused_without_changing_the_stored_order() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool.clone(), MockTransport::new());
        let (_, cookie) = signed_in(&state, "user_1@example.com").await;
        let first = created(&state, &cookie, json!({ "slug": "first-endpoint", "name": "first" })).await["id"]
            .as_str()
            .unwrap()
            .to_string();
        let second = created(&state, &cookie, json!({ "slug": "second-endpoint", "name": "second" })).await["id"]
            .as_str()
            .unwrap()
            .to_string();
        // Another user's id is as foreign as an invented one.
        let (_, other_cookie) = signed_in(&state, "user_2@example.com").await;
        let theirs = created(&state, &other_cookie, json!({ "slug": "theirs", "name": "theirs" })).await["id"]
            .as_str()
            .unwrap()
            .to_string();
        let before: Vec<String> = listed(&state, &cookie).await["providers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["slug"].as_str().unwrap().to_string())
            .collect();

        for bad in [
            json!({ "ids": [second.clone()] }),
            json!({ "ids": [first.clone(), second.clone(), "cprov_foreign"] }),
            json!({ "ids": [first.clone(), first.clone()] }),
            json!({ "ids": [first.clone(), theirs] }),
            json!({ "ids": "not-an-array" }),
            json!({ "ids": [1, 2] }),
        ] {
            let res = call(&state, request("PUT", "/api/custom-providers/order", Some(&cookie), Some(bad.clone()))).await;
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{bad} should be refused");
            let after: Vec<String> = listed(&state, &cookie).await["providers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["slug"].as_str().unwrap().to_string())
                .collect();
            assert_eq!(after, before);
        }
    }

    // -------------------------------------------------------------------- PUT /:id

    #[tokio::test]
    async fn update_is_scoped_to_the_owner_and_keeps_the_stored_key_when_none_is_sent() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool.clone(), MockTransport::new());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;

        let unknown = call(
            &state,
            request("PUT", "/api/custom-providers/nonexistent", Some(&cookie), Some(json!({ "name": "x" }))),
        )
        .await;
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

        let id = created(&state, &cookie, json!({})).await["id"].as_str().unwrap().to_string();
        let (_, other_cookie) = signed_in(&state, "user_2@example.com").await;
        let theirs = call(
            &state,
            request("PUT", &format!("/api/custom-providers/{id}"), Some(&other_cookie), Some(json!({ "name": "x" }))),
        )
        .await;
        assert_eq!(theirs.status(), StatusCode::NOT_FOUND);

        let res = call(
            &state,
            request(
                "PUT",
                &format!("/api/custom-providers/{id}"),
                Some(&cookie),
                Some(json!({ "name": "Renamed", "base_url": "https://new-upstream.example.com/v1" })),
            ),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let json = body_json(res).await;
        assert_eq!(json["name"], "Renamed");
        assert_eq!(json["base_url"], "https://new-upstream.example.com/v1");
        assert_eq!(json["key_mask"], "sk-ups…alue");
        let accounts = stored_accounts(&pool, &user.id, "my-endpoint").await;
        // Same account row throughout — updates replace the key in place.
        assert_eq!(json["account_id"], accounts[0].id);
        let credential: StoredCredential = decrypt_json(Some(TEST_TOKEN_KEY), &accounts[0].encrypted_payload).unwrap();
        assert_eq!(credential.access_token, API_KEY);

        // A blank api_key keeps the stored key (no-op).
        let blank = call(
            &state,
            request("PUT", &format!("/api/custom-providers/{id}"), Some(&cookie), Some(json!({ "api_key": "" }))),
        )
        .await;
        assert_eq!(blank.status(), StatusCode::OK);
        let accounts = stored_accounts(&pool, &user.id, "my-endpoint").await;
        let credential: StoredCredential = decrypt_json(Some(TEST_TOKEN_KEY), &accounts[0].encrypted_payload).unwrap();
        assert_eq!(credential.access_token, API_KEY);
    }

    #[tokio::test]
    async fn update_replaces_the_stored_key_and_mask_in_place() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool.clone(), MockTransport::new());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        let id = created(&state, &cookie, json!({})).await["id"].as_str().unwrap().to_string();

        let res = call(
            &state,
            request(
                "PUT",
                &format!("/api/custom-providers/{id}"),
                Some(&cookie),
                Some(json!({ "api_key": "sk-brand-new-rotated-key-9999" })),
            ),
        )
        .await;
        let json = body_json(res).await;
        assert_ne!(json["key_mask"], "sk-ups…alue");
        let accounts = stored_accounts(&pool, &user.id, "my-endpoint").await;
        // Still exactly one account row — replaced in place, not duplicated.
        assert_eq!(accounts.len(), 1);
        let credential: StoredCredential = decrypt_json(Some(TEST_TOKEN_KEY), &accounts[0].encrypted_payload).unwrap();
        assert_eq!(credential.access_token, "sk-brand-new-rotated-key-9999");
    }

    #[tokio::test]
    async fn slug_and_format_are_immutable_and_a_bad_base_url_is_refused() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool.clone(), MockTransport::new());
        let (_, cookie) = signed_in(&state, "user_1@example.com").await;
        let id = created(&state, &cookie, json!({})).await["id"].as_str().unwrap().to_string();
        let uri = format!("/api/custom-providers/{id}");

        for bad in [
            json!({ "slug": "different-slug" }),
            json!({ "format": "anthropic" }),
            json!({ "base_url": "https://127.0.0.1/v1" }),
        ] {
            let res = call(&state, request("PUT", &uri, Some(&cookie), Some(bad.clone()))).await;
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{bad} should be refused");
        }

        // Re-sending the same slug/format is a no-op, not an error.
        let same = call(
            &state,
            request("PUT", &uri, Some(&cookie), Some(json!({ "slug": "my-endpoint", "format": "openai", "name": "Still fine" }))),
        )
        .await;
        assert_eq!(same.status(), StatusCode::OK);
    }

    // ------------------------------------------------------------- count_tokens_url

    #[tokio::test]
    async fn count_tokens_url_is_validated_on_create_and_only_for_the_openai_format() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool.clone(), MockTransport::new());
        let (_, cookie) = signed_in(&state, "user_1@example.com").await;

        let ok = created(&state, &cookie, json!({ "count_tokens_url": "https://count.example.com/anthropic/count_tokens" })).await;
        assert_eq!(ok["count_tokens_url"], "https://count.example.com/anthropic/count_tokens");

        for bad in [
            json!({ "slug": "b1", "count_tokens_url": "http://count.example.com/count_tokens" }),
            json!({ "slug": "b2", "count_tokens_url": "https://127.0.0.1/count_tokens" }),
            json!({ "slug": "b3", "count_tokens_url": format!("https://count.example.com/{}", "a".repeat(290)) }),
            json!({
                "slug": "b4",
                "format": "anthropic",
                "base_url": "https://upstream.example.com",
                "count_tokens_url": "https://count.example.com/count_tokens",
            }),
        ] {
            let res = create_provider(&state, &cookie, bad.clone()).await;
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{bad} should be refused");
        }
    }

    #[tokio::test]
    async fn count_tokens_url_is_kept_cleared_or_replaced_on_update() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool.clone(), MockTransport::new());
        let (_, cookie) = signed_in(&state, "user_1@example.com").await;
        let stored = "https://count.example.com/count_tokens";

        for (label, patch, expected) in [
            ("omitted keeps the stored value", json!({ "name": "Renamed" }), json!(stored)),
            ("the empty string clears it", json!({ "count_tokens_url": "" }), Value::Null),
            ("null clears it", json!({ "count_tokens_url": null }), Value::Null),
            (
                "a new value replaces it",
                json!({ "count_tokens_url": "https://other.example.com/count" }),
                json!("https://other.example.com/count"),
            ),
        ] {
            let id = created(
                &state,
                &cookie,
                json!({ "slug": format!("s{}", label.len()), "count_tokens_url": stored }),
            )
            .await["id"]
                .as_str()
                .unwrap()
                .to_string();
            let res = call(&state, request("PUT", &format!("/api/custom-providers/{id}"), Some(&cookie), Some(patch))).await;
            assert_eq!(res.status(), StatusCode::OK, "{label}");
            assert_eq!(body_json(res).await["count_tokens_url"], expected, "{label}");
        }

        // An invalid non-empty value is refused, and so is any value on a stored
        // anthropic-format provider.
        let openai = created(&state, &cookie, json!({ "slug": "openai-endpoint" })).await["id"].as_str().unwrap().to_string();
        let res = call(
            &state,
            request(
                "PUT",
                &format!("/api/custom-providers/{openai}"),
                Some(&cookie),
                Some(json!({ "count_tokens_url": "https://127.0.0.1/count_tokens" })),
            ),
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        let anthropic = created(
            &state,
            &cookie,
            json!({ "slug": "my-claude-endpoint", "format": "anthropic", "base_url": "https://upstream.example.com" }),
        )
        .await["id"]
            .as_str()
            .unwrap()
            .to_string();
        let res = call(
            &state,
            request(
                "PUT",
                &format!("/api/custom-providers/{anthropic}"),
                Some(&cookie),
                Some(json!({ "count_tokens_url": stored })),
            ),
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    // ----------------------------------------------------------------- DELETE /:id

    #[tokio::test]
    async fn delete_removes_the_provider_and_its_accounts_and_never_crosses_users() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool.clone(), MockTransport::new());
        let (user, cookie) = signed_in(&state, "user_1@example.com").await;

        let unknown = call(&state, request("DELETE", "/api/custom-providers/nonexistent", Some(&cookie), None)).await;
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

        let id = created(&state, &cookie, json!({})).await["id"].as_str().unwrap().to_string();
        assert_eq!(stored_accounts(&pool, &user.id, "my-endpoint").await.len(), 1);

        let (_, other_cookie) = signed_in(&state, "user_2@example.com").await;
        let theirs = call(&state, request("DELETE", &format!("/api/custom-providers/{id}"), Some(&other_cookie), None)).await;
        assert_eq!(theirs.status(), StatusCode::NOT_FOUND);
        assert_eq!(stored_accounts(&pool, &user.id, "my-endpoint").await.len(), 1);

        let res = call(&state, request("DELETE", &format!("/api/custom-providers/{id}"), Some(&cookie), None)).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await, json!({ "ok": true }));
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM custom_providers").fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0);
        assert!(stored_accounts(&pool, &user.id, "my-endpoint").await.is_empty());
    }

    // ---------------------------------------------------------- POST /:id/unpause

    #[tokio::test]
    async fn unpause_clears_the_bench_without_touching_the_stored_key() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool.clone(), MockTransport::new());
        let now = state.now_ms();

        let anon = call(&state, request("POST", "/api/custom-providers/cprov_1/unpause", None, None)).await;
        assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);

        let (user, cookie) = signed_in(&state, "user_1@example.com").await;
        let unknown = call(&state, request("POST", "/api/custom-providers/nonexistent/unpause", Some(&cookie), None)).await;
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_json(unknown).await, json!({ "error": "not found" }));

        let id = created(&state, &cookie, json!({})).await["id"].as_str().unwrap().to_string();
        let account = stored_accounts(&pool, &user.id, "my-endpoint").await.remove(0);
        crate::pool::bench::mark_benched(&pool, &user.id, "my-endpoint", &account.id, None, None, now).await.unwrap();
        assert_eq!(listed(&state, &cookie).await["providers"][0]["status"], "benched");

        // Another user's provider id is a 404 and leaves the bench alone.
        let (_, other_cookie) = signed_in(&state, "user_2@example.com").await;
        let theirs = call(&state, request("POST", &format!("/api/custom-providers/{id}/unpause"), Some(&other_cookie), None)).await;
        assert_eq!(theirs.status(), StatusCode::NOT_FOUND);
        assert!(crate::pool::bench::is_benched(&pool, &user.id, &account.id, now).await.unwrap());

        let res = call(&state, request("POST", &format!("/api/custom-providers/{id}/unpause"), Some(&cookie), None)).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await, json!({ "ok": true }));
        assert!(!crate::pool::bench::is_benched(&pool, &user.id, &account.id, now).await.unwrap());
        assert_eq!(stored_accounts(&pool, &user.id, "my-endpoint").await[0].encrypted_payload, account.encrypted_payload);
        assert_eq!(listed(&state, &cookie).await["providers"][0]["status"], "active");

        // Unpausing a provider that is not benched is still a 200.
        let again = call(&state, request("POST", &format!("/api/custom-providers/{id}/unpause"), Some(&cookie), None)).await;
        assert_eq!(again.status(), StatusCode::OK);
        assert_eq!(body_json(again).await, json!({ "ok": true }));
    }

    // -------------------------------------------------------------------- POST /test

    async fn test_probe(state: &AppState, cookie: &str, body: Value) -> Response {
        call(state, request("POST", "/api/custom-providers/test", Some(cookie), Some(body))).await
    }

    fn probe_body() -> Value {
        json!({ "format": "openai", "base_url": "https://upstream.example.com/v1", "api_key": "sk-test" })
    }

    #[tokio::test]
    async fn the_test_probe_maps_every_upstream_outcome() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (_, cookie) = signed_in(&state, "user_1@example.com").await;

        transport.respond_json(StatusCode::OK, json!({ "data": [{ "id": "m1" }, { "id": "m2" }] }));
        let json = body_json(test_probe(&state, &cookie, probe_body()).await).await;
        assert_eq!(json, json!({ "ok": true, "models_count": 2, "sample": ["m1", "m2"] }));

        transport.expect(|_| Ok(UpstreamResponse::from_bytes(StatusCode::NOT_FOUND, Default::default(), "not found")));
        let json = body_json(test_probe(&state, &cookie, probe_body()).await).await;
        assert_eq!(json["ok"], true);
        assert_eq!(json["models_count"], Value::Null);
        assert!(json["note"].as_str().unwrap().contains("no models endpoint"));

        transport.expect(|_| Ok(UpstreamResponse::from_bytes(StatusCode::UNAUTHORIZED, Default::default(), "nope")));
        let json = body_json(test_probe(&state, &cookie, probe_body()).await).await;
        assert_eq!(json, json!({ "ok": false, "error": "auth rejected (401)" }));

        transport.expect(|_| Err(TransportError::Other("simulated network failure".into())));
        let json = body_json(test_probe(&state, &cookie, probe_body()).await).await;
        assert_eq!(json, json!({ "ok": false, "error": "unreachable/timeout" }));

        transport.expect(|_| Ok(UpstreamResponse::from_bytes(StatusCode::INTERNAL_SERVER_ERROR, Default::default(), "boom")));
        let json = body_json(test_probe(&state, &cookie, probe_body()).await).await;
        assert_eq!(json, json!({ "ok": false, "error": "HTTP 500" }));
    }

    #[tokio::test]
    async fn the_test_probe_runs_the_url_guard_before_probing() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        // The transport has no handler: any upstream call would panic before the assertion.
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (_, cookie) = signed_in(&state, "user_1@example.com").await;

        let res = test_probe(
            &state,
            &cookie,
            json!({ "format": "openai", "base_url": "http://insecure.example.com", "api_key": "sk-test" }),
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(transport.requests().is_empty());
    }

    #[tokio::test]
    async fn the_test_probe_uses_the_stored_key_for_a_saved_provider() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool.clone(), transport.clone());
        let (_, cookie) = signed_in(&state, "user_1@example.com").await;
        let id = created(&state, &cookie, json!({})).await["id"].as_str().unwrap().to_string();
        transport.respond_json(StatusCode::OK, json!({ "data": [] }));

        let res = test_probe(&state, &cookie, json!({ "id": id })).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(transport.requests()[0].header("authorization"), Some(format!("Bearer {API_KEY}").as_str()));
        assert_eq!(transport.requests()[0].url, "https://upstream.example.com/v1/models");

        // An unknown id is a 404 before any probe.
        let missing = test_probe(&state, &cookie, json!({ "id": "cprov_missing" })).await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }
}
