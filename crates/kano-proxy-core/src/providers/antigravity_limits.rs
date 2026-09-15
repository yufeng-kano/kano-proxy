//! Antigravity 429 classification (apps/api/src/providers/antigravity_limits.ts,
//! docs/providers.md § Antigravity). Derived from CLIProxyAPI
//! `internal/runtime/executor/antigravity_executor_credits.go` (`decideAntigravity429`) and
//! `helps/json_retry_helpers.go` (`ParseRetryDelay`).
//!
//! The CloudCode backend returns one status — 429 — for two very different situations, and
//! only the body tells them apart:
//!
//! - **Quota exhausted** — the subscription's allowance for that model family is spent.
//!   Retrying in five minutes just burns the account again, so this benches long.
//! - **Rate limited** — a short, transient throttle that carries its own `retryDelay`. This
//!   benches for exactly that delay and fails over.
//!
//! Anything the body does not clearly place in either bucket is left unclassified, and the
//! routing module's ordinary 429 default applies.

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Antigravity429Kind {
    QuotaExhausted,
    RateLimited,
    Unknown,
}

/// `retry_after_ms` is always `Some` for [`Antigravity429Kind::RateLimited`]; the other two
/// kinds carry whatever the body stated, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Antigravity429 {
    pub kind: Antigravity429Kind,
    pub retry_after_ms: Option<i64>,
}

static DURATION_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^(\d+(?:\.\d+)?)s$").expect("valid regex"));
static AFTER_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"after\s+(\d+)s\.?").expect("valid regex"));

/// A protobuf `Duration` as proto-JSON: seconds with an optional fraction and a mandatory `s`
/// suffix (`"17s"`, `"1.5s"`). Anything else is not a duration we are willing to act on.
pub fn parse_proto_duration_ms(value: &Value) -> Option<i64> {
    let text = value.as_str()?;
    let caps = DURATION_RE.captures(text.trim())?;
    let seconds: f64 = caps[1].parse().ok()?;
    if !seconds.is_finite() {
        return None;
    }
    Some((seconds * 1000.0).round() as i64)
}

/// `error.details[]`, object entries only. A heterogeneous details array (null / non-object
/// entries) must classify as `unknown`, not crash the adapter into a generic 502 before
/// dispatch can bench and fail over.
fn error_details(body: &Value) -> Vec<&Value> {
    body.get("error")
        .and_then(|e| e.get("details"))
        .and_then(Value::as_array)
        .map(|details| details.iter().filter(|d| d.is_object()).collect())
        .unwrap_or_default()
}

fn detail_type(detail: &Value) -> &str {
    detail.get("@type").and_then(Value::as_str).unwrap_or("")
}

/// How long upstream says to wait. Google states this three different ways and CLIProxyAPI
/// reads all three in this order: a `RetryInfo` detail's `retryDelay`, then an `ErrorInfo`
/// detail's `metadata.quotaResetDelay`, then an `after <n>s` phrase inside the
/// human-readable message.
pub fn antigravity_retry_delay_ms(body: &Value) -> Option<i64> {
    let details = error_details(body);
    for detail in &details {
        if detail_type(detail) != "type.googleapis.com/google.rpc.RetryInfo" {
            continue;
        }
        if let Some(ms) = parse_proto_duration_ms(detail.get("retryDelay").unwrap_or(&Value::Null)) {
            return Some(ms);
        }
    }
    for detail in &details {
        if detail_type(detail) != "type.googleapis.com/google.rpc.ErrorInfo" {
            continue;
        }
        let reset = detail.get("metadata").and_then(|m| m.get("quotaResetDelay")).unwrap_or(&Value::Null);
        if let Some(ms) = parse_proto_duration_ms(reset) {
            return Some(ms);
        }
    }
    let message = body.get("error").and_then(|e| e.get("message")).and_then(Value::as_str)?;
    let caps = AFTER_RE.captures(message)?;
    caps[1].parse::<i64>().ok().map(|s| s * 1000)
}

/// A `RATE_LIMIT_EXCEEDED` whose delay is at least this long is treated as quota exhaustion
/// rather than a throttle — CLIProxyAPI's own `antigravityShortQuotaCooldownThreshold`.
/// Below it the account recovers on its own soon enough to be worth coming back to.
pub const ANTIGRAVITY_SHORT_COOLDOWN_MS: i64 = 5 * 60_000;

pub fn classify_antigravity_429(body: &Value) -> Antigravity429 {
    let retry_after_ms = antigravity_retry_delay_ms(body);
    let unknown = Antigravity429 { kind: Antigravity429Kind::Unknown, retry_after_ms };
    let status = body.get("error").and_then(|e| e.get("status")).and_then(Value::as_str);
    match status {
        Some(s) if s.eq_ignore_ascii_case("RESOURCE_EXHAUSTED") => {}
        _ => return unknown,
    }

    for detail in error_details(body) {
        if detail_type(detail) != "type.googleapis.com/google.rpc.ErrorInfo" {
            continue;
        }
        let reason = detail.get("reason").and_then(Value::as_str).unwrap_or("").to_ascii_uppercase();
        if reason == "QUOTA_EXHAUSTED" {
            return Antigravity429 { kind: Antigravity429Kind::QuotaExhausted, retry_after_ms };
        }
        if reason == "RATE_LIMIT_EXCEEDED" {
            let Some(ms) = retry_after_ms else { return unknown };
            let kind = if ms >= ANTIGRAVITY_SHORT_COOLDOWN_MS {
                Antigravity429Kind::QuotaExhausted
            } else {
                Antigravity429Kind::RateLimited
            };
            return Antigravity429 { kind, retry_after_ms };
        }
    }

    // No structured reason: fall back to the same keyword sniff CLIProxyAPI does.
    let text = json_text_lowercase(body);
    if text.contains("quota_exhausted") || text.contains("quota exhausted") {
        return Antigravity429 { kind: Antigravity429Kind::QuotaExhausted, retry_after_ms };
    }
    unknown
}

/// `JSON.stringify(body ?? "").toLowerCase()`.
fn json_text_lowercase(body: &Value) -> String {
    let text = if body.is_null() { "\"\"".to_string() } else { body.to_string() };
    text.to_lowercase()
}

/// Quota exhaustion with no upstream reset to go on. Google returns no reset timestamp in
/// that case, and the routing module's 300s default would put the account straight back into
/// rotation to fail again, so the adapter asks for an hour instead. This number is a **chosen
/// heuristic**, not an upstream fact — see docs/providers.md § Antigravity.
pub const ANTIGRAVITY_QUOTA_BENCH_MS: i64 = 60 * 60_000;

/// Epoch-ms this account should stay benched, or `None` to leave the default alone.
pub fn antigravity_bench_until(body: &Value, now: i64) -> Option<i64> {
    let verdict = classify_antigravity_429(body);
    match verdict.kind {
        Antigravity429Kind::QuotaExhausted => {
            Some(now + verdict.retry_after_ms.unwrap_or(ANTIGRAVITY_QUOTA_BENCH_MS))
        }
        // `rate_limited` always carries its delay; the fallback keeps the branch total.
        Antigravity429Kind::RateLimited => Some(now + verdict.retry_after_ms.unwrap_or(0)),
        Antigravity429Kind::Unknown => None,
    }
}

/// "No capacity" is a fleet-side condition, not an account one — CLIProxyAPI retries the
/// other base URL rather than penalising the credential
/// (`antigravityShouldRetryNoCapacity`).
pub fn is_antigravity_no_capacity(status: u16, body: &Value) -> bool {
    if status != 429 && status != 503 {
        return false;
    }
    let text = json_text_lowercase(body);
    text.contains("no capacity") || text.contains("no_capacity")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn resource_exhausted(reason: &str, details: Value) -> Value {
        json!({
            "error": {
                "code": 429,
                "status": "RESOURCE_EXHAUSTED",
                "message": format!("Quota trouble: {reason}"),
                "details": details,
            }
        })
    }

    fn error_info(reason: &str, metadata: Option<Value>) -> Value {
        let mut v = json!({ "@type": "type.googleapis.com/google.rpc.ErrorInfo", "reason": reason });
        if let Some(metadata) = metadata {
            v["metadata"] = metadata;
        }
        v
    }

    fn retry_info(retry_delay: &str) -> Value {
        json!({ "@type": "type.googleapis.com/google.rpc.RetryInfo", "retryDelay": retry_delay })
    }

    // ── parseProtoDurationMs ────────────────────────────────────────────────

    #[test]
    fn reads_whole_and_fractional_second_durations() {
        assert_eq!(parse_proto_duration_ms(&json!("17s")), Some(17_000));
        assert_eq!(parse_proto_duration_ms(&json!("1.5s")), Some(1_500));
    }

    #[test]
    fn refuses_anything_that_is_not_a_proto_duration() {
        assert_eq!(parse_proto_duration_ms(&json!("17")), None);
        assert_eq!(parse_proto_duration_ms(&json!("17ms")), None);
        assert_eq!(parse_proto_duration_ms(&json!(17)), None);
        assert_eq!(parse_proto_duration_ms(&Value::Null), None);
    }

    // ── antigravityRetryDelayMs ─────────────────────────────────────────────

    #[test]
    fn prefers_a_retry_info_detail() {
        let body = resource_exhausted(
            "x",
            json!([error_info("RATE_LIMIT_EXCEEDED", Some(json!({ "quotaResetDelay": "600s" }))), retry_info("12s")]),
        );
        assert_eq!(antigravity_retry_delay_ms(&body), Some(12_000));
    }

    #[test]
    fn falls_back_to_error_info_quota_reset_delay() {
        let body = resource_exhausted(
            "x",
            json!([error_info("RATE_LIMIT_EXCEEDED", Some(json!({ "quotaResetDelay": "600s" })))]),
        );
        assert_eq!(antigravity_retry_delay_ms(&body), Some(600_000));
    }

    #[test]
    fn falls_back_to_an_after_n_seconds_phrase_in_the_message() {
        assert_eq!(
            antigravity_retry_delay_ms(&json!({ "error": { "message": "Try again after 45s." } })),
            Some(45_000)
        );
    }

    #[test]
    fn is_none_when_nothing_states_a_delay() {
        assert_eq!(antigravity_retry_delay_ms(&json!({ "error": { "message": "nope" } })), None);
        assert_eq!(antigravity_retry_delay_ms(&Value::Null), None);
    }

    // ── classifyAntigravity429 ──────────────────────────────────────────────

    #[test]
    fn classifies_an_explicit_quota_exhausted_reason() {
        let body = resource_exhausted("quota", json!([error_info("QUOTA_EXHAUSTED", None)]));
        assert_eq!(
            classify_antigravity_429(&body),
            Antigravity429 { kind: Antigravity429Kind::QuotaExhausted, retry_after_ms: None }
        );
    }

    #[test]
    fn treats_a_short_rate_limit_delay_as_a_transient_throttle() {
        let body = resource_exhausted("rl", json!([error_info("RATE_LIMIT_EXCEEDED", None), retry_info("20s")]));
        assert_eq!(
            classify_antigravity_429(&body),
            Antigravity429 { kind: Antigravity429Kind::RateLimited, retry_after_ms: Some(20_000) }
        );
    }

    #[test]
    fn promotes_a_long_rate_limit_delay_to_quota_exhaustion() {
        let body = resource_exhausted("rl", json!([error_info("RATE_LIMIT_EXCEEDED", None), retry_info("3600s")]));
        assert_eq!(
            classify_antigravity_429(&body),
            Antigravity429 { kind: Antigravity429Kind::QuotaExhausted, retry_after_ms: Some(3_600_000) }
        );
    }

    #[test]
    fn uses_the_threshold_boundary_inclusively() {
        let body = resource_exhausted(
            "rl",
            json!([error_info("RATE_LIMIT_EXCEEDED", None), retry_info(&format!("{}s", ANTIGRAVITY_SHORT_COOLDOWN_MS / 1000))]),
        );
        assert_eq!(classify_antigravity_429(&body).kind, Antigravity429Kind::QuotaExhausted);
    }

    #[test]
    fn leaves_a_rate_limit_with_no_delay_unclassified() {
        let body = resource_exhausted("rl", json!([error_info("RATE_LIMIT_EXCEEDED", None)]));
        assert_eq!(classify_antigravity_429(&body).kind, Antigravity429Kind::Unknown);
    }

    #[test]
    fn falls_back_to_the_quota_keyword_with_no_structured_reason() {
        let body = json!({ "error": { "status": "RESOURCE_EXHAUSTED", "message": "model quota exhausted for today" } });
        assert_eq!(classify_antigravity_429(&body).kind, Antigravity429Kind::QuotaExhausted);
    }

    #[test]
    fn survives_null_and_non_object_entries_in_a_heterogeneous_details_array() {
        // Must classify, never panic — a classifier crash would turn a benchable 429 into a
        // generic 502 before dispatch can fail over.
        let verdict = classify_antigravity_429(&resource_exhausted(
            "mixed",
            json!([Value::Null, 42, "junk", error_info("QUOTA_EXHAUSTED", None)]),
        ));
        assert_eq!(verdict.kind, Antigravity429Kind::QuotaExhausted);
    }

    #[test]
    fn does_not_classify_a_non_resource_exhausted_429() {
        let body = json!({ "error": { "status": "UNAVAILABLE", "message": "backend busy" } });
        assert_eq!(classify_antigravity_429(&body).kind, Antigravity429Kind::Unknown);
    }

    // ── antigravityBenchUntil ───────────────────────────────────────────────

    const NOW: i64 = 1_700_000_000_000;

    #[test]
    fn benches_an_hour_when_quota_is_spent_with_no_upstream_reset() {
        let body = resource_exhausted("quota", json!([error_info("QUOTA_EXHAUSTED", None)]));
        assert_eq!(antigravity_bench_until(&body, NOW), Some(NOW + ANTIGRAVITY_QUOTA_BENCH_MS));
    }

    #[test]
    fn prefers_the_upstream_reset_over_the_heuristic() {
        let body = resource_exhausted("quota", json!([error_info("QUOTA_EXHAUSTED", None), retry_info("90s")]));
        assert_eq!(antigravity_bench_until(&body, NOW), Some(NOW + 90_000));
    }

    #[test]
    fn benches_exactly_the_throttle_window_for_a_transient_rate_limit() {
        let body = resource_exhausted("rl", json!([error_info("RATE_LIMIT_EXCEEDED", None), retry_info("20s")]));
        assert_eq!(antigravity_bench_until(&body, NOW), Some(NOW + 20_000));
    }

    #[test]
    fn leaves_the_routing_modules_default_alone_when_unclassified() {
        assert_eq!(antigravity_bench_until(&json!({ "error": { "message": "?" } }), NOW), None);
    }

    // ── isAntigravityNoCapacity ─────────────────────────────────────────────

    #[test]
    fn recognizes_a_no_capacity_body_on_429_and_503() {
        let body = json!({ "error": { "message": "no capacity available for this model" } });
        assert!(is_antigravity_no_capacity(429, &body));
        assert!(is_antigravity_no_capacity(503, &body));
    }

    #[test]
    fn ignores_other_statuses_and_unrelated_bodies() {
        assert!(!is_antigravity_no_capacity(500, &json!({ "error": { "message": "no capacity" } })));
        assert!(!is_antigravity_no_capacity(429, &json!({ "error": { "message": "quota" } })));
    }
}
