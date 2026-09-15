//! Account benching (apps/api/src/pool/bench.ts, docs/providers.md). A bench is a timestamp
//! on the account row, so reading it costs nothing extra once the row is loaded and an expired
//! value is simply ignored rather than deleted.

use sqlx::PgPool;

use crate::db::accounts::{get_account, parse_iso_ms, AccountRow};

const DEFAULT_COOLDOWN_MS: i64 = 300_000;
pub const DEFAULT_BENCH_REASON: &str = "refresh_failed";

/// Reads an account row's bench state without a separate query. Expired values read as no
/// bench and are never deleted.
pub fn bench_until_from_row(row: &AccountRow, now_ms: i64) -> Option<i64> {
    let until = parse_iso_ms(row.bench_until.as_deref()?)?;
    (until > now_ms).then_some(until)
}

/// Bench-until epoch-ms for one account, or `None` when no current bench exists.
pub async fn benched_until(
    db: &PgPool,
    user_id: &str,
    account_id: &str,
    now_ms: i64,
) -> Result<Option<i64>, sqlx::Error> {
    Ok(get_account(db, user_id, account_id).await?.and_then(|row| bench_until_from_row(&row, now_ms)))
}

pub async fn is_benched(db: &PgPool, user_id: &str, account_id: &str, now_ms: i64) -> Result<bool, sqlx::Error> {
    Ok(benched_until(db, user_id, account_id, now_ms).await?.is_some())
}

/// Extends an account bench atomically. A shorter concurrent penalty never truncates an
/// existing longer one; the reason changes only when the extension wins.
pub async fn mark_benched(
    db: &PgPool,
    user_id: &str,
    provider: &str,
    account_id: &str,
    cooldown_ms: Option<i64>,
    reason: Option<&str>,
    now_ms: i64,
) -> Result<(), sqlx::Error> {
    let until = crate::db::accounts::iso_from_ms(now_ms + cooldown_ms.unwrap_or(DEFAULT_COOLDOWN_MS));
    sqlx::query(
        "UPDATE upstream_accounts
         SET bench_until = $1, bench_reason = $2
         WHERE id = $3 AND user_id = $4 AND provider = $5
           AND (bench_until IS NULL OR bench_until < $1)",
    )
    .bind(&until)
    .bind(reason.unwrap_or(DEFAULT_BENCH_REASON))
    .bind(account_id)
    .bind(user_id)
    .bind(provider)
    .execute(db)
    .await?;
    Ok(())
}

/// Unpause is deliberately idempotent and clears both bench fields.
pub async fn clear_bench(db: &PgPool, user_id: &str, provider: &str, account_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE upstream_accounts
         SET bench_until = NULL, bench_reason = NULL
         WHERE id = $1 AND user_id = $2 AND provider = $3",
    )
    .bind(account_id)
    .bind(user_id)
    .bind(provider)
    .execute(db)
    .await?;
    Ok(())
}

/// Earliest current bench expiry across the supplied account ids — what a caller reports as
/// "try again at" when every candidate is benched.
pub async fn earliest_bench_expiry(
    db: &PgPool,
    user_id: &str,
    account_ids: &[String],
    now_ms: i64,
) -> Result<Option<i64>, sqlx::Error> {
    let mut earliest: Option<i64> = None;
    for account_id in account_ids {
        if let Some(until) = benched_until(db, user_id, account_id, now_ms).await? {
            earliest = Some(earliest.map_or(until, |e: i64| e.min(until)));
        }
    }
    Ok(earliest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::accounts::iso_from_ms;
    use crate::db::test_support::{insert_account, insert_user, skip_without_db, test_pool};
    use crate::pool::StoredCredential;

    #[test]
    fn an_expired_bench_reads_as_no_bench() {
        let mut row = AccountRow {
            id: "a".into(), user_id: "u".into(), provider: "codex".into(), external_account_id: None,
            label: None, custom_label: None, priority: 0, encrypted_payload: String::new(),
            account_meta_json: None, usage_snapshot_json: None, usage_fetched_at: None,
            usage_fetching_at: None, bench_until: None, bench_reason: None, refreshing_at: None,
            edge_strikes: 0, edge_strike_at: None, created_at: String::new(), updated_at: String::new(),
        };
        assert_eq!(bench_until_from_row(&row, 1_000), None);
        row.bench_until = Some(iso_from_ms(2_000));
        assert_eq!(bench_until_from_row(&row, 1_000), Some(2_000));
        assert_eq!(bench_until_from_row(&row, 2_000), None, "the expiry instant itself is over");
        row.bench_until = Some("garbage".into());
        assert_eq!(bench_until_from_row(&row, 1_000), None);
    }

    #[tokio::test]
    async fn a_longer_bench_is_never_truncated() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "bench@example.com").await;
        let row = insert_account(&pool, &user.id, "codex", &StoredCredential::default()).await;
        let now = 1_800_000_000_000i64;

        assert!(!is_benched(&pool, &user.id, &row.id, now).await.unwrap());
        mark_benched(&pool, &user.id, "codex", &row.id, Some(60_000), Some("rate_limited"), now).await.unwrap();
        assert_eq!(benched_until(&pool, &user.id, &row.id, now).await.unwrap(), Some(now + 60_000));

        // A shorter penalty leaves the longer bench and its reason alone.
        mark_benched(&pool, &user.id, "codex", &row.id, Some(1_000), Some("refresh_failed"), now).await.unwrap();
        assert_eq!(benched_until(&pool, &user.id, &row.id, now).await.unwrap(), Some(now + 60_000));
        let stored = crate::db::accounts::get_account(&pool, &user.id, &row.id).await.unwrap().unwrap();
        assert_eq!(stored.bench_reason.as_deref(), Some("rate_limited"));

        // A longer one extends it, reason and all.
        mark_benched(&pool, &user.id, "codex", &row.id, None, None, now).await.unwrap();
        assert_eq!(benched_until(&pool, &user.id, &row.id, now).await.unwrap(), Some(now + DEFAULT_COOLDOWN_MS));
        let stored = crate::db::accounts::get_account(&pool, &user.id, &row.id).await.unwrap().unwrap();
        assert_eq!(stored.bench_reason.as_deref(), Some(DEFAULT_BENCH_REASON));

        // Another user's row, and another provider, are never benched by this call.
        mark_benched(&pool, "someone-else", "codex", &row.id, Some(10_000_000), None, now).await.unwrap();
        mark_benched(&pool, &user.id, "grok", &row.id, Some(10_000_000), None, now).await.unwrap();
        assert_eq!(benched_until(&pool, &user.id, &row.id, now).await.unwrap(), Some(now + DEFAULT_COOLDOWN_MS));

        clear_bench(&pool, &user.id, "codex", &row.id).await.unwrap();
        assert!(!is_benched(&pool, &user.id, &row.id, now).await.unwrap());
        clear_bench(&pool, &user.id, "codex", &row.id).await.unwrap();
        assert!(!is_benched(&pool, &user.id, &row.id, now).await.unwrap(), "unpause is idempotent");
    }

    #[tokio::test]
    async fn the_earliest_expiry_wins() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "earliest@example.com").await;
        let a = insert_account(&pool, &user.id, "codex", &StoredCredential::default()).await;
        let b = insert_account(&pool, &user.id, "codex", &StoredCredential::default()).await;
        let free = insert_account(&pool, &user.id, "codex", &StoredCredential::default()).await;
        let now = 1_800_000_000_000i64;
        let ids = vec![a.id.clone(), b.id.clone(), free.id.clone()];
        assert_eq!(earliest_bench_expiry(&pool, &user.id, &ids, now).await.unwrap(), None);
        mark_benched(&pool, &user.id, "codex", &a.id, Some(200_000), None, now).await.unwrap();
        mark_benched(&pool, &user.id, "codex", &b.id, Some(50_000), None, now).await.unwrap();
        assert_eq!(earliest_bench_expiry(&pool, &user.id, &ids, now).await.unwrap(), Some(now + 50_000));
    }
}
