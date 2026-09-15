//! Port of apps/api/src/maintenance/retention.ts plus the Worker's `scheduled`
//! handler and its `[triggers] crons = ["17 3 * * *"]` (docs/logging.md
//! § Retention sweep, docs/deployment.md, docs/database.md).
//!
//! Deletes `request_logs` rows past the retention window in bounded batches, plus
//! expired `sessions`, `oauth_login_states` and `cli_login_requests` rows. Nothing
//! here is ever called from the request path: [`spawn_scheduler`] owns the daily
//! wake-up, which in the Worker was Cloudflare's cron.

use std::sync::Arc;

use futures::future::BoxFuture;
use sqlx::PgPool;
use time::{Duration as TimeDuration, OffsetDateTime};

use crate::db::accounts::iso_from_ms;
use crate::ids::now_iso;
use crate::AppState;

/// Rows deleted per DELETE statement, so one sweep run never issues a single huge query.
pub const REQUEST_LOG_BATCH_SIZE: i64 = 2000;

/// Hard cap on batches per run — bounds run time; a larger backlog drains over more days.
pub const MAX_BATCHES_PER_RUN: usize = 40;

/// UTC hour and minute of the daily sweep — the Worker's `17 3 * * *` cron.
pub const SWEEP_HOUR: u8 = 3;
pub const SWEEP_MINUTE: u8 = 17;

/// What one sweep removed (the TypeScript `runRetentionSweep` return shape).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RetentionCounts {
    pub request_logs: u64,
    pub sessions: u64,
    pub oauth_states: u64,
    pub cli_login_requests: u64,
}

/// An edition's own retention, chained after the core's inside the same daily run
/// (the cloud edition's usage rollups). Failures are the hook's own business — the
/// core sweep has already committed by the time it runs.
pub type ExtraSweep = Arc<dyn Fn(AppState) -> BoxFuture<'static, ()> + Send + Sync>;

/// Deletes `request_logs` rows with `created_at` strictly before `cutoff`, in batches
/// of `batch_size` via the portable id-subquery form. Loops while a batch comes back
/// full, up to `max_batches`, so one run is always bounded — the batch size and cap
/// are parameters so both can be exercised without seeding tens of thousands of rows.
pub async fn sweep_request_logs(
    db: &PgPool,
    cutoff: &str,
    batch_size: i64,
    max_batches: usize,
) -> Result<u64, sqlx::Error> {
    let mut deleted = 0u64;
    for _ in 0..max_batches {
        let affected = sqlx::query(
            "DELETE FROM request_logs
             WHERE id IN (SELECT id FROM request_logs WHERE created_at < $1 LIMIT $2)",
        )
        .bind(cutoff)
        .bind(batch_size)
        .execute(db)
        .await?
        .rows_affected();
        deleted += affected;
        if affected < batch_size as u64 {
            break;
        }
    }
    Ok(deleted)
}

async fn delete_expired(db: &PgPool, table: &str, now: &str) -> Result<u64, sqlx::Error> {
    // `table` is one of this module's own literals, never user input.
    let sql = format!("DELETE FROM {table} WHERE expires_at < $1");
    Ok(sqlx::query(&sql).bind(now).execute(db).await?.rows_affected())
}

/// One full sweep. Errors propagate; [`spawn_scheduler`] is what logs and swallows
/// them, so a failed sweep never escapes as an unhandled task panic.
pub async fn run_retention(cx: &AppState) -> Result<RetentionCounts, sqlx::Error> {
    let now = now_iso();
    let retention_days = i64::from(cx.config().request_log_retention_days);
    let cutoff = iso_from_ms(cx.now_ms() - retention_days * 86_400_000);

    let counts = RetentionCounts {
        request_logs: sweep_request_logs(cx.pool(), &cutoff, REQUEST_LOG_BATCH_SIZE, MAX_BATCHES_PER_RUN).await?,
        sessions: delete_expired(cx.pool(), "sessions", &now).await?,
        oauth_states: delete_expired(cx.pool(), "oauth_login_states", &now).await?,
        cli_login_requests: delete_expired(cx.pool(), "cli_login_requests", &now).await?,
    };
    tracing::info!(
        request_logs = counts.request_logs,
        sessions = counts.sessions,
        oauth_login_states = counts.oauth_states,
        cli_login_requests = counts.cli_login_requests,
        "retention sweep"
    );
    Ok(counts)
}

/// The next 03:17 UTC strictly after `now`.
pub fn next_sweep_at(now: OffsetDateTime) -> OffsetDateTime {
    let at = time::Time::from_hms(SWEEP_HOUR, SWEEP_MINUTE, 0).expect("a valid time of day");
    let today = now.replace_time(at);
    if today > now {
        today
    } else {
        today + TimeDuration::days(1)
    }
}

/// The core sweep with the edition's own chained after it. A storage failure is
/// logged, never propagated — the Worker's `scheduled` handler caught each step the
/// same way, so one bad sweep never skips the rest of the run.
async fn sweep_with_extra(cx: &AppState, extra: Option<&ExtraSweep>) {
    match run_retention(cx).await {
        Ok(_) => {}
        Err(error) => tracing::error!(error = %error, "retention sweep failed"),
    }
    if let Some(extra) = extra {
        extra(cx.clone()).await;
    }
}

/// One scheduled run: retention (plus the edition's), then the price table refresh
/// — the two jobs the Worker's cron trigger drove.
async fn run_scheduled(cx: &AppState, extra: Option<&ExtraSweep>) {
    sweep_with_extra(cx, extra).await;
    crate::pricing::litellm::ensure_fresh_price_table(cx).await;
}

/// Replaces the Wrangler cron: sleeps until the next 03:17 UTC, sweeps, repeats.
/// The handle is the caller's to abort at shutdown; dropping it leaves the task
/// running for the process's lifetime, which is what a server wants.
pub fn spawn_scheduler(cx: AppState, extra: Option<ExtraSweep>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let now = OffsetDateTime::now_utc();
            let wait = next_sweep_at(now) - now;
            // A non-positive or absurd duration can only come from a clock jump;
            // a minute's wait re-reads the clock instead of spinning.
            let wait = std::time::Duration::try_from(wait).unwrap_or(std::time::Duration::from_secs(60));
            tokio::time::sleep(wait).await;
            run_scheduled(&cx, extra.as_ref()).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use time::macros::datetime;

    use super::*;
    use crate::db::test_support::{insert_user, skip_without_db, test_config, test_pool, test_state};
    use crate::ids::new_id;
    use crate::upstream::MockTransport;

    async fn seed_log(pool: &PgPool, created_at: &str) -> String {
        let id = new_id("log");
        sqlx::query(
            "INSERT INTO request_logs (id, user_id, api_key_id, provider, model, account_id, status_code,
                                       latency_ms, error_code, created_at)
             VALUES ($1, 'user_1', NULL, 'claude-code', 'claude-code/claude-opus-5', NULL, 200, 100, NULL, $2)",
        )
        .bind(&id)
        .bind(created_at)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    async fn log_ids(pool: &PgPool) -> Vec<String> {
        let mut ids: Vec<String> = sqlx::query_scalar("SELECT id FROM request_logs").fetch_all(pool).await.unwrap();
        ids.sort();
        ids
    }

    fn state_with_retention(pool: PgPool, days: u32) -> AppState {
        let mut config = test_config();
        config.request_log_retention_days = days;
        AppState::builder(config, pool).transport(MockTransport::new()).build()
    }

    /// The cutoff is strict (`created_at < cutoff`): a row exactly at it is kept, one
    /// a millisecond older is deleted, and a recent row is untouched. Pinned to an
    /// explicit cutoff — the sweep reads its own clock, so a window computed here
    /// would already have moved by the time the DELETE runs.
    #[tokio::test]
    async fn the_request_log_cutoff_boundary_is_strict() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let cutoff_ms = 1_785_931_200_000i64; // 2026-08-02T12:00:00.000Z minus 90 days
        let cutoff = iso_from_ms(cutoff_ms);
        let at_cutoff = seed_log(&pool, &cutoff).await;
        let just_older = seed_log(&pool, &iso_from_ms(cutoff_ms - 1)).await;
        let recent = seed_log(&pool, &iso_from_ms(cutoff_ms + 89 * 86_400_000)).await;

        let deleted = sweep_request_logs(&pool, &cutoff, REQUEST_LOG_BATCH_SIZE, MAX_BATCHES_PER_RUN).await.unwrap();
        assert_eq!(deleted, 1);
        let mut kept = vec![at_cutoff, recent];
        kept.sort();
        assert_eq!(log_ids(&pool).await, kept);
        assert!(!log_ids(&pool).await.contains(&just_older));
    }

    #[tokio::test]
    async fn batching_drains_every_old_row() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let cutoff_ms = 1_767_225_600_000i64; // 2026-01-01T00:00:00.000Z
        let cutoff = iso_from_ms(cutoff_ms);
        for i in 1..=7 {
            seed_log(&pool, &iso_from_ms(cutoff_ms - i * 1000)).await;
        }
        let kept = seed_log(&pool, &iso_from_ms(cutoff_ms + 86_400_000)).await;

        // batch_size 3 over 7 old rows: batches of 3, 3, 1 (terminating on the
        // non-full batch), well under the cap.
        let deleted = sweep_request_logs(&pool, &cutoff, 3, 40).await.unwrap();
        assert_eq!(deleted, 7);
        assert_eq!(log_ids(&pool).await, vec![kept]);
    }

    #[tokio::test]
    async fn the_batch_cap_bounds_a_single_run() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let cutoff_ms = 1_767_225_600_000i64;
        let cutoff = iso_from_ms(cutoff_ms);
        for i in 1..=10 {
            seed_log(&pool, &iso_from_ms(cutoff_ms - i * 1000)).await;
        }
        // Every batch of 2 comes back full, so the loop would never terminate on
        // its own; the cap stops it at exactly 3 × 2 deletions.
        let deleted = sweep_request_logs(&pool, &cutoff, 2, 3).await.unwrap();
        assert_eq!(deleted, 6);
        assert_eq!(log_ids(&pool).await.len(), 4);
    }

    #[tokio::test]
    async fn the_default_batch_size_clears_a_small_backlog_in_one_pass() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let cutoff_ms = 1_767_225_600_000i64;
        let cutoff = iso_from_ms(cutoff_ms);
        const { assert!(REQUEST_LOG_BATCH_SIZE > 5, "this seed must fit in a single default batch") };
        for i in 1..=5 {
            seed_log(&pool, &iso_from_ms(cutoff_ms - i * 1000)).await;
        }
        let deleted = sweep_request_logs(&pool, &cutoff, REQUEST_LOG_BATCH_SIZE, MAX_BATCHES_PER_RUN).await.unwrap();
        assert_eq!(deleted, 5);
        assert!(log_ids(&pool).await.is_empty());
    }

    /// `REQUEST_LOG_RETENTION_DAYS` drives the cutoff; `config.rs` is what turns an
    /// invalid value into the 90-day default before it ever reaches here.
    #[tokio::test]
    async fn the_retention_day_count_moves_the_cutoff() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = state_with_retention(pool, 30);
        let just_over = seed_log(state.pool(), &iso_from_ms(state.now_ms() - 31 * 86_400_000)).await;
        let under = seed_log(state.pool(), &iso_from_ms(state.now_ms() - 25 * 86_400_000)).await;

        let counts = run_retention(&state).await.unwrap();
        assert_eq!(counts.request_logs, 1);
        assert_eq!(log_ids(state.pool()).await, vec![under]);
        assert!(!log_ids(state.pool()).await.contains(&just_over));
    }

    #[tokio::test]
    async fn the_default_window_keeps_thirty_five_days_and_drops_ninety_five() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        assert_eq!(state.config().request_log_retention_days, 90, "the documented default");
        let inside = seed_log(state.pool(), &iso_from_ms(state.now_ms() - 35 * 86_400_000)).await;
        seed_log(state.pool(), &iso_from_ms(state.now_ms() - 95 * 86_400_000)).await;

        assert_eq!(run_retention(&state).await.unwrap().request_logs, 1);
        assert_eq!(log_ids(state.pool()).await, vec![inside]);
    }

    #[tokio::test]
    async fn expired_sessions_states_and_login_requests_go_together() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "sweep@example.com").await;
        let now = state.now_ms();
        for (id, expires) in [("sess_expired", now - 60_000), ("sess_active", now + 60_000)] {
            sqlx::query("INSERT INTO sessions (id, user_id, expires_at, created_at) VALUES ($1, $2, $3, $4)")
                .bind(id)
                .bind(&user.id)
                .bind(iso_from_ms(expires))
                .bind(now_iso())
                .execute(state.pool())
                .await
                .unwrap();
        }
        for (id, expires) in [("state_expired", now - 60_000), ("state_active", now + 60_000)] {
            sqlx::query(
                "INSERT INTO oauth_login_states (id, kind, user_id, provider, payload_json, expires_at, created_at)
                 VALUES ($1, 'provider', $2, 'claude-code', '{}', $3, $4)",
            )
            .bind(id)
            .bind(&user.id)
            .bind(iso_from_ms(expires))
            .bind(now_iso())
            .execute(state.pool())
            .await
            .unwrap();
        }
        for (id, expires) in [("clireq_expired", now - 1000), ("clireq_active", now + 60_000)] {
            sqlx::query(
                "INSERT INTO cli_login_requests (id, device_name, ip_hash, expires_at, attempts, created_at)
                 VALUES ($1, 'box', 'hash', $2, 0, $3)",
            )
            .bind(id)
            .bind(iso_from_ms(expires))
            .bind(now_iso())
            .execute(state.pool())
            .await
            .unwrap();
        }
        seed_log(state.pool(), &iso_from_ms(now - 91 * 86_400_000)).await;

        let counts = run_retention(&state).await.unwrap();
        assert_eq!(
            counts,
            RetentionCounts { request_logs: 1, sessions: 1, oauth_states: 1, cli_login_requests: 1 },
            "all four counts come back together"
        );

        for (table, kept) in [
            ("sessions", "sess_active"),
            ("oauth_login_states", "state_active"),
            ("cli_login_requests", "clireq_active"),
        ] {
            let ids: Vec<String> = sqlx::query_scalar(&format!("SELECT id FROM {table}"))
                .fetch_all(state.pool())
                .await
                .unwrap();
            assert_eq!(ids, vec![kept.to_string()], "{table}");
        }
    }

    /// An empty database sweeps to zeros rather than erroring.
    #[tokio::test]
    async fn a_clean_database_sweeps_to_zero() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        assert_eq!(run_retention(&state).await.unwrap(), RetentionCounts::default());
    }

    #[test]
    fn the_schedule_is_the_next_three_seventeen_utc() {
        // Before the hour on the same day.
        assert_eq!(next_sweep_at(datetime!(2026-09-15 00:00:00 UTC)), datetime!(2026-09-15 03:17:00 UTC));
        // After it, the next day.
        assert_eq!(next_sweep_at(datetime!(2026-09-15 03:17:01 UTC)), datetime!(2026-09-16 03:17:00 UTC));
        // Exactly at it counts as done for today.
        assert_eq!(next_sweep_at(datetime!(2026-09-15 03:17:00 UTC)), datetime!(2026-09-16 03:17:00 UTC));
        // Month and year boundaries roll over.
        assert_eq!(next_sweep_at(datetime!(2026-12-31 23:59:59 UTC)), datetime!(2027-01-01 03:17:00 UTC));
        assert!(next_sweep_at(OffsetDateTime::now_utc()) > OffsetDateTime::now_utc());
    }

    /// The edition hook runs after the core sweep, inside the same scheduled run.
    #[tokio::test]
    async fn the_extra_sweep_hook_is_chained_after_the_core_sweep() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        seed_log(state.pool(), &iso_from_ms(state.now_ms() - 200 * 86_400_000)).await;

        static CALLS: AtomicUsize = AtomicUsize::new(0);
        let extra: ExtraSweep = Arc::new(|cx: AppState| {
            Box::pin(async move {
                // The core sweep has already committed by the time the hook runs.
                let remaining: i64 =
                    sqlx::query_scalar("SELECT COUNT(*) FROM request_logs").fetch_one(cx.pool()).await.unwrap();
                assert_eq!(remaining, 0);
                CALLS.fetch_add(1, Ordering::SeqCst);
            })
        });

        sweep_with_extra(&state, Some(&extra)).await;
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
        // No hook is just as valid.
        sweep_with_extra(&state, None).await;
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
    }
}
