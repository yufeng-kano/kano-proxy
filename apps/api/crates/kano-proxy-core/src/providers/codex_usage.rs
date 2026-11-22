//! Codex subscription usage (docs/providers.md § Codex).
//!
//! Codex subscription usage. `/codex/usage` carries a path-level bot rule of its own, while
//! the `/wham/usage` alias passes, so the preferred alias is tried first and a `403` HTML
//! challenge on one alias never aborts the other.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::db::accounts::iso_from_ms;
use crate::upstream::UpstreamRequest;
use crate::AppState;

use super::types::UsageWindow;

/// The CLI identity, version tracking [`super::codex_models::CODEX_CLIENT_VERSION`].
const CODEX_CLI_UA: &str = "codex_cli_rs/0.165.0";
const CODEX_BASE: &str = "https://chatgpt.com/backend-api";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CodexRateWindow {
    pub used_percent: Option<f64>,
    pub limit_window_seconds: Option<i64>,
    pub reset_at: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CodexRateLimit {
    pub primary_window: Option<CodexRateWindow>,
    pub secondary_window: Option<CodexRateWindow>,
}

/// The parts of the usage answer this server reads: `email` and `plan_type` for the account
/// facts, the windows and credits for routing and the bars.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CodexUsagePayload {
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub rate_limit: Option<CodexRateLimit>,
    /// `credits: {has_credits, unlimited, balance, …} | null` (docs/providers.md § Codex
    /// "Credit spillover"). Kept as raw JSON so an unexpected field type never fails the whole
    /// usage parse; [`credits_meta`] reads it leniently.
    pub credits: Option<Value>,
}

/// `payload` is set exactly when the read succeeded.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CodexUsageResult {
    pub payload: Option<CodexUsagePayload>,
    /// True when edge bot-challenged or transient; the account is still usable for chat.
    pub edge_blocked: bool,
    pub error: Option<String>,
}

fn usage_request(path: &str, access_token: &str, account_id: &str) -> UpstreamRequest {
    UpstreamRequest::get(format!("{CODEX_BASE}{path}"))
        .header("authorization", &format!("Bearer {access_token}"))
        .header("chatgpt-account-id", account_id)
        .header("openai-beta", "responses=experimental")
        .header("originator", "codex_cli_rs")
        .header("accept", "application/json")
        .header("user-agent", CODEX_CLI_UA)
        .header("accept-language", "en-US,en;q=0.9")
}

fn looks_html(text: &str) -> bool {
    if text.trim_start().starts_with('<') {
        return true;
    }
    let lower = text.to_lowercase();
    lower.contains("just a moment") || lower.contains("cf-browser") || lower.contains("challenge")
}

pub async fn fetch_codex_usage_json(cx: &AppState, access_token: &str, account_id: &str) -> CodexUsageResult {
    if account_id.is_empty() {
        return CodexUsageResult { payload: None, edge_blocked: false, error: Some("missing chatgpt account id".to_string()) };
    }

    let mut last_status = 0u16;
    let mut last_body = String::new();
    let mut saw_html_challenge = false;

    for path in ["/wham/usage", "/codex/usage"] {
        let res = match cx.transport().send(usage_request(path, access_token, account_id)).await {
            Ok(res) => res,
            Err(e) => {
                last_body = e.to_string();
                continue;
            }
        };
        let status = res.status;
        last_status = status.as_u16();
        let text = match res.text().await {
            Ok(text) => text,
            Err(e) => {
                last_body = e.to_string();
                continue;
            }
        };
        last_body = text.chars().take(200).collect();

        if status.is_success() {
            return match serde_json::from_str::<CodexUsagePayload>(&text) {
                Ok(payload) => CodexUsageResult { payload: Some(payload), edge_blocked: false, error: None },
                Err(_) => CodexUsageResult {
                    payload: None,
                    edge_blocked: false,
                    error: Some("usage JSON parse failed".to_string()),
                },
            };
        }

        // HTML challenge / cloudflare / bot wall. A challenged alias must not prevent the
        // remaining alias from being tried.
        if last_status == 403 && looks_html(&text) {
            saw_html_challenge = true;
        }
    }

    let edge_blocked = last_status == 403 || saw_html_challenge;
    let error = if edge_blocked {
        if saw_html_challenge {
            "usage edge blocked (403 bot challenge)".to_string()
        } else {
            "usage edge blocked (403)".to_string()
        }
    } else {
        let status = if last_status == 0 { "error".to_string() } else { last_status.to_string() };
        let suffix = if last_body.is_empty() { String::new() } else { format!(": {last_body}") };
        format!("usage {status}{suffix}")
    };
    CodexUsageResult { payload: None, edge_blocked, error: Some(error) }
}

/// `used_percent` is a percent (0–100), never a 0–1 fraction — the admin UI renders it as
/// given (docs/admin-ui.md).
pub fn windows_from_codex_payload(payload: &CodexUsagePayload) -> Vec<UsageWindow> {
    let Some(rate_limit) = &payload.rate_limit else { return Vec::new() };
    [&rate_limit.primary_window, &rate_limit.secondary_window]
        .into_iter()
        .flatten()
        .map(|w| UsageWindow {
            label: window_label(w.limit_window_seconds),
            utilization: w.used_percent,
            resets_at: w.reset_at.map(|seconds| iso_from_ms(seconds * 1000)),
            value: None,
            scope: None,
        })
        .collect()
}

/// The `credits` value stored in the account facts: `{has_credits, unlimited, balance}` with
/// wrong types read as absent (`false` / `null`), or `null` when the payload has none — an
/// explicit null, so a credit pool that disappears upstream overwrites the stored one.
pub fn credits_meta(payload: &CodexUsagePayload) -> Value {
    let Some(credits) = payload.credits.as_ref().and_then(Value::as_object) else { return Value::Null };
    let flag = |key: &str| credits.get(key).and_then(Value::as_bool).unwrap_or(false);
    let balance = match credits.get("balance") {
        Some(Value::String(s)) => Value::String(s.clone()),
        Some(Value::Number(n)) => Value::String(n.to_string()),
        _ => Value::Null,
    };
    serde_json::json!({ "has_credits": flag("has_credits"), "unlimited": flag("unlimited"), "balance": balance })
}

fn window_label(seconds: Option<i64>) -> String {
    let Some(seconds) = seconds.filter(|s| *s != 0) else { return "window".to_string() };
    if seconds == 604_800 {
        return "Week".to_string();
    }
    if seconds % 3600 == 0 && seconds < 86_400 {
        return format!("{}h", seconds / 3600);
    }
    if seconds % 86_400 == 0 {
        return format!("{}d", seconds / 86_400);
    }
    format!("{seconds}s")
}

/// The usage payload as the generic JSON the identity helper reads.
pub fn usage_payload_value(payload: &CodexUsagePayload) -> Value {
    serde_json::to_value(payload).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{skip_without_db, test_pool, test_state};
    use crate::upstream::transport::UpstreamResponse;
    use crate::upstream::MockTransport;
    use http::StatusCode;
    use serde_json::json;
    use std::sync::Arc;

    const CHALLENGE: &str = "<!doctype html><title>Just a moment</title>";

    fn valid_usage() -> Value {
        json!({ "rate_limit": { "primary_window": { "used_percent": 17, "limit_window_seconds": 18000 } } })
    }

    fn payload(value: Value) -> CodexUsagePayload {
        serde_json::from_value(value).unwrap()
    }

    async fn state(mock: &Arc<MockTransport>) -> Option<crate::AppState> {
        Some(test_state(test_pool().await?, mock.clone()))
    }

    fn html(status: u16) -> Result<UpstreamResponse, crate::upstream::transport::TransportError> {
        let mut headers = http::HeaderMap::new();
        headers.insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("text/html"));
        Ok(UpstreamResponse::from_bytes(StatusCode::from_u16(status).unwrap(), headers, CHALLENGE))
    }

    #[tokio::test]
    async fn tries_wham_usage_first_and_succeeds_there() {
        let mock = MockTransport::new();
        let Some(cx) = state(&mock).await else { return skip_without_db() };
        mock.expect(|_| Ok(UpstreamResponse::json(StatusCode::OK, &valid_usage())));

        let result = fetch_codex_usage_json(&cx, "tok_test", "acct_test").await;

        assert!(result.payload.is_some());
        assert_eq!(result.payload, Some(payload(valid_usage())));
        let urls: Vec<String> = mock.requests().iter().map(|r| r.url.clone()).collect();
        assert_eq!(urls, vec!["https://chatgpt.com/backend-api/wham/usage".to_string()]);
        assert_eq!(mock.requests()[0].header("user-agent"), Some(CODEX_CLI_UA));
        assert_eq!(mock.requests()[0].header("chatgpt-account-id"), Some("acct_test"));
    }

    #[tokio::test]
    async fn continues_from_a_challenged_wham_alias_to_codex_usage() {
        let mock = MockTransport::new();
        let Some(cx) = state(&mock).await else { return skip_without_db() };
        mock.expect(|_| html(403));
        mock.expect(|_| Ok(UpstreamResponse::json(StatusCode::OK, &valid_usage())));

        let result = fetch_codex_usage_json(&cx, "tok_test", "acct_test").await;

        assert!(result.payload.is_some());
        assert_eq!(result.payload, Some(payload(valid_usage())));
        let urls: Vec<String> = mock.requests().iter().map(|r| r.url.clone()).collect();
        assert_eq!(
            urls,
            vec![
                "https://chatgpt.com/backend-api/wham/usage".to_string(),
                "https://chatgpt.com/backend-api/codex/usage".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn reports_edge_blocked_only_after_both_aliases_challenge() {
        let mock = MockTransport::new();
        let Some(cx) = state(&mock).await else { return skip_without_db() };
        mock.expect(|_| html(403));
        mock.expect(|_| html(403));

        let result = fetch_codex_usage_json(&cx, "tok_test", "acct_test").await;

        assert_eq!(mock.requests().len(), 2);
        assert!(result.payload.is_none());
        assert!(result.edge_blocked);
        assert_eq!(result.error.as_deref(), Some("usage edge blocked (403 bot challenge)"));
    }

    #[tokio::test]
    async fn a_successful_response_that_is_not_json_does_not_try_the_fallback_alias() {
        let mock = MockTransport::new();
        let Some(cx) = state(&mock).await else { return skip_without_db() };
        mock.expect(|_| Ok(UpstreamResponse::from_bytes(StatusCode::OK, http::HeaderMap::new(), "not JSON")));

        let result = fetch_codex_usage_json(&cx, "tok_test", "acct_test").await;

        assert_eq!(
            result,
            CodexUsageResult { payload: None, edge_blocked: false, error: Some("usage JSON parse failed".into()) }
        );
        assert_eq!(mock.requests().len(), 1);
    }

    #[tokio::test]
    async fn a_missing_account_id_never_touches_the_network() {
        let mock = MockTransport::new();
        let Some(cx) = state(&mock).await else { return skip_without_db() };
        let result = fetch_codex_usage_json(&cx, "tok_test", "").await;
        assert_eq!(result.error.as_deref(), Some("missing chatgpt account id"));
        assert!(mock.requests().is_empty());
    }

    // --- windows_from_codex_payload: window mapping and the utilization scale contract ---

    fn windows(value: Value) -> Vec<UsageWindow> {
        windows_from_codex_payload(&payload(value))
    }

    #[test]
    fn used_percent_passes_through_as_a_percent_and_absent_is_null_not_zero() {
        // 1 is the value a 0–1 heuristic would rescale to 100%.
        for percent in [1, 73, 100] {
            let w = windows(json!({ "rate_limit": { "primary_window": { "used_percent": percent, "limit_window_seconds": 18000 } } }));
            assert_eq!(w[0].utilization, Some(f64::from(percent)));
        }
        let w = windows(json!({ "rate_limit": { "primary_window": { "limit_window_seconds": 18000 } } }));
        assert_eq!(w[0].utilization, None);
    }

    #[test]
    fn reset_at_converts_to_an_iso_string_and_absent_maps_to_null() {
        let w = windows(json!({ "rate_limit": {
            "primary_window": { "used_percent": 10, "limit_window_seconds": 18000, "reset_at": 1_735_689_600u64 },
            "secondary_window": { "used_percent": 20, "limit_window_seconds": 604800 },
        } }));
        assert_eq!(w[0].resets_at.as_deref(), Some("2025-01-01T00:00:00.000Z"));
        assert_eq!(w[1].resets_at, None);
    }

    #[test]
    fn labels_derive_from_limit_window_seconds() {
        let label = |seconds: Value| {
            windows(json!({ "rate_limit": { "primary_window": { "used_percent": 1, "limit_window_seconds": seconds } } }))[0]
                .label
                .clone()
        };
        assert_eq!(label(json!(604800)), "Week");
        assert_eq!(label(json!(18000)), "5h");
        assert_eq!(label(json!(3600)), "1h");
        assert_eq!(label(json!(172800)), "2d");
        assert_eq!(label(json!(7777)), "7777s");
        assert_eq!(
            windows(json!({ "rate_limit": { "primary_window": { "used_percent": 1 } } }))[0].label,
            "window"
        );
    }

    #[test]
    fn both_windows_map_in_order_and_an_explicit_null_window_is_skipped() {
        let w = windows(json!({ "rate_limit": {
            "primary_window": { "used_percent": 5, "limit_window_seconds": 18000 },
            "secondary_window": { "used_percent": 6, "limit_window_seconds": 604800 },
        } }));
        assert_eq!(w.len(), 2);
        assert_eq!((w[0].label.as_str(), w[0].utilization), ("5h", Some(5.0)));
        assert_eq!((w[1].label.as_str(), w[1].utilization), ("Week", Some(6.0)));

        assert!(windows(json!({})).is_empty());
        let w = windows(json!({ "rate_limit": {
            "primary_window": { "used_percent": 1, "limit_window_seconds": 18000 },
            "secondary_window": null,
        } }));
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn credits_are_read_leniently_and_a_missing_pool_is_an_explicit_null() {
        let credits = |value: Value| credits_meta(&payload(value));
        assert_eq!(
            credits(json!({ "credits": { "has_credits": true, "unlimited": false, "balance": "12.50", "approx_local_messages": [1, 2] } })),
            json!({ "has_credits": true, "unlimited": false, "balance": "12.50" })
        );
        // A wrong type is absent, not a parse failure of the whole usage payload.
        assert_eq!(
            credits(json!({ "credits": { "has_credits": "yes", "unlimited": null, "balance": 3 } })),
            json!({ "has_credits": false, "unlimited": false, "balance": "3" })
        );
        assert_eq!(credits(json!({ "credits": null })), Value::Null);
        assert_eq!(credits(json!({})), Value::Null);
        assert_eq!(credits(json!({ "credits": "unexpected" })), Value::Null);
    }

    #[test]
    fn the_cli_user_agent_tracks_the_single_client_version_constant() {
        assert_eq!(CODEX_CLI_UA, format!("codex_cli_rs/{}", super::super::codex_models::CODEX_CLIENT_VERSION));
    }
}
