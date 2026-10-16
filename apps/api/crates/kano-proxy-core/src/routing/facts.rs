//! Routing facts (docs/providers.md § Routing module "Facts").
//!
//! Per-candidate usability, computed from stored state ONLY. Dispatch never makes a
//! synchronous upstream call to learn these; the two sources are:
//!
//! - Bench state from the already-loaded account row.
//! - Usage windows from the row's stored `usage_snapshot_json`: any window with
//!   `utilization >= 100` marks the candidate unusable until that window's `resets_at`.
//!   Precision is bounded by snapshot freshness, but the skip self-expires at `resets_at`
//!   even off a stale snapshot.
//!
//! A malformed or missing snapshot reads as "no window fact" (fail open).
//!
//! Both are **quota-scoped** (docs/providers.md § Routing module "Quota scopes"): a window
//! tagged with a scope, and a bench on a scope, apply only when the candidate's model draws on
//! that scope. A caller with no model to scope by (`scope: None`, the Providers dot) sees only
//! the account-wide ones.
//!
//! Deviation from the TypeScript: these functions are synchronous and take no `Env`/`userId`.
//! The originals were `async` and ignored both (`void env; void userId`), and nothing in the
//! Rust port needs an await here — every fact comes off the loaded row.

use serde_json::Value;

use crate::db::accounts::{parse_iso_ms, read_usage_snapshot, AccountRow};
use crate::pool::bench::{bench_until_from_row, scope_bench_until_from_row};
use crate::routing::types::{CandidateFacts, RoutingCandidate};

/// The latest `resets_at` among the account's currently-exhausted windows
/// (`utilization >= 100` AND `resets_at` still in the future) — the candidate stays unusable
/// until every exhausted window has cleared, not just the first one. `None` when no window is
/// currently exhausted (never fetched, none over 100%, or every over-100% window's `resets_at`
/// already passed — the self-expiry the doc requires even off a stale snapshot).
pub fn usage_window_unusable_until(row: &AccountRow, scope: Option<&str>, now_ms: i64) -> Option<i64> {
    let snapshot = read_usage_snapshot(row)?;
    windows_unusable_until(&snapshot.windows, scope, now_ms)
}

/// Whether a stored window gates a candidate drawing on `scope`: an untagged window gates
/// everything, a tagged one only its own scope.
pub fn window_applies(window: &Value, scope: Option<&str>) -> bool {
    match window.get("scope").and_then(Value::as_str) {
        None => true,
        Some(tag) => scope == Some(tag),
    }
}

/// Same rule against a windows array the caller already holds — the admin accounts route
/// derives its status dot from the snapshot it is about to return, which on a refresh is newer
/// than the one on the loaded row (docs/admin-ui.md § Providers page).
pub fn windows_unusable_until(windows: &[Value], scope: Option<&str>, now_ms: i64) -> Option<i64> {
    let mut latest: Option<i64> = None;
    for window in windows.iter().filter(|w| window_applies(w, scope)) {
        let Some(utilization) = window.get("utilization").and_then(Value::as_f64) else { continue };
        if utilization < 100.0 {
            continue;
        }
        let Some(resets_at) = window.get("resets_at").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            continue;
        };
        let Some(at) = parse_iso_ms(resets_at) else { continue };
        if at <= now_ms {
            continue;
        }
        if latest.is_none_or(|l| at > l) {
            latest = Some(at);
        }
    }
    latest
}

/// Bench-until and usage-window facts for one candidate.
pub fn candidate_facts(candidate: &RoutingCandidate, now_ms: i64) -> CandidateFacts {
    facts_for_row(&candidate.account, candidate.adapter.quota_scope(&candidate.upstream_model), now_ms)
}

/// The same rule straight off a row — what `candidate_facts` computes, for callers that hold
/// the account row rather than a candidate.
pub fn facts_for_row(row: &AccountRow, scope: Option<&str>, now_ms: i64) -> CandidateFacts {
    let account_bench = bench_until_from_row(row, now_ms);
    let scope_bench = scope.and_then(|s| scope_bench_until_from_row(row, s, now_ms));
    let bench_until = account_bench.max(scope_bench);
    let usage_window_until = usage_window_unusable_until(row, scope, now_ms);
    let unusable_until = match (bench_until, usage_window_until) {
        (None, w) => w,
        (b, None) => b,
        (Some(b), Some(w)) => Some(b.max(w)),
    };
    CandidateFacts { usable: unusable_until.is_none(), unusable_until, bench_until, usage_window_until }
}

/// Facts for a whole candidate list, in the same order, from the loaded rows.
pub fn candidate_facts_list(candidates: &[RoutingCandidate], now_ms: i64) -> Vec<CandidateFacts> {
    candidates.iter().map(|c| candidate_facts(c, now_ms)).collect()
}

/// Earliest `unusable_until` across a set of facts, or `None` when none carry one — used for
/// `Retry-After`.
pub fn earliest_unusable_until(facts: &[CandidateFacts]) -> Option<i64> {
    facts.iter().filter_map(|f| f.unusable_until).min()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::accounts::iso_from_ms;
    use serde_json::json;

    /// A bare `upstream_accounts` row; tests set only the fields they assert on.
    pub(crate) fn account_row(id: &str, provider: &str) -> AccountRow {
        AccountRow {
            id: id.into(),
            user_id: "user_1".into(),
            provider: provider.into(),
            external_account_id: None,
            label: None,
            custom_label: None,
            priority: 1,
            encrypted_payload: "encrypted".into(),
            account_meta_json: None,
            usage_snapshot_json: None,
            usage_fetched_at: None,
            usage_fetching_at: None,
            bench_until: None,
            bench_reason: None,
            bench_scopes_json: None,
            refreshing_at: None,
            edge_strikes: 0,
            edge_strike_at: None,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
        }
    }

    struct NoopAdapter(String);

    #[async_trait::async_trait]
    impl crate::providers::ProviderAdapter for NoopAdapter {
        fn id(&self) -> &str {
            &self.0
        }
        async fn chat_completions(
            &self,
            _: &crate::AppState,
            _: &crate::pool::AcquiredAccount,
            _: &crate::providers::ChatCompletionRequest,
            _: &crate::providers::CallExtras,
        ) -> Result<axum::response::Response, crate::providers::AdapterError> {
            Err(crate::providers::AdapterError::Unsupported("chat_completions"))
        }
    }

    fn test_candidate(account: AccountRow) -> RoutingCandidate {
        RoutingCandidate {
            target_index: 0,
            pinned: false,
            provider: account.provider.clone(),
            upstream_model: "claude-opus-5".into(),
            is_builtin: true,
            custom_provider: None,
            adapter: std::sync::Arc::new(NoopAdapter(account.provider.clone())),
            account,
            share: None,
        }
    }

    fn snapshot_json(windows: Value) -> String {
        json!({ "windows": windows, "error": null, "stale": false, "edgeBlocked": false }).to_string()
    }

    fn at(iso: &str) -> i64 {
        parse_iso_ms(iso).expect("test timestamp parses")
    }

    #[test]
    fn a_window_at_exactly_100_percent_with_a_future_reset_marks_unusable_until_that_time() {
        let mut row = account_row("acc_1", "claude-code");
        row.usage_snapshot_json =
            Some(snapshot_json(json!([{ "label": "5h", "utilization": 100, "resets_at": "2026-06-01T05:00:00.000Z" }])));
        assert_eq!(
            usage_window_unusable_until(&row, None, at("2026-06-01T00:00:00.000Z")),
            Some(at("2026-06-01T05:00:00.000Z"))
        );
    }

    #[test]
    fn a_window_under_100_percent_is_not_exhausted() {
        let mut row = account_row("acc_1", "claude-code");
        row.usage_snapshot_json = Some(snapshot_json(
            json!([{ "label": "5h", "utilization": 99.9, "resets_at": "2026-06-01T05:00:00.000Z" }]),
        ));
        assert_eq!(usage_window_unusable_until(&row, None, at("2026-06-01T00:00:00.000Z")), None);
    }

    #[test]
    fn a_past_reset_self_expires_even_off_a_stale_snapshot() {
        let mut row = account_row("acc_1", "claude-code");
        row.usage_snapshot_json =
            Some(snapshot_json(json!([{ "label": "5h", "utilization": 100, "resets_at": "2026-06-01T00:00:00.000Z" }])));
        assert_eq!(usage_window_unusable_until(&row, None, at("2026-06-01T05:00:00.000Z")), None);
    }

    #[test]
    fn a_malformed_or_missing_snapshot_reads_as_no_window_fact() {
        let mut row = account_row("acc_1", "claude-code");
        assert_eq!(usage_window_unusable_until(&row, None, 0), None);
        row.usage_snapshot_json = Some("not json".into());
        assert_eq!(usage_window_unusable_until(&row, None, 0), None);
        row.usage_snapshot_json = Some(json!({ "notWindows": [] }).to_string());
        assert_eq!(usage_window_unusable_until(&row, None, 0), None);
    }

    #[test]
    fn multiple_exhausted_windows_hold_until_the_latest_reset() {
        let mut row = account_row("acc_1", "claude-code");
        row.usage_snapshot_json = Some(snapshot_json(json!([
            { "label": "5h", "utilization": 100, "resets_at": "2026-06-01T01:00:00.000Z" },
            { "label": "Week", "utilization": 100, "resets_at": "2026-06-05T00:00:00.000Z" },
        ])));
        assert_eq!(
            usage_window_unusable_until(&row, None, at("2026-06-01T00:00:00.000Z")),
            Some(at("2026-06-05T00:00:00.000Z"))
        );
    }

    #[test]
    fn a_window_missing_resets_at_is_ignored_even_at_100_percent() {
        let mut row = account_row("acc_1", "claude-code");
        row.usage_snapshot_json =
            Some(snapshot_json(json!([{ "label": "5h", "utilization": 100, "resets_at": null }])));
        assert_eq!(usage_window_unusable_until(&row, None, at("2026-06-01T00:00:00.000Z")), None);
    }

    #[test]
    fn neither_benched_nor_limited_is_usable() {
        let facts = candidate_facts(&test_candidate(account_row("acc_1", "claude-code")), 1_000);
        assert_eq!(
            facts,
            CandidateFacts { usable: true, unusable_until: None, bench_until: None, usage_window_until: None }
        );
    }

    #[test]
    fn benched_only_is_unusable_until_the_rows_bench_expiry() {
        let now = at("2026-01-01T00:00:00.000Z");
        let mut row = account_row("acc_1", "claude-code");
        row.bench_until = Some(iso_from_ms(now + 300_000));
        let facts = candidate_facts(&test_candidate(row), now);
        assert!(!facts.usable);
        assert_eq!(facts.unusable_until, Some(now + 300_000));
    }

    #[test]
    fn limited_only_is_unusable_until_the_window_resets() {
        let now = at("2026-06-01T00:00:00.000Z");
        let mut row = account_row("acc_1", "claude-code");
        row.usage_snapshot_json =
            Some(snapshot_json(json!([{ "label": "5h", "utilization": 100, "resets_at": "2026-06-01T05:00:00.000Z" }])));
        let facts = candidate_facts(&test_candidate(row), now);
        assert_eq!(
            facts,
            CandidateFacts {
                usable: false,
                unusable_until: Some(at("2026-06-01T05:00:00.000Z")),
                bench_until: None,
                usage_window_until: Some(at("2026-06-01T05:00:00.000Z")),
            }
        );
    }

    #[test]
    fn benched_and_limited_holds_until_whichever_is_later() {
        let now = at("2026-06-01T00:00:00.000Z");
        let mut row = account_row("acc_1", "claude-code");
        row.bench_until = Some(iso_from_ms(now + 300_000));
        row.usage_snapshot_json =
            Some(snapshot_json(json!([{ "label": "5h", "utilization": 100, "resets_at": "2026-06-01T05:00:00.000Z" }])));
        let facts = candidate_facts(&test_candidate(row), now);
        assert!(!facts.usable);
        assert_eq!(facts.unusable_until, Some(at("2026-06-01T05:00:00.000Z")));
    }

    #[test]
    fn the_earliest_unusable_until_wins() {
        let facts = vec![
            CandidateFacts { usable: true, unusable_until: None, bench_until: None, usage_window_until: None },
            CandidateFacts {
                usable: false,
                unusable_until: Some(500),
                bench_until: Some(500),
                usage_window_until: None,
            },
            CandidateFacts {
                usable: false,
                unusable_until: Some(200),
                bench_until: Some(200),
                usage_window_until: None,
            },
        ];
        assert_eq!(earliest_unusable_until(&facts), Some(200));
        assert_eq!(earliest_unusable_until(&[]), None);
    }

    #[test]
    fn a_scoped_window_gates_only_its_scope_and_the_scopeless_dot_ignores_it() {
        let mut row = account_row("acc_1", "antigravity");
        row.usage_snapshot_json = Some(snapshot_json(json!([
            { "label": "Argon Week", "utilization": 100, "resets_at": "2026-06-05T00:00:00.000Z", "scope": "Argon" },
            { "label": "Gemini Week", "utilization": 40, "resets_at": "2026-06-05T00:00:00.000Z", "scope": "Gemini" },
        ])));
        let now = at("2026-06-01T00:00:00.000Z");
        assert_eq!(usage_window_unusable_until(&row, Some("Argon"), now), Some(at("2026-06-05T00:00:00.000Z")));
        assert_eq!(usage_window_unusable_until(&row, Some("Gemini"), now), None);
        assert_eq!(usage_window_unusable_until(&row, None, now), None);
    }

    #[test]
    fn an_untagged_window_still_gates_every_scope() {
        let mut row = account_row("acc_1", "antigravity");
        row.usage_snapshot_json =
            Some(snapshot_json(json!([{ "label": "5h", "utilization": 100, "resets_at": "2026-06-01T05:00:00.000Z" }])));
        let now = at("2026-06-01T00:00:00.000Z");
        for scope in [Some("Argon"), Some("Gemini"), None] {
            assert_eq!(usage_window_unusable_until(&row, scope, now), Some(at("2026-06-01T05:00:00.000Z")));
        }
    }

    #[test]
    fn a_scoped_bench_makes_only_that_scope_unusable() {
        let mut row = account_row("acc_1", "antigravity");
        row.bench_scopes_json = Some(json!({ "Argon": { "until": "2026-06-02T00:00:00.000Z", "reason": "429" } }).to_string());
        let now = at("2026-06-01T00:00:00.000Z");
        let argon = facts_for_row(&row, Some("Argon"), now);
        assert!(!argon.usable);
        assert_eq!(argon.bench_until, Some(at("2026-06-02T00:00:00.000Z")));
        assert!(facts_for_row(&row, Some("Gemini"), now).usable);
        assert!(facts_for_row(&row, None, now).usable);
    }
}
