//! Codex subscription usage (docs/providers.md § Codex).
//!
//! Codex subscription usage. `/codex/usage` carries a path-level bot rule of its own, while
//! the `/wham/usage` alias passes, so the preferred alias is tried first and a `403` HTML
//! challenge on one alias never aborts the other. Egress is direct from this server (the
//! Cloud Run relay is retired, docs/rust-server.md), so the Worker-specific `CF-Worker`
//! header wall no longer applies; the alias order and the edge-blocked classification stay
//! as documented because the path rule is upstream's, not Cloudflare's.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::db::accounts::iso_from_ms;
use crate::upstream::UpstreamRequest;
use crate::AppState;

use super::codex_models::CODEX_CLIENT_VERSION;
use super::types::UsageWindow;

pub const CODEX_CLI_UA: &str = "codex_cli_rs/0.156.0";
const CODEX_BASE: &str = "https://chatgpt.com/backend-api";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CodexRateWindow {
    #[serde(default)]
    pub used_percent: Option<f64>,
    #[serde(default)]
    pub limit_window_seconds: Option<i64>,
    #[serde(default)]
    pub reset_after_seconds: Option<i64>,
    #[serde(default)]
    pub reset_at: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CodexRateLimit {
    #[serde(default)]
    pub allowed: Option<bool>,
    #[serde(default)]
    pub limit_reached: Option<bool>,
    #[serde(default)]
    pub primary_window: Option<CodexRateWindow>,
    #[serde(default)]
    pub secondary_window: Option<CodexRateWindow>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CodexUsagePayload {
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub plan_type: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub rate_limit: Option<CodexRateLimit>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CodexUsageResult {
    pub ok: bool,
    pub status: u16,
    pub payload: Option<CodexUsagePayload>,
    /// True when edge bot-challenged or transient; the account is still usable for chat.
    pub edge_blocked: bool,
    pub error: Option<String>,
}

pub fn codex_usage_headers(access_token: &str, account_id: &str) -> Vec<(&'static str, String)> {
    vec![
        ("authorization", format!("Bearer {access_token}")),
        ("chatgpt-account-id", account_id.to_string()),
        ("openai-beta", "responses=experimental".to_string()),
        ("originator", "codex_cli_rs".to_string()),
        ("accept", "application/json".to_string()),
        ("user-agent", CODEX_CLI_UA.to_string()),
        ("accept-language", "en-US,en;q=0.9".to_string()),
    ]
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
        return CodexUsageResult {
            ok: false,
            status: 0,
            payload: None,
            edge_blocked: false,
            error: Some("missing chatgpt account id".to_string()),
        };
    }

    let mut last_status = 0u16;
    let mut last_body = String::new();
    let mut saw_html_challenge = false;

    for path in ["/wham/usage", "/codex/usage"] {
        let mut req = UpstreamRequest::get(format!("{CODEX_BASE}{path}"));
        for (name, value) in codex_usage_headers(access_token, account_id) {
            req = req.header(name, &value);
        }
        let res = match cx.transport().send(req).await {
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
                Ok(payload) => CodexUsageResult {
                    ok: true,
                    status: last_status,
                    payload: Some(payload),
                    edge_blocked: false,
                    error: None,
                },
                Err(_) => CodexUsageResult {
                    ok: false,
                    status: last_status,
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
    CodexUsageResult { ok: false, status: last_status, payload: None, edge_blocked, error: Some(error) }
}

/// `used_percent` is a percent (0–100), never a 0–1 fraction — the admin UI renders it as
/// given (docs/admin-ui.md).
pub fn windows_from_codex_payload(payload: &CodexUsagePayload) -> Vec<UsageWindow> {
    let rate_limit = payload.rate_limit.clone().unwrap_or_default();
    [rate_limit.primary_window, rate_limit.secondary_window]
        .into_iter()
        .flatten()
        .map(|w| UsageWindow {
            label: window_label(w.limit_window_seconds),
            utilization: w.used_percent,
            resets_at: w.reset_at.map(|seconds| iso_from_ms(seconds * 1000)),
            value: None,
        })
        .collect()
}

pub fn window_label(seconds: Option<i64>) -> String {
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

/// Kept so the module's CLI identity stays tied to one constant.
pub fn codex_cli_user_agent() -> String {
    format!("codex_cli_rs/{CODEX_CLIENT_VERSION}")
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

        assert!(result.ok);
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

        assert!(result.ok);
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
        assert!(!result.ok);
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
            CodexUsageResult {
                ok: false,
                status: 200,
                payload: None,
                edge_blocked: false,
                error: Some("usage JSON parse failed".into()),
            }
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

    // --- windowsFromCodexPayload: window mapping and the utilization scale contract ---

    fn windows(value: Value) -> Vec<UsageWindow> {
        windows_from_codex_payload(&payload(value))
    }

    #[test]
    fn regression_used_percent_one_stays_one() {
        // The exact value a removed frontend heuristic once rescaled to 100%.
        let w = windows(json!({ "rate_limit": { "primary_window": {
            "used_percent": 1, "limit_window_seconds": 18000, "reset_at": 1_780_000_000u64 } } }));
        assert_eq!(w[0].utilization, Some(1.0));
    }

    #[test]
    fn mid_range_and_full_percentages_pass_through_unchanged() {
        let w = windows(json!({ "rate_limit": { "primary_window": { "used_percent": 73, "limit_window_seconds": 18000 } } }));
        assert_eq!(w[0].utilization, Some(73.0));
        let w = windows(json!({ "rate_limit": { "primary_window": { "used_percent": 100, "limit_window_seconds": 604800 } } }));
        assert_eq!(w[0].utilization, Some(100.0));
    }

    #[test]
    fn a_window_without_used_percent_maps_to_null_not_zero() {
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
    fn the_cli_user_agent_tracks_the_single_client_version_constant() {
        assert_eq!(codex_cli_user_agent(), CODEX_CLI_UA);
    }
}
