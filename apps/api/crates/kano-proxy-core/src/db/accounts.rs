//! `upstream_accounts` rows. The row shape is shared with
//! routing, pool and providers; the single-flight locks and the edge-timeout strike counter
//! are compare-and-swap statements, so concurrent requests cannot both win.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sqlx::PgPool;

use crate::ids::{iso_ms, new_id, now_iso};

/// `provider` is a builtin `ProviderId` or a custom/CLI provider slug.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct AccountRow {
    pub id: String,
    pub user_id: String,
    pub provider: String,
    pub external_account_id: Option<String>,
    pub label: Option<String>,
    pub custom_label: Option<String>,
    pub priority: i32,
    pub encrypted_payload: String,
    pub account_meta_json: Option<String>,
    pub usage_snapshot_json: Option<String>,
    pub usage_fetched_at: Option<String>,
    pub usage_fetching_at: Option<String>,
    pub bench_until: Option<String>,
    pub bench_reason: Option<String>,
    pub refreshing_at: Option<String>,
    pub edge_strikes: i32,
    pub edge_strike_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// `Date.parse(value)` — epoch milliseconds, or `None` when the text is not a timestamp.
pub fn parse_iso_ms(value: &str) -> Option<i64> {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(|t| (t.unix_timestamp_nanos() / 1_000_000) as i64)
}

/// `new Date(ms).toISOString()`.
pub fn iso_from_ms(ms: i64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    iso_ms(t)
}

pub async fn list_accounts(db: &PgPool, user_id: &str, provider: &str) -> Result<Vec<AccountRow>, sqlx::Error> {
    sqlx::query_as::<_, AccountRow>(
        "SELECT * FROM upstream_accounts
         WHERE user_id = $1 AND provider = $2
         ORDER BY priority DESC, created_at DESC",
    )
    .bind(user_id)
    .bind(provider)
    .fetch_all(db)
    .await
}

pub async fn get_account(db: &PgPool, user_id: &str, account_id: &str) -> Result<Option<AccountRow>, sqlx::Error> {
    sqlx::query_as::<_, AccountRow>("SELECT * FROM upstream_accounts WHERE id = $1 AND user_id = $2")
        .bind(account_id)
        .bind(user_id)
        .fetch_optional(db)
        .await
}

pub const EDGE_STRIKE_WINDOW_MS: i64 = 10 * 60 * 1000;

/// Atomically records one upstream edge-timeout strike. A stale prior strike restarts at one;
/// the third fresh strike resets to zero and returns `true` so the caller can apply the 30s
/// bench. `RETURNING` ties that decision to this exact write, so concurrent timeouts cannot
/// both bench or lose a strike.
pub async fn record_edge_timeout_strike(
    db: &PgPool,
    user_id: &str,
    provider: &str,
    account_id: &str,
    now_ms: i64,
) -> Result<bool, sqlx::Error> {
    let at = iso_from_ms(now_ms);
    let stale_before = iso_from_ms(now_ms - EDGE_STRIKE_WINDOW_MS);
    let strikes: Option<i32> = sqlx::query_scalar(
        "UPDATE upstream_accounts
         SET edge_strikes = CASE
               WHEN edge_strike_at IS NULL OR edge_strike_at < $1 THEN 1
               WHEN edge_strikes >= 2 THEN 0
               ELSE edge_strikes + 1
             END,
             edge_strike_at = $2, updated_at = $2
         WHERE id = $3 AND user_id = $4 AND provider = $5
         RETURNING edge_strikes",
    )
    .bind(&stale_before)
    .bind(&at)
    .bind(account_id)
    .bind(user_id)
    .bind(provider)
    .fetch_optional(db)
    .await?;
    Ok(strikes == Some(0))
}

/// Sets or clears the user-owned display name without changing upstream identity. Unlike the
/// COALESCE convention of [`update_account_identity`], `None` here is an explicit clear.
pub async fn set_account_custom_label(
    db: &PgPool,
    user_id: &str,
    account_id: &str,
    custom_label: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let r = sqlx::query("UPDATE upstream_accounts SET custom_label = $1, updated_at = $2 WHERE id = $3 AND user_id = $4")
        .bind(custom_label)
        .bind(now_iso())
        .bind(account_id)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(r.rows_affected() > 0)
}

#[derive(Debug, Clone, Default)]
pub struct NewAccount<'a> {
    pub user_id: &'a str,
    pub provider: &'a str,
    pub encrypted_payload: &'a str,
    pub label: Option<&'a str>,
    pub external_account_id: Option<&'a str>,
    pub account_meta_json: Option<&'a str>,
    /// Absent means "one above the pool's current maximum" — new accounts are tried first.
    pub priority: Option<i32>,
}

pub async fn insert_account(db: &PgPool, input: NewAccount<'_>) -> Result<AccountRow, sqlx::Error> {
    let id = new_id("acc");
    let ts = now_iso();
    let max: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(priority), 0) FROM upstream_accounts WHERE user_id = $1 AND provider = $2",
    )
    .bind(input.user_id)
    .bind(input.provider)
    .fetch_one(db)
    .await?;
    let priority = input.priority.unwrap_or(max + 1);
    sqlx::query(
        "INSERT INTO upstream_accounts
         (id, user_id, provider, external_account_id, label, priority, encrypted_payload, account_meta_json, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9)",
    )
    .bind(&id)
    .bind(input.user_id)
    .bind(input.provider)
    .bind(input.external_account_id)
    .bind(input.label)
    .bind(priority)
    .bind(input.encrypted_payload)
    .bind(input.account_meta_json)
    .bind(&ts)
    .execute(db)
    .await?;
    Ok(AccountRow {
        id,
        user_id: input.user_id.to_string(),
        provider: input.provider.to_string(),
        external_account_id: input.external_account_id.map(str::to_string),
        label: input.label.map(str::to_string),
        custom_label: None,
        priority,
        encrypted_payload: input.encrypted_payload.to_string(),
        account_meta_json: input.account_meta_json.map(str::to_string),
        usage_snapshot_json: None,
        usage_fetched_at: None,
        usage_fetching_at: None,
        bench_until: None,
        bench_reason: None,
        refreshing_at: None,
        edge_strikes: 0,
        edge_strike_at: None,
        created_at: ts.clone(),
        updated_at: ts,
    })
}

/// Identity fields that follow the COALESCE convention: `None` keeps the stored value.
#[derive(Debug, Clone, Copy, Default)]
pub struct AccountIdentity<'a> {
    pub label: Option<&'a str>,
    pub account_meta_json: Option<&'a str>,
}

pub async fn update_account_payload(
    db: &PgPool,
    account_id: &str,
    encrypted_payload: &str,
    meta: Option<AccountIdentity<'_>>,
) -> Result<(), sqlx::Error> {
    let ts = now_iso();
    match meta {
        Some(meta) => {
            sqlx::query(
                "UPDATE upstream_accounts SET encrypted_payload = $1, label = COALESCE($2, label),
                 account_meta_json = COALESCE($3, account_meta_json), updated_at = $4 WHERE id = $5",
            )
            .bind(encrypted_payload)
            .bind(meta.label)
            .bind(meta.account_meta_json)
            .bind(&ts)
            .bind(account_id)
            .execute(db)
            .await?;
        }
        None => {
            sqlx::query("UPDATE upstream_accounts SET encrypted_payload = $1, updated_at = $2 WHERE id = $3")
                .bind(encrypted_payload)
                .bind(&ts)
                .bind(account_id)
                .execute(db)
                .await?;
        }
    }
    Ok(())
}

/// Cached usage read for one account (docs/providers.md § Usage cache). `error`/`stale`/
/// `edge_blocked` travel with the windows because the route derives `status: "unusable"` from
/// all three — dropping them on a cache hit would silently reactivate an unusable account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub windows: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<Map<String, Value>>,
    pub error: Option<String>,
    pub stale: bool,
    #[serde(rename = "edgeBlocked")]
    pub edge_blocked: bool,
}

/// Server-side usage TTL: within this, a read never touches upstream.
pub const USAGE_TTL_MS: i64 = 120_000;

/// How long a lock may be held before another caller may break it. Bounds the damage from a
/// process that died mid-fetch, at the cost of allowing a second fetch past that point.
const USAGE_LOCK_TTL_MS: i64 = 30_000;

pub fn is_usage_fresh(row: &AccountRow, now_ms: i64) -> bool {
    let (Some(at), Some(_)) = (row.usage_fetched_at.as_deref(), row.usage_snapshot_json.as_deref()) else {
        return false;
    };
    parse_iso_ms(at).is_some_and(|at| now_ms - at < USAGE_TTL_MS)
}

/// A malformed blob reads as a miss, never as trusted data.
pub fn read_usage_snapshot(row: &AccountRow) -> Option<UsageSnapshot> {
    let raw = row.usage_snapshot_json.as_deref()?;
    let parsed: Value = serde_json::from_str(raw).ok()?;
    let windows = parsed.get("windows")?.as_array()?.clone();
    Some(UsageSnapshot {
        windows,
        account: parsed.get("account").and_then(|v| v.as_object().cloned()),
        error: parsed.get("error").and_then(|v| v.as_str()).map(str::to_string),
        stale: parsed.get("stale").and_then(Value::as_bool).unwrap_or(false),
        edge_blocked: parsed.get("edgeBlocked").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// Stored profile facts for routing rules: `account_meta_json` with the usage snapshot's
/// fresher `account` merged over it — the same merge the Providers page shows. Never an
/// upstream call; `None` when neither source exists.
pub fn account_profile_meta(row: &AccountRow) -> Option<Map<String, Value>> {
    let meta = row
        .account_meta_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|v| v.as_object().cloned());
    let snapshot_account = read_usage_snapshot(row).and_then(|s| s.account);
    if meta.is_none() && snapshot_account.is_none() {
        return None;
    }
    let mut out = meta.unwrap_or_default();
    if let Some(account) = snapshot_account {
        for (k, v) in account {
            out.insert(k, v);
        }
    }
    Some(out)
}

/// Single-flight lock acquire as a compare-and-swap: the WHERE decides the winner and the
/// affected-row count reports it. Returns the token to release with, or `None` when another
/// caller already holds a fresh lock.
pub async fn acquire_usage_lock(db: &PgPool, account_id: &str) -> Result<Option<String>, sqlx::Error> {
    let token = now_iso();
    let break_before = iso_from_ms(crate::app::now_ms() - USAGE_LOCK_TTL_MS);
    let r = sqlx::query(
        "UPDATE upstream_accounts
         SET usage_fetching_at = $1
         WHERE id = $2 AND (usage_fetching_at IS NULL OR usage_fetching_at < $3)",
    )
    .bind(&token)
    .bind(account_id)
    .bind(&break_before)
    .execute(db)
    .await?;
    Ok((r.rows_affected() > 0).then_some(token))
}

/// Writes the snapshot and releases the lock in one statement. The release is conditional on
/// still holding `token`: once the stale-lock breaker handed the lock to a second caller, an
/// unconditional release from the first would free the *second* caller's lock.
pub async fn write_usage_snapshot(
    db: &PgPool,
    account_id: &str,
    token: &str,
    snapshot: &UsageSnapshot,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE upstream_accounts
         SET usage_snapshot_json = $1, usage_fetched_at = $2, usage_fetching_at = NULL
         WHERE id = $3 AND usage_fetching_at = $4",
    )
    .bind(serde_json::to_string(snapshot).expect("snapshot serializes"))
    .bind(now_iso())
    .bind(account_id)
    .bind(token)
    .execute(db)
    .await?;
    Ok(())
}

/// Releases without writing — the upstream call failed. The previous snapshot is deliberately
/// left intact so one hiccup does not blank the usage bars.
pub async fn release_usage_lock(db: &PgPool, account_id: &str, token: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE upstream_accounts SET usage_fetching_at = NULL WHERE id = $1 AND usage_fetching_at = $2")
        .bind(account_id)
        .bind(token)
        .execute(db)
        .await?;
    Ok(())
}

/// OAuth refresh single-flight uses the same 30s breakable CAS pattern as usage.
pub async fn acquire_refresh_lock(db: &PgPool, account_id: &str) -> Result<Option<String>, sqlx::Error> {
    let token = now_iso();
    let break_before = iso_from_ms(crate::app::now_ms() - USAGE_LOCK_TTL_MS);
    let r = sqlx::query(
        "UPDATE upstream_accounts
         SET refreshing_at = $1
         WHERE id = $2 AND (refreshing_at IS NULL OR refreshing_at < $3)",
    )
    .bind(&token)
    .bind(account_id)
    .bind(&break_before)
    .execute(db)
    .await?;
    Ok((r.rows_affected() > 0).then_some(token))
}

/// Persists a refreshed credential and releases only the lock this caller owns.
pub async fn write_refreshed_credential(
    db: &PgPool,
    account_id: &str,
    token: &str,
    encrypted_payload: &str,
) -> Result<bool, sqlx::Error> {
    let r = sqlx::query(
        "UPDATE upstream_accounts
         SET encrypted_payload = $1, refreshing_at = NULL, updated_at = $2
         WHERE id = $3 AND refreshing_at = $4",
    )
    .bind(encrypted_payload)
    .bind(now_iso())
    .bind(account_id)
    .bind(token)
    .execute(db)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// Releases a failed OAuth refresh without changing its previous credential.
pub async fn release_refresh_lock(db: &PgPool, account_id: &str, token: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE upstream_accounts SET refreshing_at = NULL WHERE id = $1 AND refreshing_at = $2")
        .bind(account_id)
        .bind(token)
        .execute(db)
        .await?;
    Ok(())
}

/// Persists display label / meta without touching secrets (COALESCE convention).
pub async fn update_account_identity(
    db: &PgPool,
    account_id: &str,
    identity: AccountIdentity<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE upstream_accounts
         SET label = COALESCE($1, label),
             account_meta_json = COALESCE($2, account_meta_json),
             updated_at = $3
         WHERE id = $4",
    )
    .bind(identity.label)
    .bind(identity.account_meta_json)
    .bind(now_iso())
    .bind(account_id)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn promote_account(db: &PgPool, user_id: &str, account_id: &str) -> Result<bool, sqlx::Error> {
    let Some(row) = get_account(db, user_id, account_id).await? else {
        return Ok(false);
    };
    let max: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(priority), 0) FROM upstream_accounts WHERE user_id = $1 AND provider = $2",
    )
    .bind(user_id)
    .bind(&row.provider)
    .fetch_one(db)
    .await?;
    sqlx::query("UPDATE upstream_accounts SET priority = $1, updated_at = $2 WHERE id = $3")
        .bind(max + 1)
        .bind(now_iso())
        .bind(account_id)
        .execute(db)
        .await?;
    Ok(true)
}

pub async fn remove_account(db: &PgPool, user_id: &str, account_id: &str) -> Result<bool, sqlx::Error> {
    let r = sqlx::query("DELETE FROM upstream_accounts WHERE id = $1 AND user_id = $2")
        .bind(account_id)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(r.rows_affected() > 0)
}

pub async fn user_has_provider(db: &PgPool, user_id: &str, provider: &str) -> Result<bool, sqlx::Error> {
    let row: Option<i32> =
        sqlx::query_scalar("SELECT 1 FROM upstream_accounts WHERE user_id = $1 AND provider = $2 LIMIT 1")
            .bind(user_id)
            .bind(provider)
            .fetch_optional(db)
            .await?;
    Ok(row.is_some())
}

/// Bulk-deletes every account row for one (user, provider) — used when a custom provider is
/// removed (its accounts have no FK to cascade on). Returns the deleted rows so callers can
/// best-effort clear their bench state.
pub async fn delete_accounts_for_provider(
    db: &PgPool,
    user_id: &str,
    provider: &str,
) -> Result<Vec<AccountRow>, sqlx::Error> {
    let rows = list_accounts(db, user_id, provider).await?;
    sqlx::query("DELETE FROM upstream_accounts WHERE user_id = $1 AND provider = $2")
        .bind(user_id)
        .bind(provider)
        .execute(db)
        .await?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{insert_user, skip_without_db, test_pool};

    async fn seed(pool: &PgPool, user_id: &str, provider: &str) -> AccountRow {
        insert_account(
            pool,
            NewAccount { user_id, provider, encrypted_payload: "blob", label: Some("acct"), ..Default::default() },
        )
        .await
        .unwrap()
    }

    #[test]
    fn iso_round_trip() {
        assert_eq!(iso_from_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(parse_iso_ms("2026-08-04T15:30:00.000Z"), Some(1785857400000));
        assert_eq!(parse_iso_ms("not a date"), None);
    }

    #[test]
    fn usage_snapshot_reads_and_profile_merge() {
        let mut row = AccountRow {
            id: "a".into(), user_id: "u".into(), provider: "codex".into(), external_account_id: None,
            label: None, custom_label: None, priority: 0, encrypted_payload: String::new(),
            account_meta_json: Some(r#"{"plan":"pro","email":"a@b"}"#.into()),
            usage_snapshot_json: Some(r#"{"windows":[{"w":1}],"account":{"plan":"max"},"stale":true}"#.into()),
            usage_fetched_at: None, usage_fetching_at: None, bench_until: None, bench_reason: None,
            refreshing_at: None, edge_strikes: 0, edge_strike_at: None,
            created_at: String::new(), updated_at: String::new(),
        };
        let snap = read_usage_snapshot(&row).unwrap();
        assert_eq!(snap.windows.len(), 1);
        assert!(snap.stale);
        assert!(!snap.edge_blocked);
        assert_eq!(snap.error, None);
        let merged = account_profile_meta(&row).unwrap();
        assert_eq!(merged["plan"], "max", "the snapshot's account wins");
        assert_eq!(merged["email"], "a@b");

        row.usage_snapshot_json = Some("{not json".into());
        assert!(read_usage_snapshot(&row).is_none());
        row.usage_snapshot_json = Some(r#"{"windows":"nope"}"#.into());
        assert!(read_usage_snapshot(&row).is_none());
        row.account_meta_json = None;
        row.usage_snapshot_json = None;
        assert!(account_profile_meta(&row).is_none());

        row.usage_snapshot_json = Some(r#"{"windows":[]}"#.into());
        row.usage_fetched_at = Some(iso_from_ms(1_000_000));
        assert!(is_usage_fresh(&row, 1_000_000 + USAGE_TTL_MS - 1));
        assert!(!is_usage_fresh(&row, 1_000_000 + USAGE_TTL_MS));
        row.usage_snapshot_json = None;
        assert!(!is_usage_fresh(&row, 1_000_000));
    }

    #[tokio::test]
    async fn insert_list_promote_delete() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "acc@example.com").await;
        let first = seed(&pool, &user.id, "codex").await;
        let second = seed(&pool, &user.id, "codex").await;
        assert_eq!(first.priority, 1);
        assert_eq!(second.priority, 2, "new accounts outrank the pool");
        let rows = list_accounts(&pool, &user.id, "codex").await.unwrap();
        assert_eq!(rows.iter().map(|r| r.id.clone()).collect::<Vec<_>>(), vec![second.id.clone(), first.id.clone()]);

        assert!(promote_account(&pool, &user.id, &first.id).await.unwrap());
        assert_eq!(list_accounts(&pool, &user.id, "codex").await.unwrap()[0].id, first.id);
        assert!(!promote_account(&pool, "someone-else", &first.id).await.unwrap());

        assert!(user_has_provider(&pool, &user.id, "codex").await.unwrap());
        assert!(!user_has_provider(&pool, &user.id, "grok").await.unwrap());
        assert!(get_account(&pool, "someone-else", &first.id).await.unwrap().is_none());

        assert!(set_account_custom_label(&pool, &user.id, &first.id, Some("mine")).await.unwrap());
        assert_eq!(get_account(&pool, &user.id, &first.id).await.unwrap().unwrap().custom_label.as_deref(), Some("mine"));
        set_account_custom_label(&pool, &user.id, &first.id, None).await.unwrap();
        assert_eq!(get_account(&pool, &user.id, &first.id).await.unwrap().unwrap().custom_label, None);

        assert!(!remove_account(&pool, "someone-else", &first.id).await.unwrap());
        assert!(remove_account(&pool, &user.id, &first.id).await.unwrap());
        let deleted = delete_accounts_for_provider(&pool, &user.id, "codex").await.unwrap();
        assert_eq!(deleted.len(), 1);
        assert!(list_accounts(&pool, &user.id, "codex").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn edge_strikes_bench_on_the_third_and_reset_when_stale() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "strike@example.com").await;
        let row = seed(&pool, &user.id, "codex").await;
        let t0 = 1_800_000_000_000i64;
        assert!(!record_edge_timeout_strike(&pool, &user.id, "codex", &row.id, t0).await.unwrap());
        assert!(!record_edge_timeout_strike(&pool, &user.id, "codex", &row.id, t0 + 1).await.unwrap());
        assert!(record_edge_timeout_strike(&pool, &user.id, "codex", &row.id, t0 + 2).await.unwrap(), "third strike benches");
        // After the reset the counter starts over.
        assert!(!record_edge_timeout_strike(&pool, &user.id, "codex", &row.id, t0 + 3).await.unwrap());
        // A strike older than the window (measured from the last one) restarts at one rather
        // than accumulating.
        let later = t0 + 3 + EDGE_STRIKE_WINDOW_MS + 1;
        assert!(!record_edge_timeout_strike(&pool, &user.id, "codex", &row.id, later).await.unwrap());
        assert_eq!(get_account(&pool, &user.id, &row.id).await.unwrap().unwrap().edge_strikes, 1);
        // A row belonging to someone else is never struck.
        assert!(!record_edge_timeout_strike(&pool, "someone-else", "codex", &row.id, later).await.unwrap());
    }

    #[tokio::test]
    async fn locks_are_single_flight_and_release_only_their_own_token() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "lock@example.com").await;
        let row = seed(&pool, &user.id, "codex").await;

        let token = acquire_usage_lock(&pool, &row.id).await.unwrap().expect("first caller wins");
        assert!(acquire_usage_lock(&pool, &row.id).await.unwrap().is_none(), "second caller is locked out");
        release_usage_lock(&pool, &row.id, "not-the-token").await.unwrap();
        assert!(acquire_usage_lock(&pool, &row.id).await.unwrap().is_none(), "a foreign token never releases");
        let snapshot = UsageSnapshot { windows: vec![serde_json::json!({"w":1})], account: None, error: None, stale: false, edge_blocked: false };
        write_usage_snapshot(&pool, &row.id, &token, &snapshot).await.unwrap();
        let stored = get_account(&pool, &user.id, &row.id).await.unwrap().unwrap();
        assert_eq!(read_usage_snapshot(&stored).unwrap(), snapshot);
        assert!(stored.usage_fetching_at.is_none());
        assert!(acquire_usage_lock(&pool, &row.id).await.unwrap().is_some());

        let refresh = acquire_refresh_lock(&pool, &row.id).await.unwrap().expect("refresh lock");
        assert!(acquire_refresh_lock(&pool, &row.id).await.unwrap().is_none());
        assert!(!write_refreshed_credential(&pool, &row.id, "wrong", "new-blob").await.unwrap());
        assert!(write_refreshed_credential(&pool, &row.id, &refresh, "new-blob").await.unwrap());
        assert_eq!(get_account(&pool, &user.id, &row.id).await.unwrap().unwrap().encrypted_payload, "new-blob");
        let refresh = acquire_refresh_lock(&pool, &row.id).await.unwrap().unwrap();
        release_refresh_lock(&pool, &row.id, &refresh).await.unwrap();
        assert!(acquire_refresh_lock(&pool, &row.id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn payload_and_identity_updates_follow_the_coalesce_convention() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "meta@example.com").await;
        let row = seed(&pool, &user.id, "codex").await;
        update_account_payload(&pool, &row.id, "blob2", None).await.unwrap();
        let stored = get_account(&pool, &user.id, &row.id).await.unwrap().unwrap();
        assert_eq!(stored.encrypted_payload, "blob2");
        assert_eq!(stored.label.as_deref(), Some("acct"));

        update_account_payload(&pool, &row.id, "blob3", Some(AccountIdentity { label: None, account_meta_json: Some("{}") }))
            .await
            .unwrap();
        let stored = get_account(&pool, &user.id, &row.id).await.unwrap().unwrap();
        assert_eq!(stored.label.as_deref(), Some("acct"), "None keeps the stored label");
        assert_eq!(stored.account_meta_json.as_deref(), Some("{}"));

        update_account_identity(&pool, &row.id, AccountIdentity { label: Some("renamed"), account_meta_json: None })
            .await
            .unwrap();
        let stored = get_account(&pool, &user.id, &row.id).await.unwrap().unwrap();
        assert_eq!(stored.label.as_deref(), Some("renamed"));
        assert_eq!(stored.account_meta_json.as_deref(), Some("{}"));
        assert_eq!(stored.encrypted_payload, "blob3");
    }
}
