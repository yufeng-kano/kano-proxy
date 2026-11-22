//! Account benching (docs/providers.md). A bench is a timestamp
//! on the account row, so reading it costs nothing extra once the row is loaded and an expired
//! value is simply ignored rather than deleted.

use sqlx::PgPool;

use crate::db::accounts::{parse_iso_ms, AccountRow};

/// Reads an account row's bench state without a separate query. Expired values read as no
/// bench and are never deleted.
pub fn bench_until_from_row(row: &AccountRow, now_ms: i64) -> Option<i64> {
    let until = parse_iso_ms(row.bench_until.as_deref()?)?;
    (until > now_ms).then_some(until)
}

/// The current bench on one quota scope of the row (`bench_scopes_json`), read the same way as
/// `bench_until`: expired or malformed entries read as no bench and are never deleted.
pub fn scope_bench_until_from_row(row: &AccountRow, scope: &str, now_ms: i64) -> Option<i64> {
    let scopes: serde_json::Value = serde_json::from_str(row.bench_scopes_json.as_deref()?).ok()?;
    let until = parse_iso_ms(scopes.get(scope)?.get("until")?.as_str()?)?;
    (until > now_ms).then_some(until)
}

/// Whether one stored account is benched at `now_ms`, read with its own query.
#[cfg(test)]
pub(crate) async fn is_benched(db: &PgPool, user_id: &str, account_id: &str, now_ms: i64) -> Result<bool, sqlx::Error> {
    let row = crate::db::accounts::get_account(db, user_id, account_id).await?;
    Ok(row.is_some_and(|row| bench_until_from_row(&row, now_ms).is_some()))
}

/// Extends an account bench atomically. A shorter concurrent penalty never truncates an
/// existing longer one; the reason changes only when the extension wins.
pub async fn mark_benched(
    db: &PgPool,
    user_id: &str,
    provider: &str,
    account_id: &str,
    cooldown_ms: i64,
    reason: &str,
    now_ms: i64,
) -> Result<(), sqlx::Error> {
    let until = crate::db::accounts::iso_from_ms(now_ms + cooldown_ms);
    sqlx::query(
        "UPDATE upstream_accounts
         SET bench_until = $1, bench_reason = $2
         WHERE id = $3 AND user_id = $4 AND provider = $5
           AND (bench_until IS NULL OR bench_until < $1)",
    )
    .bind(&until)
    .bind(reason)
    .bind(account_id)
    .bind(user_id)
    .bind(provider)
    .execute(db)
    .await?;
    Ok(())
}

/// Extends the bench on one quota scope of an account (docs/providers.md § Routing module
/// "Quota scopes"), with the same monotonic rule as [`mark_benched`] applied to that scope's
/// entry alone: ISO strings in one format compare in time order, so a shorter concurrent
/// penalty never truncates a longer one, and the other scopes' entries are left untouched.
#[allow(clippy::too_many_arguments)]
pub async fn mark_scope_benched(
    db: &PgPool,
    user_id: &str,
    provider: &str,
    account_id: &str,
    scope: &str,
    cooldown_ms: i64,
    reason: &str,
    now_ms: i64,
) -> Result<(), sqlx::Error> {
    let until = crate::db::accounts::iso_from_ms(now_ms + cooldown_ms);
    sqlx::query(
        "UPDATE upstream_accounts
         SET bench_scopes_json = (COALESCE(bench_scopes_json::jsonb, '{}'::jsonb)
                                  || jsonb_build_object($1::text, jsonb_build_object('until', $2::text, 'reason', $3::text)))::text
         WHERE id = $4 AND user_id = $5 AND provider = $6
           AND (bench_scopes_json IS NULL
                OR (bench_scopes_json::jsonb -> $1::text ->> 'until') IS NULL
                OR (bench_scopes_json::jsonb -> $1::text ->> 'until') < $2::text)",
    )
    .bind(scope)
    .bind(&until)
    .bind(reason)
    .bind(account_id)
    .bind(user_id)
    .bind(provider)
    .execute(db)
    .await?;
    Ok(())
}

/// Unpause is deliberately idempotent and clears every bench on the row, scoped ones included.
pub async fn clear_bench(db: &PgPool, user_id: &str, provider: &str, account_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE upstream_accounts
         SET bench_until = NULL, bench_reason = NULL, bench_scopes_json = NULL
         WHERE id = $1 AND user_id = $2 AND provider = $3",
    )
    .bind(account_id)
    .bind(user_id)
    .bind(provider)
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::accounts::{get_account, iso_from_ms};
    use crate::db::test_support::{insert_account, insert_user, skip_without_db, test_pool};
    use crate::pool::StoredCredential;

    #[test]
    fn an_expired_bench_reads_as_no_bench() {
        let mut row = AccountRow {
            id: "a".into(), user_id: "u".into(), provider: "codex".into(), external_account_id: None,
            label: None, custom_label: None, priority: 0, encrypted_payload: String::new(),
            account_meta_json: None, usage_snapshot_json: None, usage_fetched_at: None,
            usage_fetching_at: None, bench_until: None, bench_reason: None, refreshing_at: None, refresh_failure_json: None, sign_in_rejected_at: None,
            bench_scopes_json: None,
            edge_strikes: 0, edge_strike_at: None, allow_credits: false, strip_fast_mode: false,
            strip_claude_code_line: false, force_cache_ttl_1h: false, created_at: String::new(), updated_at: String::new(),
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
        let stored = || get_account(&pool, &user.id, &row.id);

        assert!(!is_benched(&pool, &user.id, &row.id, now).await.unwrap());
        mark_benched(&pool, &user.id, "codex", &row.id, 60_000, "rate_limited", now).await.unwrap();
        assert_eq!(bench_until_from_row(&stored().await.unwrap().unwrap(), now), Some(now + 60_000));

        // A shorter penalty leaves the longer bench and its reason alone.
        mark_benched(&pool, &user.id, "codex", &row.id, 1_000, "429", now).await.unwrap();
        let after = stored().await.unwrap().unwrap();
        assert_eq!(bench_until_from_row(&after, now), Some(now + 60_000));
        assert_eq!(after.bench_reason.as_deref(), Some("rate_limited"));

        // A longer one extends it, reason and all.
        mark_benched(&pool, &user.id, "codex", &row.id, 300_000, "401", now).await.unwrap();
        let after = stored().await.unwrap().unwrap();
        assert_eq!(bench_until_from_row(&after, now), Some(now + 300_000));
        assert_eq!(after.bench_reason.as_deref(), Some("401"));

        // Another user's row, and another provider, are never benched by this call.
        mark_benched(&pool, "someone-else", "codex", &row.id, 10_000_000, "429", now).await.unwrap();
        mark_benched(&pool, &user.id, "grok", &row.id, 10_000_000, "429", now).await.unwrap();
        assert_eq!(bench_until_from_row(&stored().await.unwrap().unwrap(), now), Some(now + 300_000));

        clear_bench(&pool, &user.id, "codex", &row.id).await.unwrap();
        assert!(!is_benched(&pool, &user.id, &row.id, now).await.unwrap());
        clear_bench(&pool, &user.id, "codex", &row.id).await.unwrap();
        assert!(!is_benched(&pool, &user.id, &row.id, now).await.unwrap(), "unpause is idempotent");
    }

    #[tokio::test]
    async fn a_scoped_bench_only_extends_its_own_scope_and_unpause_clears_it() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "scoped@example.com").await;
        let row = insert_account(&pool, &user.id, "antigravity", &StoredCredential::default()).await;
        let now = 1_000_000;
        mark_scope_benched(&pool, &user.id, "antigravity", &row.id, "Argon", 60_000, "429", now).await.unwrap();
        mark_scope_benched(&pool, &user.id, "antigravity", &row.id, "Argon", 1_000, "429", now).await.unwrap();
        mark_scope_benched(&pool, &user.id, "antigravity", &row.id, "Gemini", 5_000, "429", now).await.unwrap();
        let stored = get_account(&pool, &user.id, &row.id).await.unwrap().unwrap();
        assert_eq!(scope_bench_until_from_row(&stored, "Argon", now), Some(now + 60_000), "a shorter penalty never truncates");
        assert_eq!(scope_bench_until_from_row(&stored, "Gemini", now), Some(now + 5_000));
        assert_eq!(scope_bench_until_from_row(&stored, "Other", now), None);
        assert_eq!(bench_until_from_row(&stored, now), None, "the account itself is untouched");
        assert_eq!(scope_bench_until_from_row(&stored, "Argon", now + 60_000), None, "expired reads as no bench");

        clear_bench(&pool, &user.id, "antigravity", &row.id).await.unwrap();
        let cleared = get_account(&pool, &user.id, &row.id).await.unwrap().unwrap();
        assert_eq!(cleared.bench_scopes_json, None);
    }
}
