//! Port of apps/api/src/providers/usage_refresh.ts (docs/providers.md § Usage cache).
//!
//! The single-flight lock/fetch/write machinery shared by `GET
//! /api/providers/:provider/accounts` (routes/providers.rs) and the routing module's
//! background refresh fired on every dispatch (docs/providers.md § Routing module "Facts").
//! The lock primitives themselves (`acquire_usage_lock` / `write_usage_snapshot` /
//! `release_usage_lock` — the compare-and-swap) live in `db::accounts` and are never
//! reimplemented here; this module is the one place that calls them plus `fetch_usage` and
//! persists the result.

use serde_json::{Map, Value};

use crate::crypto::token_crypto::decrypt_json;
use crate::db::accounts::{
    acquire_usage_lock, get_account, is_usage_fresh, read_usage_snapshot, release_usage_lock, update_account_identity,
    write_usage_snapshot, AccountIdentity, AccountRow, UsageSnapshot,
};
use crate::pool::{AcquiredAccount, StoredCredential};
use crate::providers::identity::pick_account_label;
use crate::providers::types::{DynAdapter, ProviderAdapter};
use crate::AppState;

/// `{ ok: true, snapshot } | { ok: false, error }` in the TypeScript.
pub type UsagePersistResult = Result<UsageSnapshot, String>;

/// Fetch usage from upstream and persist it. A failure releases the lock without writing; a
/// soft failure (the adapter reports `error` with no windows) still writes, merged with the
/// prior snapshot's windows/account and flagged `stale`, so the TTL restarts either way — one
/// hiccup must not blank a good snapshot. Caller must already hold `lock_token` from
/// `acquire_usage_lock`.
pub async fn fetch_and_persist_usage(
    cx: &AppState,
    row: &AccountRow,
    adapter: &dyn ProviderAdapter,
    lock_token: &str,
) -> UsagePersistResult {
    match persist(cx, row, adapter, lock_token).await {
        Ok(snapshot) => Ok(snapshot),
        Err(error) => {
            let _ = release_usage_lock(cx.pool(), &row.id, lock_token).await;
            Err(error)
        }
    }
}

async fn persist(cx: &AppState, row: &AccountRow, adapter: &dyn ProviderAdapter, lock_token: &str) -> UsagePersistResult {
    let credential: StoredCredential =
        decrypt_json(cx.config().token_encryption_key.as_deref(), &row.encrypted_payload).map_err(|e| e.to_string())?;
    let snap = adapter.fetch_usage(cx, &AcquiredAccount { row: row.clone(), credential }).await;

    let prior_meta: Map<String, Value> = row
        .account_meta_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let mut account_meta = prior_meta;
    for (key, value) in snap.account.clone() {
        account_meta.insert(key, value);
    }

    // Prefer upstream email / username as stable pool label, same as the route's inline
    // version this replaces.
    let email = snap.account.get("email").and_then(Value::as_str);
    let display = snap
        .account
        .get("display_name")
        .and_then(Value::as_str)
        .or_else(|| snap.account.get("username").and_then(Value::as_str));
    let fallback = row.label.as_deref().filter(|l| !l.is_empty()).unwrap_or(&row.id);
    let better = pick_account_label(email, display, fallback);
    let meta_json = serde_json::to_string(&Value::Object(account_meta)).map_err(|e| e.to_string())?;
    let label = (Some(better.as_str()) != row.label.as_deref()).then_some(better.as_str());
    update_account_identity(cx.pool(), &row.id, AccountIdentity { label, account_meta_json: Some(&meta_json) })
        .await
        .map_err(|e| e.to_string())?;

    // A soft failure (error + no windows of its own) is a failed read wearing a 200 — same
    // rule as a thrown error: never overwrite a good snapshot (docs/providers.md § Usage cache
    // "'Failure' includes a soft failure"). Windows always win when they do arrive, error or
    // not.
    let prior_snapshot = if snap.windows.is_empty() && snap.error.is_some() { read_usage_snapshot(row) } else { None };
    let persisted = match prior_snapshot {
        Some(prior) => UsageSnapshot {
            windows: prior.windows,
            account: prior.account,
            error: snap.error.clone(),
            stale: true,
            edge_blocked: snap.edge_blocked,
        },
        None => UsageSnapshot {
            windows: snap.windows.iter().map(|w| serde_json::to_value(w).expect("usage window serializes")).collect(),
            account: Some(snap.account.clone()),
            error: snap.error.clone(),
            stale: snap.stale,
            edge_blocked: snap.edge_blocked,
        },
    };
    write_usage_snapshot(cx.pool(), &row.id, lock_token, &persisted).await.map_err(|e| e.to_string())?;
    Ok(persisted)
}

/// Background refresh (docs/providers.md § Routing module "Facts"): fired on every dispatch so
/// limit-aware skip facts stay warm while traffic flows, reusing the exact 2 min-TTL
/// single-flight cache `GET /api/providers/:provider/accounts` uses — zero added request
/// latency.
///
/// Fresh-within-2-min short-circuits with no upstream call. A lock already held by another
/// caller (another concurrent request, or the accounts-page poll) also short-circuits without
/// queuing — the stored snapshot already serves every reader; there is nothing here to report
/// a failure to, so a lost race is simply a no-op rather than a queued retry.
pub async fn refresh_account_usage_in_background(cx: &AppState, row: &AccountRow, adapter: &dyn ProviderAdapter) {
    if !adapter.has_fetch_usage() {
        return;
    }
    if is_usage_fresh(row, crate::app::now_ms()) {
        return;
    }
    let Ok(Some(lock_token)) = acquire_usage_lock(cx.pool(), &row.id).await else { return };
    // Dispatch may have refreshed this account after it loaded `row`; use the post-lock row so
    // fetch_usage never sends a stale credential.
    let fresh_row = match get_account(cx.pool(), &row.user_id, &row.id).await {
        Ok(Some(fresh_row)) => fresh_row,
        _ => {
            let _ = release_usage_lock(cx.pool(), &row.id, &lock_token).await;
            return;
        }
    };
    let _ = fetch_and_persist_usage(cx, &fresh_row, adapter, &lock_token).await;
}

/// The `ctx.waitUntil` form: the same refresh detached onto the runtime, for callers on a
/// request path that must not wait for it (docs/rust-server.md § Module map, "Background
/// work").
pub fn spawn_account_usage_refresh(cx: &AppState, row: AccountRow, adapter: DynAdapter) -> tokio::task::JoinHandle<()> {
    let cx = cx.clone();
    tokio::spawn(async move {
        if !cx.background_work() {
            return;
        }
        refresh_account_usage_in_background(&cx, &row, adapter.as_ref()).await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::token_crypto::encrypt_json;
    use crate::db::accounts::update_account_payload;
    use crate::db::test_support::{insert_account, insert_user, skip_without_db, test_pool, test_state, TEST_TOKEN_KEY};
    use crate::providers::types::{AdapterError, CallExtras, ChatCompletionRequest, FetchedUsage, UsageWindow};
    use crate::upstream::MockTransport;
    use async_trait::async_trait;
    use axum::response::Response;
    use serde_json::json;
    use sqlx::PgPool;
    use std::sync::{Arc, Mutex};

    /// A stub adapter: `fetch_usage` answers with `usage` and records the credential it was
    /// handed, standing in for the vitest inline adapter objects.
    struct StubAdapter {
        usage: Mutex<Option<FetchedUsage>>,
        seen_credential: Arc<Mutex<Option<String>>>,
    }

    impl StubAdapter {
        fn new(usage: FetchedUsage) -> Self {
            Self { usage: Mutex::new(Some(usage)), seen_credential: Arc::new(Mutex::new(None)) }
        }
    }

    #[async_trait]
    impl ProviderAdapter for StubAdapter {
        fn id(&self) -> &str {
            "grok"
        }
        async fn chat_completions(
            &self,
            _: &AppState,
            _: &AcquiredAccount,
            _: &ChatCompletionRequest,
            _: &CallExtras,
        ) -> Result<Response, AdapterError> {
            Err(AdapterError::Unsupported("chat_completions"))
        }
        fn has_fetch_usage(&self) -> bool {
            true
        }
        async fn fetch_usage(&self, _: &AppState, account: &AcquiredAccount) -> FetchedUsage {
            *self.seen_credential.lock().unwrap() = Some(account.credential.access_token.clone());
            self.usage.lock().unwrap().take().unwrap_or_default()
        }
    }

    fn usage(windows: Vec<UsageWindow>, account: Value, error: Option<&str>, stale: bool) -> FetchedUsage {
        FetchedUsage {
            windows,
            account: account.as_object().cloned().unwrap_or_default(),
            stale,
            error: error.map(str::to_string),
            edge_blocked: false,
        }
    }

    async fn stored(pool: &PgPool, user_id: &str, id: &str) -> AccountRow {
        get_account(pool, user_id, id).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn re_reads_the_account_after_its_usage_lock_and_uses_a_post_refresh_credential() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let cx = test_state(pool.clone(), MockTransport::new());
        let user = insert_user(&pool, "usage-refresh@example.com").await;
        let stale = StoredCredential { access_token: "stale-access".into(), ..Default::default() };
        let captured_row = insert_account(&pool, &user.id, "grok", &stale).await;
        // Simulate dispatch completing OAuth refresh after it built its candidates.
        let fresh = encrypt_json(Some(TEST_TOKEN_KEY), &StoredCredential { access_token: "fresh-access".into(), ..Default::default() }).unwrap();
        update_account_payload(&pool, &captured_row.id, &fresh, None).await.unwrap();

        let adapter = StubAdapter::new(usage(Vec::new(), json!({}), None, false));
        let seen = adapter.seen_credential.clone();
        refresh_account_usage_in_background(&cx, &captured_row, &adapter).await;
        assert_eq!(seen.lock().unwrap().as_deref(), Some("fresh-access"));
    }

    // ---------------- fetchAndPersistUsage — soft failure vs. hard failure

    fn prior_snapshot() -> UsageSnapshot {
        UsageSnapshot {
            windows: vec![json!({ "label": "5h", "utilization": 10, "resets_at": null })],
            account: Some(json!({ "email": "a@example.com" }).as_object().cloned().unwrap()),
            error: None,
            stale: false,
            edge_blocked: false,
        }
    }

    /// Seeds an account (optionally with a prior snapshot) and takes its usage lock, as the
    /// vitest `setup` helper does.
    async fn setup(pool: &PgPool, prior: Option<UsageSnapshot>) -> (AccountRow, String) {
        let user = insert_user(pool, &format!("persist-{}@example.com", crate::ids::new_id("t"))).await;
        let row = insert_account(pool, &user.id, "grok", &StoredCredential { access_token: "tok".into(), ..Default::default() }).await;
        if let Some(prior) = prior {
            sqlx::query("UPDATE upstream_accounts SET usage_snapshot_json = $1, usage_fetched_at = $2 WHERE id = $3")
                .bind(serde_json::to_string(&prior).unwrap())
                .bind(crate::db::accounts::iso_from_ms(crate::app::now_ms() - 120_000))
                .bind(&row.id)
                .execute(pool)
                .await
                .unwrap();
        }
        let row = stored(pool, &user.id, &row.id).await;
        let token = acquire_usage_lock(pool, &row.id).await.unwrap().expect("expected to acquire the lock");
        (row, token)
    }

    #[tokio::test]
    async fn preserves_the_prior_snapshot_on_a_soft_failure_flagged_stale() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let cx = test_state(pool.clone(), MockTransport::new());
        let before = crate::ids::now_iso();
        let (row, token) = setup(&pool, Some(prior_snapshot())).await;
        let adapter = StubAdapter::new(usage(Vec::new(), json!({}), Some("usage 429"), true));

        let snapshot = fetch_and_persist_usage(&cx, &row, &adapter, &token).await.unwrap();
        assert_eq!(
            snapshot,
            UsageSnapshot {
                windows: prior_snapshot().windows,
                account: prior_snapshot().account,
                error: Some("usage 429".into()),
                stale: true,
                edge_blocked: false,
            }
        );
        let stored = stored(&pool, &row.user_id, &row.id).await;
        assert_eq!(serde_json::from_str::<UsageSnapshot>(stored.usage_snapshot_json.as_deref().unwrap()).unwrap(), snapshot);
        // The read genuinely happened: the lock is released via a write, not a bare release.
        assert!(stored.usage_fetching_at.is_none());
        assert!(stored.usage_fetched_at.as_deref().unwrap() >= before.as_str());
    }

    #[tokio::test]
    async fn writes_the_empty_errored_snapshot_unchanged_when_there_is_no_prior_snapshot() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let cx = test_state(pool.clone(), MockTransport::new());
        let before = crate::ids::now_iso();
        let (row, token) = setup(&pool, None).await;
        let adapter = StubAdapter::new(usage(Vec::new(), json!({}), Some("usage 429"), true));

        let snapshot = fetch_and_persist_usage(&cx, &row, &adapter, &token).await.unwrap();
        assert_eq!(
            snapshot,
            UsageSnapshot {
                windows: Vec::new(),
                account: Some(Map::new()),
                error: Some("usage 429".into()),
                stale: true,
                edge_blocked: false,
            }
        );
        let stored = stored(&pool, &row.user_id, &row.id).await;
        assert_eq!(serde_json::from_str::<UsageSnapshot>(stored.usage_snapshot_json.as_deref().unwrap()).unwrap(), snapshot);
        assert!(stored.usage_fetching_at.is_none());
        assert!(stored.usage_fetched_at.as_deref().unwrap() >= before.as_str());
    }

    #[tokio::test]
    async fn lets_windows_win_even_when_the_incoming_snapshot_also_carries_an_error() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let cx = test_state(pool.clone(), MockTransport::new());
        let before = crate::ids::now_iso();
        let (row, token) = setup(&pool, Some(prior_snapshot())).await;
        let adapter = StubAdapter::new(usage(
            vec![UsageWindow { label: "5h".into(), utilization: Some(42.0), resets_at: None, value: None }],
            json!({ "email": "new@example.com" }),
            Some("usage degraded"),
            false,
        ));

        let snapshot = fetch_and_persist_usage(&cx, &row, &adapter, &token).await.unwrap();
        assert_eq!(snapshot.windows, vec![json!({ "label": "5h", "utilization": 42.0, "resets_at": null })]);
        assert_eq!(snapshot.account, Some(json!({ "email": "new@example.com" }).as_object().cloned().unwrap()));
        assert_eq!(snapshot.error.as_deref(), Some("usage degraded"));
        assert!(!snapshot.stale);
        let stored = stored(&pool, &row.user_id, &row.id).await;
        assert_eq!(serde_json::from_str::<UsageSnapshot>(stored.usage_snapshot_json.as_deref().unwrap()).unwrap(), snapshot);
        assert!(stored.usage_fetching_at.is_none());
        assert!(stored.usage_fetched_at.as_deref().unwrap() >= before.as_str());
        // The upstream email becomes the pool label and merges into the stored meta.
        assert_eq!(stored.label.as_deref(), Some("new@example.com"));
        assert_eq!(stored.account_meta_json.as_deref(), Some(r#"{"email":"new@example.com"}"#));
    }

    #[tokio::test]
    async fn releases_the_lock_without_writing_on_a_hard_failure_preserving_the_prior_snapshot() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let cx = test_state(pool.clone(), MockTransport::new());
        let (row, token) = setup(&pool, Some(prior_snapshot())).await;
        let prior_fetched_at = row.usage_fetched_at.clone();
        // The Rust `fetchUsage` cannot throw, so the hard failure is the equivalent one this
        // port can reach: an undecryptable payload, before any upstream call.
        let mut broken = row.clone();
        broken.encrypted_payload = "not-a-ciphertext".into();
        let adapter = StubAdapter::new(usage(Vec::new(), json!({}), None, false));

        let result = fetch_and_persist_usage(&cx, &broken, &adapter, &token).await;
        assert!(result.is_err());
        assert!(adapter.seen_credential.lock().unwrap().is_none());
        let stored = stored(&pool, &row.user_id, &row.id).await;
        assert_eq!(serde_json::from_str::<UsageSnapshot>(stored.usage_snapshot_json.as_deref().unwrap()).unwrap(), prior_snapshot());
        assert_eq!(stored.usage_fetched_at, prior_fetched_at);
        assert!(stored.usage_fetching_at.is_none());
    }

    #[tokio::test]
    async fn background_refresh_short_circuits_on_fresh_usage_and_on_a_held_lock() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let cx = test_state(pool.clone(), MockTransport::new());
        // Fresh within the TTL: no upstream call at all.
        let (row, token) = setup(&pool, Some(prior_snapshot())).await;
        release_usage_lock(&pool, &row.id, &token).await.unwrap();
        sqlx::query("UPDATE upstream_accounts SET usage_fetched_at = $1 WHERE id = $2")
            .bind(crate::ids::now_iso())
            .bind(&row.id)
            .execute(&pool)
            .await
            .unwrap();
        let fresh_row = stored(&pool, &row.user_id, &row.id).await;
        let adapter = StubAdapter::new(usage(Vec::new(), json!({}), None, false));
        refresh_account_usage_in_background(&cx, &fresh_row, &adapter).await;
        assert!(adapter.seen_credential.lock().unwrap().is_none());

        // A lock another caller already holds: also a no-op, never a queued retry.
        let (locked_row, _held) = setup(&pool, None).await;
        refresh_account_usage_in_background(&cx, &locked_row, &adapter).await;
        assert!(adapter.seen_credential.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn the_spawned_form_runs_the_same_refresh() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let cx = AppState::builder(crate::db::test_support::test_config(), pool.clone()).transport(MockTransport::new()).background_work(true).build();
        let user = insert_user(&pool, "spawned@example.com").await;
        let row = insert_account(&pool, &user.id, "grok", &StoredCredential { access_token: "tok".into(), ..Default::default() }).await;
        let adapter = Arc::new(StubAdapter::new(usage(
            vec![UsageWindow { label: "5h".into(), utilization: Some(7.0), resets_at: None, value: None }],
            json!({}),
            None,
            false,
        )));
        spawn_account_usage_refresh(&cx, row.clone(), adapter.clone()).await.unwrap();
        let stored = stored(&pool, &user.id, &row.id).await;
        assert_eq!(
            read_usage_snapshot(&stored).unwrap().windows,
            vec![json!({ "label": "5h", "utilization": 7.0, "resets_at": null })]
        );
    }
}
