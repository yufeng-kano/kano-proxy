//! Port of apps/api/src/logging/request_log.ts (docs/logging.md, docs/database.md
//! § request_logs).
//!
//! One `request_logs` row per client request, written off the response path. Prompts,
//! completions and tokens never reach this module — only counts, a cost estimate and the two
//! name snapshots. Logging must never break the proxy, so every failure here is swallowed
//! after a `tracing` line.

use crate::db::request_logs::{account_label, api_key_name, insert_request_log, RequestLogEntry};
use crate::ids::{iso_ms, now_iso};
use crate::pricing::litellm::{estimate_cost, get_price_table, CostUsage};
use crate::AppState;

/// What a dispatch transport knows when it decides a row. `started_at_ms` is the UTC epoch
/// millisecond stamp taken before dispatch; `None` means "now".
#[derive(Debug, Clone, Default)]
pub struct LogEntry {
    pub started_at_ms: Option<i64>,
    pub user_id: String,
    pub api_key_id: Option<String>,
    pub provider: String,
    pub model: String,
    pub account_id: Option<String>,
    pub status_code: i32,
    pub latency_ms: i64,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
    pub error_code: Option<String>,
    /// Last upstream HTTP response status observed; `None` means no upstream response headers
    /// arrived.
    pub upstream_status: Option<i32>,
    /// The model-group alias this request was addressed to, if any; `model`/`provider` always
    /// store the expanded canonical target.
    pub group_name: Option<String>,
}

/// Writes one row, cost included. Never returns an error: a logging failure is recorded and
/// dropped, exactly as the TypeScript `try { … } catch {}` did.
pub async fn log_request(cx: &AppState, entry: LogEntry) {
    if let Err(error) = write_request_log(cx, entry).await {
        tracing::error!(%error, "Failed to write request log");
    }
}

async fn write_request_log(cx: &AppState, entry: LogEntry) -> anyhow::Result<()> {
    let started_at = match entry.started_at_ms {
        Some(ms) => iso_ms(
            time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000)
                .unwrap_or(time::OffsetDateTime::UNIX_EPOCH),
        ),
        None => now_iso(),
    };

    // Name snapshots (docs/database.md § request_logs): the key's name and the account's
    // display label as they are right now, so a record deleted later still reads by its last
    // name on the Logs page. Two point reads off the request path; a record already gone simply
    // leaves NULL.
    let api_key_name = match entry.api_key_id.as_deref() {
        Some(id) => api_key_name(cx.pool(), id).await.unwrap_or(None),
        None => None,
    };
    let account_label = match entry.account_id.as_deref() {
        Some(id) => account_label(cx.pool(), id).await.unwrap_or(None),
        None => None,
    };

    // Estimated USD at write time (docs/pricing.md). `get_price_table` never fetches — memo and
    // cache only — so a missing table degrades to NULL cost without delaying the write.
    let cost = match get_price_table(cx).await {
        Some(table) => estimate_cost(
            &table,
            &entry.model,
            &CostUsage {
                prompt_tokens: entry.prompt_tokens,
                completion_tokens: entry.completion_tokens,
                cache_read_input_tokens: entry.cache_read_input_tokens,
                cache_creation_input_tokens: entry.cache_creation_input_tokens,
            },
        ),
        None => None,
    };

    insert_request_log(
        cx.pool(),
        &RequestLogEntry {
            user_id: entry.user_id,
            api_key_id: entry.api_key_id,
            provider: entry.provider,
            model: entry.model,
            account_id: entry.account_id,
            status_code: entry.status_code,
            latency_ms: entry.latency_ms,
            prompt_tokens: entry.prompt_tokens,
            completion_tokens: entry.completion_tokens,
            cache_read_input_tokens: entry.cache_read_input_tokens,
            cache_creation_input_tokens: entry.cache_creation_input_tokens,
            cost,
            error_code: entry.error_code,
            upstream_status: entry.upstream_status,
            group_name: entry.group_name,
            api_key_name,
            account_label,
            started_at,
        },
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{insert_account, insert_api_key, insert_user, skip_without_db, test_pool, test_state};
    use crate::pool::StoredCredential;
    use crate::pricing::litellm::{ModelPrice, PriceSource, PriceTable};
    use crate::upstream::MockTransport;

    async fn rows(state: &AppState) -> Vec<crate::db::request_logs::RequestLogRow> {
        sqlx::query_as::<_, crate::db::request_logs::RequestLogRow>(
            "SELECT * FROM request_logs ORDER BY created_at ASC",
        )
        .fetch_all(state.pool())
        .await
        .expect("read logs")
    }

    /// A price table in the cache, so `get_price_table` (memo → cache → None) finds one without
    /// any network.
    async fn seed_price_table(state: &AppState) {
        crate::pricing::litellm::reset_pricing_for_tests();
        let mut table = PriceTable::new();
        table.insert(
            "some-model".into(),
            ModelPrice {
                input: 0.000_01,
                output: 0.000_02,
                cache_read: Some(0.000_001),
                cache_creation: Some(0.000_012_5),
                source: Some(PriceSource::Litellm),
            },
        );
        state
            .cache()
            .put_json(
                crate::pricing::litellm::CACHE_KEY,
                &serde_json::json!({ "table": table, "fetchedAt": crate::app::now_ms() }),
                std::time::Duration::from_secs(600),
            )
            .await;
    }

    #[tokio::test]
    async fn writes_one_row_with_name_snapshots_and_a_cost() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "log@example.com").await;
        let (key, _) = insert_api_key(state.pool(), &user.id).await;
        let account = insert_account(state.pool(), &user.id, "grok", &StoredCredential::default()).await;
        seed_price_table(&state).await;

        log_request(
            &state,
            LogEntry {
                started_at_ms: Some(1_800_000_000_000),
                user_id: user.id.clone(),
                api_key_id: Some(key.id.clone()),
                provider: "grok".into(),
                model: "grok/some-model".into(),
                account_id: Some(account.id.clone()),
                status_code: 200,
                latency_ms: 42,
                prompt_tokens: Some(1_000),
                completion_tokens: Some(100),
                cache_read_input_tokens: Some(0),
                cache_creation_input_tokens: Some(0),
                error_code: None,
                upstream_status: Some(200),
                group_name: Some("team/opus".into()),
            },
        )
        .await;

        let rows = rows(&state).await;
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.provider, "grok");
        assert_eq!(row.model, "grok/some-model");
        assert_eq!(row.account_id.as_deref(), Some(account.id.as_str()));
        assert_eq!(row.status_code, 200);
        assert_eq!(row.latency_ms, 42);
        assert_eq!(row.prompt_tokens, Some(1_000));
        assert_eq!(row.completion_tokens, Some(100));
        assert_eq!(row.error_code, None);
        assert_eq!(row.upstream_status, Some(200));
        assert_eq!(row.group_name.as_deref(), Some("team/opus"));
        assert_eq!(row.api_key_name.as_deref(), Some("test key"));
        assert_eq!(row.account_label.as_deref(), Some("grok test"));
        assert_eq!(row.started_at.as_deref(), Some("2027-01-15T08:00:00.000Z"));
        assert!(row.id.starts_with("log_"));
        // Cost is whatever the loaded price table says for this model — the row must agree
        // with `pricing::litellm`, not with a number hard-coded here. (The price memo is
        // process-wide, so the table is read back rather than assumed.)
        let usage = CostUsage {
            prompt_tokens: Some(1_000),
            completion_tokens: Some(100),
            cache_read_input_tokens: Some(0),
            cache_creation_input_tokens: Some(0),
        };
        let expected = get_price_table(&state).await.and_then(|t| estimate_cost(&t, "grok/some-model", &usage));
        match expected {
            Some(expected) => {
                let cost = row.cost.expect("a priced model costs something");
                assert!((cost - expected).abs() < 1e-9, "{cost} vs {expected}");
                // 1000 uncached input at 1e-5 + 100 output at 2e-5 = 0.012, for the seeded table.
                assert!(cost > 0.0);
            }
            None => assert_eq!(row.cost, None),
        }
    }

    #[tokio::test]
    async fn an_unpriced_model_or_absent_usage_leaves_cost_null() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "nocost@example.com").await;
        seed_price_table(&state).await;

        log_request(
            &state,
            LogEntry {
                user_id: user.id.clone(),
                provider: "grok".into(),
                model: "grok/never-priced".into(),
                status_code: 200,
                latency_ms: 1,
                prompt_tokens: Some(10),
                ..Default::default()
            },
        )
        .await;
        log_request(
            &state,
            LogEntry {
                user_id: user.id.clone(),
                provider: "grok".into(),
                model: "grok/some-model".into(),
                status_code: 200,
                latency_ms: 1,
                ..Default::default()
            },
        )
        .await;

        let rows = rows(&state).await;
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.cost.is_none()));
    }

    #[tokio::test]
    async fn deleted_key_and_account_ids_simply_leave_null_snapshots() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "gone@example.com").await;

        log_request(
            &state,
            LogEntry {
                user_id: user.id.clone(),
                api_key_id: Some("key_gone".into()),
                account_id: Some("acc_gone".into()),
                provider: "grok".into(),
                model: "grok/some-model".into(),
                status_code: 503,
                latency_ms: 5,
                error_code: Some("upstream_unavailable".into()),
                ..Default::default()
            },
        )
        .await;

        let rows = rows(&state).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].api_key_name, None);
        assert_eq!(rows[0].account_label, None);
        assert_eq!(rows[0].error_code.as_deref(), Some("upstream_unavailable"));
    }

    #[tokio::test]
    async fn a_storage_failure_never_propagates() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        // Storage is gone: the insert fails and logging still returns normally.
        state.pool().close().await;
        log_request(
            &state,
            LogEntry {
                user_id: "user_never_existed".into(),
                provider: "grok".into(),
                model: "grok/some-model".into(),
                status_code: 200,
                latency_ms: 1,
                ..Default::default()
            },
        )
        .await;
        // The pool is closed, so even reading back fails — the point is that `log_request`
        // returned at all instead of propagating.
        assert!(state.pool().is_closed());
    }

    #[tokio::test]
    async fn an_absent_started_at_stamps_now() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "now@example.com").await;
        log_request(
            &state,
            LogEntry {
                user_id: user.id.clone(),
                provider: "grok".into(),
                model: "grok/some-model".into(),
                status_code: 200,
                latency_ms: 1,
                ..Default::default()
            },
        )
        .await;
        let rows = rows(&state).await;
        let started = rows[0].started_at.clone().expect("started_at is always written");
        assert!(started.ends_with('Z') && started.len() == 24, "{started}");
    }
}
