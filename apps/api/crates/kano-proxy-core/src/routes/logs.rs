//! `/api/logs` — the Logs page's cursor-paged request history (//! docs/admin-ui.md § Logs page, docs/logging.md).
//!
//! Rows carry counts, costs and names only: prompts, completions and tokens never enter
//! `request_logs`, and the `api_keys` id is resolved to a name here so it never leaves the
//! process. A row served by an account that is not the viewer's own is either borrowed
//! through the pool extension (which names the ones it still lets this viewer see) or removed.

use std::collections::{HashMap, HashSet};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde_json::{json, Value};

use crate::auth::session::SessionUser;
use crate::db::keys::list_keys;
use crate::db::request_logs::{list_request_log_page, viewer_account_labels, LogPageQuery, RequestLogPageRow};
use crate::pool::extension::SharedLabel;
use crate::routes::usage::{fill_estimated_costs, is_builtin_provider, read_time_price_table};
use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/", get(list))
}

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 100;

fn error(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "error": code }))).into_response()
}

/// The page cursor: the last shown row's `(created_at, id)`, base64 of `"<created_at> <id>"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub created_at: String,
    pub id: String,
}

pub fn encode_cursor(cursor: &Cursor) -> String {
    BASE64.encode(format!("{} {}", cursor.created_at, cursor.id))
}

/// `None` for anything that is not exactly one base64-encoded `"<created_at> <id>"` pair —
/// a malformed cursor is a 400, never a silently different page.
pub fn decode_cursor(value: &str) -> Option<Cursor> {
    if value.len() % 4 != 0 || !value.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=') {
        return None;
    }
    // Padding may only ever trail the payload.
    if value.trim_end_matches('=').contains('=') || value.len() - value.trim_end_matches('=').len() > 2 {
        return None;
    }
    let bytes = BASE64.decode(value).ok()?;
    let decoded = String::from_utf8(bytes).ok()?;
    let separator = decoded.find(' ')?;
    if separator == 0 || Some(separator) != decoded.rfind(' ') {
        return None;
    }
    let id = decoded[separator + 1..].to_string();
    if id.is_empty() {
        return None;
    }
    Some(Cursor { created_at: decoded[..separator].to_string(), id })
}

/// `50` when absent; otherwise a plain decimal integer in `1..=100`. Anything else is a 400,
/// so a typo never silently reads the whole table.
pub fn parse_limit(value: Option<&str>) -> Option<i64> {
    let Some(value) = value else { return Some(DEFAULT_LIMIT) };
    if value.is_empty() || !value.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let limit: i64 = value.parse().ok()?;
    (1..=MAX_LIMIT).contains(&limit).then_some(limit)
}

async fn list(State(state): State<AppState>, session: SessionUser, Query(params): Query<HashMap<String, String>>) -> Response {
    let Some(limit) = parse_limit(params.get("limit").map(String::as_str)) else {
        return error(StatusCode::BAD_REQUEST, "invalid_limit");
    };
    let cursor = match params.get("cursor") {
        None => None,
        Some(value) => match decode_cursor(value) {
            Some(cursor) => Some(cursor),
            None => return error(StatusCode::BAD_REQUEST, "invalid_cursor"),
        },
    };

    let query = LogPageQuery {
        user_id: &session.user.id,
        provider: params.get("provider").map(String::as_str),
        errors_only: params.get("errors").map(String::as_str) == Some("1"),
        cursor: cursor.as_ref().map(|c| (c.created_at.as_str(), c.id.as_str())),
        limit: limit + 1,
    };
    let Ok(mut fetched) = list_request_log_page(state.pool(), &query).await else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error");
    };
    let has_next_page = fetched.len() as i64 > limit;
    fetched.truncate(limit as usize);
    let page = fetched;

    // Read-time pricing matches the summary. A missing or legacy table may be refreshed on
    // this admin surface; proxied request handling never fetches it.
    let table = read_time_price_table(&state).await;
    let mut priced = page.clone();
    fill_estimated_costs(&mut priced, table.as_ref());

    let Ok(account_labels) = viewer_account_labels(state.pool(), &session.user.id).await else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error");
    };
    let account_labels: HashMap<String, Option<String>> = account_labels.into_iter().collect();
    let Ok(keys) = list_keys(state.pool(), &session.user.id).await else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error");
    };
    let api_key_names: HashMap<String, String> = keys.into_iter().map(|k| (k.id, k.name)).collect();

    // Ids that are not the viewer's own rows, in page order and asked about once.
    let mut seen: HashSet<&str> = HashSet::new();
    let foreign: Vec<String> = priced
        .iter()
        .filter_map(|row| row.account_id.as_deref())
        .filter(|id| !account_labels.contains_key(*id) && seen.insert(id))
        .map(str::to_string)
        .collect();
    let shared: HashMap<String, SharedLabel> = match state.pool_extension() {
        Some(ext) if !foreign.is_empty() => match ext.label_shared(&state, &session.user.id, &foreign).await {
            Ok(labels) => labels,
            Err(error) => {
                tracing::error!(%error, "pool extension labelShared failed for the logs page");
                HashMap::new()
            }
        },
        _ => HashMap::new(),
    };

    let rows: Vec<Value> = priced.iter().map(|row| row_json(row, &account_labels, &api_key_names, &shared)).collect();
    let next_cursor = match page.last() {
        Some(last) if has_next_page => {
            Value::String(encode_cursor(&Cursor { created_at: last.created_at.clone(), id: last.id.clone() }))
        }
        _ => Value::Null,
    };
    Json(json!({ "rows": rows, "next_cursor": next_cursor })).into_response()
}

fn row_json(
    row: &RequestLogPageRow,
    account_labels: &HashMap<String, Option<String>>,
    api_key_names: &HashMap<String, String>,
    shared: &HashMap<String, SharedLabel>,
) -> Value {
    let account_label = match row.account_id.as_deref() {
        None => None,
        // Live label, else the lender's label for a borrowed row, else the name stored when
        // the row was written — removal is the explicit flag, never a null name.
        Some(id) => account_labels
            .get(id)
            .cloned()
            .flatten()
            .or_else(|| shared.get(id).map(|s| s.label.clone()))
            .or_else(|| row.account_label.clone()),
    };
    let account_removed = row
        .account_id
        .as_deref()
        .is_some_and(|id| !account_labels.contains_key(id) && !shared.contains_key(id));
    let account_shared_by = row.account_id.as_deref().and_then(|id| shared.get(id)).map(|s| s.owner_label.clone());
    // The `api_keys` id is resolved to a name here and never leaves the process
    // (docs/admin-ui.md § Logs page): a bare id under "API key" reads as the credential to
    // whoever is looking, so the client gets only the name plus this explicit removed flag.
    let api_key_name = row
        .api_key_id
        .as_deref()
        .and_then(|id| api_key_names.get(id).cloned().or_else(|| row.api_key_name.clone()));
    let api_key_removed = row.api_key_id.as_deref().is_some_and(|id| !api_key_names.contains_key(id));
    json!({
        "id": row.id,
        "created_at": row.created_at,
        "provider": row.provider,
        "model": row.model,
        "group_name": row.group_name,
        "account_id": row.account_id,
        "account_label": account_label,
        "account_removed": account_removed,
        "account_shared_by": account_shared_by,
        "api_key_name": api_key_name,
        "api_key_removed": api_key_removed,
        "usage_type": if is_builtin_provider(&row.provider) { "oauth" } else { "api" },
        "status_code": row.status_code,
        "upstream_status": row.upstream_status,
        "error_code": row.error_code,
        "latency_ms": row.latency_ms,
        "prompt_tokens": row.prompt_tokens,
        "completion_tokens": row.completion_tokens,
        "cache_read_input_tokens": row.cache_read_input_tokens,
        "cache_creation_input_tokens": row.cache_creation_input_tokens,
        "cost": row.cost,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::session::create_session;
    use crate::db::request_logs::{insert_request_log, RequestLogEntry};
    use crate::db::test_support::{insert_user, skip_without_db, test_pool, test_state, TEST_APP_URL};
    use crate::extensions::Extensions;
    use crate::pool::extension::{ListSharedOptions, PoolExtension, ReserveContext, ReserveOutcome, SharedAccount};
    use crate::providers::ProviderId;
    use crate::routing::types::RoutingCandidate;
    use crate::upstream::{MockTransport, UpstreamResponse};
    use axum::body::Body;
    use axum::http::{header, Request};
    use http_body_util::BodyExt;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    /// A transport that answers every upstream attempt (the read-time price fetch) with a 500,
    /// so no test ever reaches the real network. Enough handlers are queued for the price
    /// refresh both sources make on every admin request in the test.
    fn offline() -> Arc<MockTransport> {
        let mock = MockTransport::new();
        for _ in 0..64 {
            mock.expect(|_| {
                Ok(UpstreamResponse::from_bytes(StatusCode::INTERNAL_SERVER_ERROR, http::HeaderMap::new(), "offline"))
            });
        }
        mock
    }

    async fn body_json(response: Response) -> Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn signed_in(state: &AppState, email: &str) -> (String, String) {
        let user = insert_user(state.pool(), email).await;
        let (_, cookie) = create_session(state, &user.id, false).await.unwrap();
        (user.id, cookie.split(';').next().unwrap().to_string())
    }

    /// Seeds one `request_logs` row, overriding `created_at` and `id` after the insert (the
    /// writer stamps both).
    async fn seed_log(state: &AppState, user_id: &str, id: Option<&str>, overrides: Value) {
        let mut base = json!({
            "user_id": user_id,
            "provider": "claude-code",
            "model": "claude-code/claude-opus-5",
            "status_code": 200,
            "latency_ms": 100,
            "started_at": crate::ids::now_iso(),
        });
        let mut created_at = "2026-08-14T12:00:00.000Z".to_string();
        for (k, v) in overrides.as_object().expect("overrides object") {
            if k == "created_at" {
                created_at = v.as_str().expect("iso string").to_string();
                continue;
            }
            base[k] = v.clone();
        }
        let entry: RequestLogEntry = serde_json::from_value(base).expect("a log entry");
        let written = insert_request_log(state.pool(), &entry).await.unwrap();
        sqlx::query("UPDATE request_logs SET created_at = $1, id = $2 WHERE id = $3")
            .bind(&created_at)
            .bind(id.unwrap_or(&written))
            .bind(&written)
            .execute(state.pool())
            .await
            .unwrap();
    }

    async fn seed_account(state: &AppState, user_id: &str, id: &str, label: Option<&str>, custom_label: Option<&str>) {
        sqlx::query(
            "INSERT INTO upstream_accounts (id, user_id, provider, external_account_id, label, custom_label, priority,
                                            encrypted_payload, created_at, updated_at)
             VALUES ($1, $2, 'claude-code', NULL, $3, $4, 1, 'encrypted', $5, $5)",
        )
        .bind(id)
        .bind(user_id)
        .bind(label)
        .bind(custom_label)
        .bind(crate::ids::now_iso())
        .execute(state.pool())
        .await
        .unwrap();
    }

    fn router(state: AppState) -> axum::Router {
        crate::build_router(state, Extensions::default())
    }

    async fn get(router: axum::Router, cookie: &str, uri: &str) -> Response {
        router
            .oneshot(Request::builder().uri(uri).header(header::COOKIE, cookie).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[test]
    fn cursors_round_trip_and_reject_junk() {
        let cursor = Cursor { created_at: "2026-08-14T12:00:00.000Z".into(), id: "log_a".into() };
        let encoded = encode_cursor(&cursor);
        assert_eq!(decode_cursor(&encoded), Some(cursor));
        for bad in ["not-a-cursor", "", &BASE64.encode("no-space"), &BASE64.encode(" leading"), &BASE64.encode("two spaces here")] {
            assert_eq!(decode_cursor(bad), None, "{bad}");
        }
        assert_eq!(decode_cursor(&BASE64.encode("2026-08-14T12:00:00.000Z ")), None, "an empty id is no cursor");
    }

    #[test]
    fn the_limit_defaults_to_fifty_and_is_bounded() {
        assert_eq!(parse_limit(None), Some(50));
        assert_eq!(parse_limit(Some("1")), Some(1));
        assert_eq!(parse_limit(Some("100")), Some(100));
        for bad in ["0", "101", "1.5", "-1", "abc", ""] {
            assert_eq!(parse_limit(Some(bad)), None, "{bad}");
        }
    }

    #[tokio::test]
    async fn requires_a_session() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let response = router(state).oneshot(Request::builder().uri("/api/logs").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(response).await, json!({ "error": "unauthorized" }));
    }

    #[tokio::test]
    async fn uses_the_default_limit_and_rejects_invalid_limits() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let (user_id, cookie) = signed_in(&state, "limit@example.com").await;
        for i in 0..51 {
            seed_log(
                &state,
                &user_id,
                Some(&format!("log_{i:03}")),
                json!({ "created_at": format!("2026-08-14T12:{i:02}:00.000Z") }),
            )
            .await;
        }

        let json = body_json(get(router(state.clone()), &cookie, "/api/logs").await).await;
        assert_eq!(json["rows"].as_array().unwrap().len(), 50);
        assert!(json["next_cursor"].is_string());

        for bad in ["0", "101", "1.5", "-1", "abc", ""] {
            let response = get(router(state.clone()), &cookie, &format!("/api/logs?limit={bad}")).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{bad}");
            assert_eq!(body_json(response).await, json!({ "error": "invalid_limit" }));
        }
    }

    #[tokio::test]
    async fn cursor_pages_stably_by_created_at_then_id() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let (user_id, cookie) = signed_in(&state, "cursor@example.com").await;
        for id in ["log_a", "log_c", "log_b", "log_d", "log_e"] {
            seed_log(&state, &user_id, Some(id), json!({})).await;
        }

        let first = body_json(get(router(state.clone()), &cookie, "/api/logs?limit=2").await).await;
        assert_eq!(ids(&first), ["log_e", "log_d"]);
        let cursor = first["next_cursor"].as_str().expect("a next cursor").to_string();

        let second =
            body_json(get(router(state.clone()), &cookie, &format!("/api/logs?limit=2&cursor={}", urlencode(&cursor))).await).await;
        assert_eq!(ids(&second), ["log_c", "log_b"]);
        let cursor = second["next_cursor"].as_str().expect("a next cursor").to_string();

        let third =
            body_json(get(router(state.clone()), &cookie, &format!("/api/logs?limit=2&cursor={}", urlencode(&cursor))).await).await;
        assert_eq!(ids(&third), ["log_a"]);
        assert_eq!(third["next_cursor"], Value::Null, "the last page carries no cursor");

        let response = get(router(state), &cookie, "/api/logs?cursor=not-a-cursor").await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await, json!({ "error": "invalid_cursor" }));
    }

    fn ids(json: &Value) -> Vec<String> {
        json["rows"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_string()).collect()
    }

    fn urlencode(value: &str) -> String {
        value.replace('+', "%2B").replace('/', "%2F").replace('=', "%3D")
    }

    #[tokio::test]
    async fn filters_by_exact_provider_and_by_errors() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let (user_id, cookie) = signed_in(&state, "filter@example.com").await;
        seed_log(&state, &user_id, Some("builtin"), json!({ "provider": "claude-code" })).await;
        seed_log(&state, &user_id, Some("custom"), json!({ "provider": "deleted-custom" })).await;
        seed_log(&state, &user_id, Some("error-code"), json!({ "provider": "deleted-custom", "error_code": "invalid_model" })).await;
        seed_log(&state, &user_id, Some("error-status"), json!({ "provider": "deleted-custom", "status_code": 503 })).await;

        let json = body_json(get(router(state.clone()), &cookie, "/api/logs?provider=deleted-custom").await).await;
        let mut got = ids(&json);
        got.sort();
        assert_eq!(got, ["custom", "error-code", "error-status"], "a deleted provider is still filterable");

        let json = body_json(get(router(state), &cookie, "/api/logs?errors=1").await).await;
        let mut got = ids(&json);
        got.sort();
        assert_eq!(got, ["error-code", "error-status"]);
    }

    #[tokio::test]
    async fn resolves_display_fields_derives_usage_type_and_prices_null_costs() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let mock = MockTransport::new();
        for _ in 0..8 {
            mock.expect(|request| {
                if request.url.contains("openrouter.ai") {
                    Ok(UpstreamResponse::json(StatusCode::OK, &json!({ "data": [] })))
                } else {
                    Ok(UpstreamResponse::json(
                        StatusCode::OK,
                        &json!({ "claude-opus-5": { "input_cost_per_token": 0.00001, "output_cost_per_token": 0.00005 } }),
                    ))
                }
            });
        }
        let state = test_state(pool, mock);
        let (user_id, cookie) = signed_in(&state, "display@example.com").await;
        let other = insert_user(state.pool(), "other@example.com").await;
        seed_account(&state, &user_id, "account_live", Some("Upstream name"), Some("Primary")).await;
        seed_account(&state, &user_id, "account_fallback", Some("Fallback name"), None).await;
        seed_account(&state, &other.id, "other_account", Some("Leaked"), Some("Leaked")).await;
        let key = crate::db::keys::create_key(state.pool(), &user_id, "Production", None).await.unwrap().row;
        crate::db::keys::create_key(state.pool(), &other.id, "Leaked", None).await.unwrap();

        seed_log(
            &state,
            &user_id,
            Some("oauth"),
            json!({ "account_id": "account_live", "api_key_id": key.id, "prompt_tokens": 100, "completion_tokens": 10 }),
        )
        .await;
        seed_log(
            &state,
            &user_id,
            Some("custom"),
            json!({ "provider": "deleted-custom", "model": "deleted-custom/model", "account_id": "account_fallback",
                    "api_key_id": "deleted_key" }),
        )
        .await;
        seed_log(&state, &user_id, Some("deleted-account"), json!({ "account_id": "deleted_account" })).await;
        // Written after the name-snapshot migration: both records being gone still leaves
        // something to read.
        seed_log(
            &state,
            &user_id,
            Some("named-and-gone"),
            json!({ "account_id": "deleted_account_2", "account_label": "Old laptop", "api_key_id": "deleted_key_2",
                    "api_key_name": "Old CI key" }),
        )
        .await;

        let json = body_json(get(router(state), &cookie, "/api/logs?limit=10").await).await;
        let rows = json["rows"].as_array().unwrap();
        for row in rows {
            assert!(row.get("api_key_id").is_none(), "the api_keys id never leaves the process");
        }
        let by_id = |id: &str| rows.iter().find(|r| r["id"] == id).expect("row").clone();

        let oauth = by_id("oauth");
        assert_eq!(oauth["account_label"], "Primary", "the user's own name wins");
        assert_eq!(oauth["account_removed"], false);
        assert_eq!(oauth["account_shared_by"], Value::Null);
        assert_eq!(oauth["api_key_name"], "Production");
        assert_eq!(oauth["api_key_removed"], false);
        assert_eq!(oauth["usage_type"], "oauth");
        let expected = 100.0 * 0.00001 + 10.0 * 0.00005;
        assert!((oauth["cost"].as_f64().unwrap() - expected).abs() < 1e-12);

        // "custom" points at a key that never appears in api_keys for this user — that is
        // exactly what api_key_removed means.
        let custom = by_id("custom");
        assert_eq!(custom["account_label"], "Fallback name");
        assert_eq!(custom["api_key_name"], Value::Null);
        assert_eq!(custom["api_key_removed"], true);
        assert_eq!(custom["usage_type"], "api");
        assert_eq!(custom["cost"], Value::Null);

        // A NULL api_key_id (no key attributed) is "not reported", not "removed".
        let deleted = by_id("deleted-account");
        assert_eq!(deleted["account_label"], Value::Null);
        assert_eq!(deleted["account_removed"], true);
        assert_eq!(deleted["api_key_name"], Value::Null);
        assert_eq!(deleted["api_key_removed"], false);

        // A removed record keeps its last name: removal is the flag, not a null name.
        let named = by_id("named-and-gone");
        assert_eq!(named["account_label"], "Old laptop");
        assert_eq!(named["account_removed"], true);
        assert_eq!(named["api_key_name"], "Old CI key");
        assert_eq!(named["api_key_removed"], true);
    }

    /// A pool extension that names exactly one borrowed account and records what it was asked.
    struct LabelStub {
        asked: Mutex<Vec<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl PoolExtension for LabelStub {
        async fn list_shared(
            &self,
            _cx: &AppState,
            _viewer_user_id: &str,
            _provider: ProviderId,
            _options: ListSharedOptions,
        ) -> anyhow::Result<Vec<SharedAccount>> {
            Ok(Vec::new())
        }
        async fn reserve_attempt(
            &self,
            _cx: &AppState,
            _ctx: ReserveContext<'_>,
            _candidate: &RoutingCandidate,
        ) -> anyhow::Result<ReserveOutcome> {
            Ok(ReserveOutcome::Ungoverned)
        }
        async fn set_shared_priority(&self, _cx: &AppState, _viewer: &str, _account_id: &str, _priority: i32) -> anyhow::Result<bool> {
            Ok(false)
        }
        async fn label_shared(
            &self,
            _cx: &AppState,
            viewer_user_id: &str,
            account_ids: &[String],
        ) -> anyhow::Result<HashMap<String, SharedLabel>> {
            let mut asked = vec![viewer_user_id.to_string()];
            asked.extend(account_ids.iter().cloned());
            self.asked.lock().unwrap().push(asked);
            Ok(HashMap::from([(
                "lent".to_string(),
                SharedLabel { label: "Team Claude".into(), owner_label: "Owner Person".into() },
            )]))
        }
    }

    #[tokio::test]
    async fn names_a_borrowed_account_through_the_pool_extension_and_never_another_users_row() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let base = test_state(pool, offline());
        let (user_id, cookie) = signed_in(&base, "borrow@example.com").await;
        let lender = insert_user(base.pool(), "lender@example.com").await;
        seed_account(&base, &user_id, "own", Some("Mine"), None).await;
        seed_account(&base, &lender.id, "lent", Some("owner@example.test"), Some("Team Claude")).await;
        seed_account(&base, &lender.id, "withdrawn", Some("No longer shared"), None).await;
        seed_log(&base, &user_id, Some("own"), json!({ "account_id": "own" })).await;
        seed_log(&base, &user_id, Some("lent"), json!({ "account_id": "lent", "account_label": "Team Claude" })).await;
        seed_log(&base, &user_id, Some("withdrawn"), json!({ "account_id": "withdrawn", "account_label": "Was shared" })).await;

        let stub = Arc::new(LabelStub { asked: Mutex::new(Vec::new()) });
        let with_ext = AppState::builder(crate::db::test_support::test_config(), base.pool().clone())
            .transport(offline())
            .pool_extension(Some(stub.clone()))
            .build();
        let json = body_json(get(router(with_ext), &cookie, "/api/logs?limit=10").await).await;
        // Only the ids that are not the viewer's own rows are asked about, once, in page order.
        assert_eq!(
            *stub.asked.lock().unwrap(),
            vec![vec![user_id.clone(), "withdrawn".to_string(), "lent".to_string()]]
        );
        let rows = json["rows"].as_array().unwrap();
        let by_id = |id: &str| rows.iter().find(|r| r["id"] == id).expect("row").clone();
        assert_eq!(by_id("own")["account_label"], "Mine");
        assert_eq!(by_id("own")["account_removed"], false);
        assert_eq!(by_id("own")["account_shared_by"], Value::Null);
        assert_eq!(by_id("lent")["account_label"], "Team Claude");
        assert_eq!(by_id("lent")["account_removed"], false);
        assert_eq!(by_id("lent")["account_shared_by"], "Owner Person");
        // The extension did not name it, so the lender's live label must not leak: the row
        // falls back to the label stored when it was written, flagged as removed.
        assert_eq!(by_id("withdrawn")["account_label"], "Was shared");
        assert_eq!(by_id("withdrawn")["account_removed"], true);
        assert_eq!(by_id("withdrawn")["account_shared_by"], Value::Null);

        // Without an extension the same rows read as removed, by their stored names.
        let json = body_json(get(router(base), &cookie, "/api/logs?limit=10").await).await;
        let rows = json["rows"].as_array().unwrap();
        let lent = rows.iter().find(|r| r["id"] == "lent").expect("row");
        assert_eq!(lent["account_label"], "Team Claude");
        assert_eq!(lent["account_removed"], true);
        assert_eq!(lent["account_shared_by"], Value::Null);
    }

    #[tokio::test]
    async fn the_admin_cors_and_no_store_rules_apply() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let (_, cookie) = signed_in(&state, "cors@example.com").await;
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/logs")
                    .header(header::COOKIE, &cookie)
                    .header(header::ORIGIN, TEST_APP_URL)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(), TEST_APP_URL);
        assert_eq!(response.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
    }
}
