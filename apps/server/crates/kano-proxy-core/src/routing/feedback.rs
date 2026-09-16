//! Routing penalties (docs/providers.md § Routing module "Penalties").
//!
//! What a failed upstream response costs the account, decided in one place:
//!
//! | Upstream outcome | Penalty |
//! |---|---|
//! | 401/403 (auth), 402 (billing) | bench 300s |
//! | 429 (rate limit) | bench until the upstream reset when derivable — reset headers, else the earliest exhausted window's `resets_at`, else 300s; capped at 7 days |
//! | 520/522/524 (upstream edge failed/timed out before first byte) | request-local exclusion; the third fresh strike benches 30s |
//! | anything else non-2xx | no bench — passthrough / in-stream error, unchanged |
//!
//! Bench outcomes and edge-timeout exclusions both continue to the next candidate; dispatch
//! owns the walk and strike persistence.

use http::HeaderMap;
use serde_json::Value;

use crate::db::accounts::{parse_iso_ms, read_usage_snapshot, AccountRow};

const DEFAULT_COOLDOWN_MS: i64 = 300_000;
pub const EDGE_TIMEOUT_COOLDOWN_MS: i64 = 30_000;
const MAX_COOLDOWN_MS: i64 = 7 * 24 * 60 * 60 * 1000;

const AUTH_BILLING_STATUSES: [u16; 3] = [401, 402, 403];
const EDGE_TIMEOUT_STATUSES: [u16; 3] = [520, 522, 524];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Penalty {
    pub cooldown_ms: i64,
}

/// 520/522/524 always fail over, but only record a persistent strike.
pub fn is_edge_timeout_status(status: u16) -> bool {
    EDGE_TIMEOUT_STATUSES.contains(&status)
}

fn clamp_cooldown(ms: i64) -> i64 {
    ms.clamp(0, MAX_COOLDOWN_MS)
}

/// Latest reset among any `anthropic-ratelimit-*-reset` response header (RFC 3339 timestamps)
/// — taking the latest, not the first found, so a multi-dimension 429 (requests vs. tokens,
/// each with its own reset) is not retried before every dimension has actually cleared.
fn rate_limit_reset_header_ms(headers: &HeaderMap) -> Option<i64> {
    let mut latest: Option<i64> = None;
    for (name, value) in headers {
        let key = name.as_str().to_ascii_lowercase();
        if !(key.starts_with("anthropic-ratelimit-") && key.ends_with("-reset")) {
            continue;
        }
        let Some(at) = value.to_str().ok().and_then(parse_iso_ms) else { continue };
        if latest.is_none_or(|l| at > l) {
            latest = Some(at);
        }
    }
    latest
}

/// Earliest `resets_at` among the account's currently-exhausted (`utilization >= 100`)
/// windows, per docs/providers.md § Routing module "Penalties".
fn earliest_exhausted_window_reset_ms(account: &AccountRow, now_ms: i64) -> Option<i64> {
    let snapshot = read_usage_snapshot(account)?;
    let mut earliest: Option<i64> = None;
    for window in &snapshot.windows {
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
        if earliest.is_none_or(|e| at < e) {
            earliest = Some(at);
        }
    }
    earliest
}

/// Proxy-internal reset hint (epoch ms), for adapters whose upstream states the reset in the
/// **body** rather than a header — antigravity's 429 carries a `RetryInfo` detail, and only its
/// adapter can classify quota exhaustion apart from a transient throttle (docs/providers.md
/// § Antigravity). Set by an adapter on the response it hands back; no upstream ever sends it,
/// so this cannot change what the other providers already do.
pub const RATELIMIT_RESET_HINT_HEADER: &str = "x-kano-ratelimit-reset";

fn reset_hint_header_ms(headers: &HeaderMap) -> Option<i64> {
    headers.get(RATELIMIT_RESET_HINT_HEADER)?.to_str().ok()?.trim().parse::<f64>().ok().filter(|v| v.is_finite()).map(|v| v as i64)
}

fn rate_limit_cooldown_ms(headers: &HeaderMap, account: &AccountRow, now_ms: i64) -> i64 {
    if let Some(hint) = reset_hint_header_ms(headers) {
        return clamp_cooldown(hint - now_ms);
    }
    if let Some(header) = rate_limit_reset_header_ms(headers) {
        return clamp_cooldown(header - now_ms);
    }
    if let Some(snapshot) = earliest_exhausted_window_reset_ms(account, now_ms) {
        return clamp_cooldown(snapshot - now_ms);
    }
    DEFAULT_COOLDOWN_MS
}

/// The penalty for one upstream attempt's outcome, or `None` when the outcome is not a
/// bench-and-try-next-candidate status — that status passes through / in-stream errors exactly
/// as before, untouched by the routing module.
pub fn penalty_for_outcome(status: u16, headers: &HeaderMap, account: &AccountRow, now_ms: i64) -> Option<Penalty> {
    if AUTH_BILLING_STATUSES.contains(&status) {
        return Some(Penalty { cooldown_ms: DEFAULT_COOLDOWN_MS });
    }
    if status == 429 {
        return Some(Penalty { cooldown_ms: rate_limit_cooldown_ms(headers, account, now_ms) });
    }
    None
}

/// Agent-tunnel fault classification (docs/cli.md § Failover semantics): a response carrying
/// `x-agent-fault` never reached the local server, so it must degrade the route, never account
/// state — every fault fails over; only `offline` benches the provider's internal account row,
/// for 60s, so a group under traffic is not paying a tunnel round-trip per request while a
/// laptop is closed. A response marked `x-agent-upstream` is a real local answer and takes the
/// normal penalty table above.
pub const AGENT_FAULT_HEADER: &str = "x-agent-fault";
pub const AGENT_UPSTREAM_MARKER_HEADER: &str = "x-agent-upstream";
pub const AGENT_OFFLINE_COOLDOWN_MS: i64 = 60_000;

/// Present only for a fault; `failover` is implicit (every verdict fails over).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFaultVerdict {
    pub bench_ms: Option<i64>,
    pub reason: String,
}

pub fn agent_fault_verdict(headers: &HeaderMap) -> Option<AgentFaultVerdict> {
    if headers.contains_key(AGENT_UPSTREAM_MARKER_HEADER) {
        return None;
    }
    let reason = headers.get(AGENT_FAULT_HEADER)?.to_str().ok().filter(|s| !s.is_empty())?.to_string();
    let bench_ms = (reason == "offline").then_some(AGENT_OFFLINE_COOLDOWN_MS);
    Some(AgentFaultVerdict { bench_ms, reason })
}

/// Same status set [`penalty_for_outcome`] benches on — kept for call sites that only need the
/// yes/no check.
pub fn is_bench_status(status: u16) -> bool {
    AUTH_BILLING_STATUSES.contains(&status) || status == 429
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::candidates::tests_support::account_row;
    use serde_json::json;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.insert(
                http::HeaderName::from_bytes(k.as_bytes()).expect("header name"),
                http::HeaderValue::from_str(v).expect("header value"),
            );
        }
        map
    }

    fn now() -> i64 {
        parse_iso_ms("2026-06-01T00:00:00.000Z").expect("timestamp")
    }

    #[test]
    fn edge_timeout_statuses() {
        for status in [520u16, 522, 524] {
            assert!(is_edge_timeout_status(status));
            assert!(penalty_for_outcome(status, &HeaderMap::new(), &account_row("a", "grok"), now()).is_none());
        }
        assert!(!is_edge_timeout_status(500));
        assert!(!is_edge_timeout_status(429));
    }

    #[test]
    fn auth_and_billing_statuses_bench_five_minutes() {
        for status in [401u16, 402, 403] {
            assert!(is_bench_status(status));
            assert_eq!(
                penalty_for_outcome(status, &HeaderMap::new(), &account_row("a", "grok"), now()),
                Some(Penalty { cooldown_ms: 300_000 })
            );
        }
    }

    #[test]
    fn a_plain_429_falls_back_to_the_default_cooldown() {
        assert_eq!(
            penalty_for_outcome(429, &HeaderMap::new(), &account_row("a", "grok"), now()),
            Some(Penalty { cooldown_ms: 300_000 })
        );
    }

    #[test]
    fn a_429_uses_the_latest_anthropic_reset_header() {
        let h = headers(&[
            ("anthropic-ratelimit-requests-reset", "2026-06-01T00:01:00.000Z"),
            ("anthropic-ratelimit-tokens-reset", "2026-06-01T00:05:00.000Z"),
        ]);
        assert_eq!(
            penalty_for_outcome(429, &h, &account_row("a", "grok"), now()),
            Some(Penalty { cooldown_ms: 300_000 })
        );
    }

    #[test]
    fn the_proxy_internal_hint_header_wins_over_everything_else() {
        let h = headers(&[
            (RATELIMIT_RESET_HINT_HEADER, &(now() + 30_000).to_string()),
            ("anthropic-ratelimit-tokens-reset", "2026-06-01T05:00:00.000Z"),
        ]);
        assert_eq!(
            penalty_for_outcome(429, &h, &account_row("a", "grok"), now()),
            Some(Penalty { cooldown_ms: 30_000 })
        );
    }

    #[test]
    fn a_429_falls_back_to_the_earliest_exhausted_window_reset() {
        let mut row = account_row("a", "claude-code");
        row.usage_snapshot_json = Some(
            json!({
                "windows": [
                    { "label": "5h", "utilization": 100, "resets_at": "2026-06-01T02:00:00.000Z" },
                    { "label": "Week", "utilization": 100, "resets_at": "2026-06-03T00:00:00.000Z" },
                    { "label": "Other", "utilization": 10, "resets_at": "2026-06-01T00:30:00.000Z" },
                ],
                "error": null, "stale": false, "edgeBlocked": false
            })
            .to_string(),
        );
        assert_eq!(
            penalty_for_outcome(429, &HeaderMap::new(), &row, now()),
            Some(Penalty { cooldown_ms: 2 * 60 * 60 * 1000 })
        );
    }

    #[test]
    fn cooldowns_clamp_to_zero_and_seven_days() {
        let past = headers(&[(RATELIMIT_RESET_HINT_HEADER, &(now() - 10_000).to_string())]);
        assert_eq!(
            penalty_for_outcome(429, &past, &account_row("a", "grok"), now()),
            Some(Penalty { cooldown_ms: 0 })
        );
        let far = headers(&[(RATELIMIT_RESET_HINT_HEADER, &(now() + 30 * 24 * 3_600_000).to_string())]);
        assert_eq!(
            penalty_for_outcome(429, &far, &account_row("a", "grok"), now()),
            Some(Penalty { cooldown_ms: MAX_COOLDOWN_MS })
        );
    }

    #[test]
    fn a_non_bench_status_earns_no_penalty() {
        for status in [200u16, 400, 404, 500, 529] {
            assert!(penalty_for_outcome(status, &HeaderMap::new(), &account_row("a", "grok"), now()).is_none());
            assert!(!is_bench_status(status));
        }
    }

    #[test]
    fn agent_faults_fail_over_and_only_offline_benches() {
        assert_eq!(agent_fault_verdict(&HeaderMap::new()), None);
        assert_eq!(
            agent_fault_verdict(&headers(&[(AGENT_FAULT_HEADER, "offline")])),
            Some(AgentFaultVerdict { bench_ms: Some(AGENT_OFFLINE_COOLDOWN_MS), reason: "offline".into() })
        );
        assert_eq!(
            agent_fault_verdict(&headers(&[(AGENT_FAULT_HEADER, "timeout")])),
            Some(AgentFaultVerdict { bench_ms: None, reason: "timeout".into() })
        );
        // A real local answer is never a fault, whatever else the headers say.
        assert_eq!(
            agent_fault_verdict(&headers(&[(AGENT_UPSTREAM_MARKER_HEADER, "1"), (AGENT_FAULT_HEADER, "offline")])),
            None
        );
    }
}
