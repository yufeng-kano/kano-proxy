//! Per-key spend windows (apps/api/src/auth/spend_limit.ts, docs/pricing.md).
//!
//! The window sum reads only stored `request_logs.cost` values through one indexed aggregate.
//! Enforcement memoizes that sum for 60s so a busy key does not add an aggregate to every
//! request; the admin keys list calls the uncached form for fresh display. A storage failure
//! reads as "unknown" (`None`) and the middleware fails open — an infrastructure hiccup must
//! not take the proxy down.

use std::collections::HashMap;
use std::sync::Mutex;

use once_cell::sync::Lazy;
use sqlx::PgPool;
use time::{Date, OffsetDateTime, Time};

use crate::db::keys::ApiKeyRow;
use crate::ids::iso_ms;
use crate::providers::PROVIDERS;

pub const SPEND_LIMIT_INTERVALS: [&str; 4] = ["daily", "weekly", "monthly", "total"];

pub fn is_spend_limit_interval(v: &str) -> bool {
    SPEND_LIMIT_INTERVALS.contains(&v)
}

/// The fields of a key the window sum needs; the whole row is not required.
#[derive(Debug, Clone, PartialEq)]
pub struct SpendKey {
    pub id: String,
    pub spend_limit_interval: String,
    pub spend_limit_include_oauth: i32,
}

impl From<&ApiKeyRow> for SpendKey {
    fn from(row: &ApiKeyRow) -> Self {
        Self {
            id: row.id.clone(),
            spend_limit_interval: row.spend_limit_interval.clone(),
            spend_limit_include_oauth: row.spend_limit_include_oauth,
        }
    }
}

fn utc_midnight(date: Date) -> String {
    iso_ms(OffsetDateTime::new_utc(date, Time::MIDNIGHT))
}

/// Inclusive ISO lower bound of the current window. An unknown value falls back to monthly —
/// the column default — rather than failing on a hand-edited row.
pub fn spend_window_start(interval: &str, now_ms: i64) -> String {
    if interval == "total" {
        return "1970-01-01T00:00:00.000Z".to_string();
    }
    let now = OffsetDateTime::from_unix_timestamp_nanos(now_ms as i128 * 1_000_000)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    let date = now.date();
    match interval {
        "daily" => utc_midnight(date),
        // ISO week: Monday 00:00 UTC.
        "weekly" => {
            let since_monday = date.weekday().number_days_from_monday() as i64;
            utc_midnight(date - time::Duration::days(since_monday))
        }
        _ => utc_midnight(Date::from_calendar_date(date.year(), date.month(), 1).unwrap_or(date)),
    }
}

/// Estimated USD this key spent in its current window; `None` on a storage failure.
/// `include_oauth = 0` excludes the builtin subscription providers — custom (BYO-key) traffic
/// always counts.
pub async fn key_window_spend(db: &PgPool, key: &SpendKey, now_ms: i64) -> Option<f64> {
    let since = spend_window_start(&key.spend_limit_interval, now_ms);
    let result: Result<f64, sqlx::Error> = if key.spend_limit_include_oauth == 0 {
        let builtins: Vec<String> = PROVIDERS.iter().map(|p| p.as_str().to_string()).collect();
        sqlx::query_scalar(
            "SELECT COALESCE(SUM(cost), 0) FROM request_logs
             WHERE api_key_id = $1 AND created_at >= $2 AND provider <> ALL($3)",
        )
        .bind(&key.id)
        .bind(&since)
        .bind(&builtins)
        .fetch_one(db)
        .await
    } else {
        sqlx::query_scalar(
            "SELECT COALESCE(SUM(cost), 0) FROM request_logs WHERE api_key_id = $1 AND created_at >= $2",
        )
        .bind(&key.id)
        .bind(&since)
        .fetch_one(db)
        .await
    };
    match result {
        Ok(sum) => Some(sum),
        Err(err) => {
            tracing::warn!(error = %err, "spend window sum unavailable; failing open");
            None
        }
    }
}

const MEMO_TTL_MS: i64 = 60_000;

/// Keyed by id plus the limit settings, so a PATCH starts a fresh entry. Process-wide, as the
/// isolate-wide map was: the key ids it holds are globally unique, so no two apps can read
/// each other's numbers by accident.
static MEMO: Lazy<Mutex<HashMap<String, (i64, f64)>>> = Lazy::new(|| Mutex::new(HashMap::new()));

pub fn reset_spend_memo_for_tests() {
    MEMO.lock().expect("memo lock").clear();
}

/// Memoized [`key_window_spend`] for the request path. A `None` (storage failure) is never
/// memoized, so the next request retries instead of caching an outage.
pub async fn key_window_spend_cached(db: &PgPool, key: &SpendKey, now_ms: i64) -> Option<f64> {
    let memo_key = format!("{}:{}:{}", key.id, key.spend_limit_interval, key.spend_limit_include_oauth);
    if let Some((at, spend)) = MEMO.lock().expect("memo lock").get(&memo_key).copied() {
        if now_ms - at < MEMO_TTL_MS {
            return Some(spend);
        }
    }
    let spend = key_window_spend(db, key, now_ms).await?;
    let mut memo = MEMO.lock().expect("memo lock");
    memo.insert(memo_key, (now_ms, spend));
    // The map only ever holds keys this process has seen, but a long-lived process serving
    // many keys should not grow it unboundedly.
    if memo.len() > 1000 {
        let drop: Vec<String> = memo.keys().take(memo.len() - 500).cloned().collect();
        for k in drop {
            memo.remove(&k);
        }
    }
    Some(spend)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::keys::SpendLimitFields;
    use crate::db::request_logs::{insert_request_log, RequestLogEntry};
    use crate::db::test_support::{insert_api_key_with_limit, insert_user, skip_without_db, test_pool};

    async fn seed_spend(pool: &PgPool, user_id: &str, key_id: &str, cost: Option<f64>, provider: &str, created_at: &str) {
        let id = insert_request_log(
            pool,
            &RequestLogEntry {
                user_id: user_id.into(),
                api_key_id: Some(key_id.into()),
                provider: provider.into(),
                model: "m".into(),
                status_code: 200,
                latency_ms: 1,
                cost,
                started_at: created_at.into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        sqlx::query("UPDATE request_logs SET created_at = $1 WHERE id = $2")
            .bind(created_at)
            .bind(&id)
            .execute(pool)
            .await
            .unwrap();
    }

    #[test]
    fn window_starts() {
        // 2026-08-04 is a Tuesday.
        let now = crate::db::accounts::parse_iso_ms("2026-08-04T15:30:00.000Z").unwrap();
        assert_eq!(spend_window_start("daily", now), "2026-08-04T00:00:00.000Z");
        assert_eq!(spend_window_start("weekly", now), "2026-08-03T00:00:00.000Z");
        assert_eq!(spend_window_start("monthly", now), "2026-08-01T00:00:00.000Z");
        assert_eq!(spend_window_start("total", now), "1970-01-01T00:00:00.000Z");
        // An unknown interval falls back to monthly, never to "no window".
        assert_eq!(spend_window_start("bogus", now), "2026-08-01T00:00:00.000Z");
        // A Monday is its own week start; Sunday reaches back six days.
        let monday = crate::db::accounts::parse_iso_ms("2026-08-03T00:00:00.000Z").unwrap();
        assert_eq!(spend_window_start("weekly", monday), "2026-08-03T00:00:00.000Z");
        let sunday = crate::db::accounts::parse_iso_ms("2026-08-09T23:59:59.000Z").unwrap();
        assert_eq!(spend_window_start("weekly", sunday), "2026-08-03T00:00:00.000Z");
    }

    #[test]
    fn interval_vocabulary() {
        for v in SPEND_LIMIT_INTERVALS {
            assert!(is_spend_limit_interval(v));
        }
        assert!(!is_spend_limit_interval("hourly"));
        assert!(!is_spend_limit_interval(""));
    }

    #[tokio::test]
    async fn sums_only_this_key_inside_the_window() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "spend@example.com").await;
        let limits = SpendLimitFields { spend_limit: Some(10.0), spend_limit_interval: "monthly".into(), spend_limit_include_oauth: true };
        let (row, _) = insert_api_key_with_limit(&pool, &user.id, limits).await;
        let now = crate::db::accounts::parse_iso_ms("2026-08-04T15:30:00.000Z").unwrap();
        let inside = "2026-08-04T00:00:01.000Z";
        seed_spend(&pool, &user.id, &row.id, Some(1.25), "claude-code", inside).await;
        seed_spend(&pool, &user.id, &row.id, Some(0.75), "claude-code", inside).await;
        // A NULL cost contributes nothing; older rows and other keys are out of scope.
        seed_spend(&pool, &user.id, &row.id, None, "claude-code", inside).await;
        seed_spend(&pool, &user.id, &row.id, Some(5.0), "claude-code", "2020-01-01T00:00:00.000Z").await;
        seed_spend(&pool, &user.id, "key_other", Some(3.0), "claude-code", inside).await;
        let spend = key_window_spend(&pool, &SpendKey::from(&row), now).await.unwrap();
        assert!((spend - 2.0).abs() < 1e-9, "got {spend}");
    }

    #[tokio::test]
    async fn include_oauth_zero_excludes_builtin_providers_only() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "oauth@example.com").await;
        let limits = SpendLimitFields { spend_limit: Some(10.0), spend_limit_interval: "monthly".into(), spend_limit_include_oauth: false };
        let (row, _) = insert_api_key_with_limit(&pool, &user.id, limits).await;
        let now = crate::db::accounts::parse_iso_ms("2026-08-04T15:30:00.000Z").unwrap();
        let inside = "2026-08-04T00:00:01.000Z";
        for provider in ["claude-code", "codex", "grok", "antigravity"] {
            seed_spend(&pool, &user.id, &row.id, Some(10.0), provider, inside).await;
        }
        seed_spend(&pool, &user.id, &row.id, Some(2.5), "my-endpoint", inside).await;
        let spend = key_window_spend(&pool, &SpendKey::from(&row), now).await.unwrap();
        assert!((spend - 2.5).abs() < 1e-9, "got {spend}");
        // The same rows with include_oauth = 1 count in full.
        let all = SpendKey { spend_limit_include_oauth: 1, ..SpendKey::from(&row) };
        assert!((key_window_spend(&pool, &all, now).await.unwrap() - 42.5).abs() < 1e-9);
    }

    #[tokio::test]
    async fn a_storage_failure_reads_as_unknown_and_is_never_memoized() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "fail@example.com").await;
        let limits = SpendLimitFields { spend_limit: Some(1.0), spend_limit_interval: "monthly".into(), spend_limit_include_oauth: true };
        let (row, _) = insert_api_key_with_limit(&pool, &user.id, limits).await;
        let now = crate::app::now_ms();
        // Dropping the table is the local stand-in for the D1 outage the TypeScript stubbed.
        sqlx::query("ALTER TABLE request_logs RENAME TO request_logs_hidden").execute(&pool).await.unwrap();
        reset_spend_memo_for_tests();
        assert_eq!(key_window_spend(&pool, &SpendKey::from(&row), now).await, None);
        assert_eq!(key_window_spend_cached(&pool, &SpendKey::from(&row), now).await, None);
        sqlx::query("ALTER TABLE request_logs_hidden RENAME TO request_logs").execute(&pool).await.unwrap();
        assert_eq!(key_window_spend_cached(&pool, &SpendKey::from(&row), now).await, Some(0.0), "the outage was not cached");
    }

    #[tokio::test]
    async fn the_cached_sum_holds_for_the_ttl() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "memo@example.com").await;
        let limits = SpendLimitFields { spend_limit: Some(10.0), spend_limit_interval: "monthly".into(), spend_limit_include_oauth: true };
        let (row, _) = insert_api_key_with_limit(&pool, &user.id, limits).await;
        let now = crate::db::accounts::parse_iso_ms("2026-08-04T15:30:00.000Z").unwrap();
        let inside = "2026-08-04T00:00:01.000Z";
        reset_spend_memo_for_tests();
        seed_spend(&pool, &user.id, &row.id, Some(1.0), "claude-code", inside).await;
        let spend_key = SpendKey::from(&row);
        assert_eq!(key_window_spend_cached(&pool, &spend_key, now).await, Some(1.0));
        seed_spend(&pool, &user.id, &row.id, Some(1.0), "claude-code", inside).await;
        assert_eq!(key_window_spend_cached(&pool, &spend_key, now).await, Some(1.0), "the memo serves, not the new row");
        assert_eq!(key_window_spend_cached(&pool, &spend_key, now + MEMO_TTL_MS).await, Some(2.0), "the TTL expires it");
        // A settings change starts a fresh memo entry rather than reusing the old sum.
        let changed = SpendKey { spend_limit_interval: "daily".into(), ..spend_key.clone() };
        assert_eq!(key_window_spend_cached(&pool, &changed, now).await, Some(2.0));
    }
}
