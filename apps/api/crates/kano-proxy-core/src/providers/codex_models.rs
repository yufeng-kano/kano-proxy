//! The Codex model catalog (docs/providers.md § Codex § Models).
//!
//! The live catalog comes from `GET chatgpt.com/backend-api/codex/models?client_version=…`
//! with the CLI identity headers; on any failure the public catalog mirrors are tried in
//! order. If every source fails the list is empty — never a hard-coded or invented catalog.
//! Egress is direct in the Rust edition (docs/rust-server.md § Storage): the Cloud Run relay
//! is retired, so the primary request goes straight through `AppState::transport()`.

use std::time::Duration;

use serde_json::Value;

use crate::upstream::{UpstreamRequest, UpstreamResponse};
use crate::AppState;

use super::types::{ListedModels, UpstreamModel};

pub const DIRECT_UPSTREAM_BASE: &str = "https://chatgpt.com/backend-api";
pub const CODEX_MODELS_ENDPOINT: &str = concat!("https://chatgpt.com/backend-api", "/codex/models");

/// The one Codex CLI version every chatgpt.com call advertises. `/codex/models` filters its
/// catalog by `client_version` — an old pin silently hides models the account can already
/// use, so bump this when a new model fails to appear. Track npm `@openai/codex` latest.
pub const CODEX_CLIENT_VERSION: &str = "0.156.0";
pub const CODEX_USER_AGENT: &str = "codex_cli_rs/0.156.0 (Mac OS 26.3.1; arm64) iTerm.app/3.6.9";

/// Public catalog mirrors, tried in order when the live endpoint is bot-walled.
pub const CODEX_MODEL_MIRROR_URLS: [&str; 2] = [
    "https://models.router-for.me/codex_client_models.json",
    "https://raw.githubusercontent.com/router-for-me/models/refs/heads/main/codex_client_models.json",
];

const CODEX_ORIGINATOR: &str = "codex_cli_rs";
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Everything `encodeURIComponent` escapes: all but the unreserved set plus `!*'()`.
const URI_COMPONENT: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

fn is_json_content_type(content_type: Option<&str>) -> bool {
    let Some(value) = content_type else { return false };
    let media_type = value.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    media_type == "application/json" || media_type.ends_with("+json")
}

fn map_models(payload: &Value) -> Option<Vec<UpstreamModel>> {
    let models = payload.as_object()?.get("models")?.as_array()?;
    let mut seen: Vec<&str> = Vec::new();
    let mut mapped = Vec::new();
    for value in models {
        let Some(entry) = value.as_object() else { continue };
        if entry.get("visibility").and_then(Value::as_str) == Some("hide") {
            continue;
        }
        let Some(slug) = entry.get("slug").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) else {
            continue;
        };
        if seen.contains(&slug) {
            continue;
        }
        seen.push(slug);
        mapped.push(UpstreamModel {
            id: slug.to_string(),
            display_name: entry.get("display_name").and_then(Value::as_str).map(str::to_string),
        });
    }
    Some(mapped)
}

fn primary_request(access_token: &str, account_id: &str) -> UpstreamRequest {
    // `encodeURIComponent(CODEX_CLIENT_VERSION)`: the identity for a dotted semver, kept
    // explicit so a future pin carrying a reserved character still produces a valid URL.
    let encoded: String = percent_encoding::utf8_percent_encode(CODEX_CLIENT_VERSION, URI_COMPONENT).to_string();
    let url = format!("{CODEX_MODELS_ENDPOINT}?client_version={encoded}");
    let mut req = UpstreamRequest::get(url)
        .header("accept", "application/json")
        .header("authorization", &format!("Bearer {access_token}"))
        .header("originator", CODEX_ORIGINATOR)
        .header("user-agent", CODEX_USER_AGENT)
        .timeout(FETCH_TIMEOUT);
    if !account_id.is_empty() {
        req = req.header("chatgpt-account-id", account_id);
    }
    req
}

/// Ordered sources: the live endpoint with CLI identity headers, then the public mirrors
/// (plain GETs — a mirror never sees an account token).
pub async fn fetch_codex_models(cx: &AppState, access_token: &str, account_id: &str) -> ListedModels {
    let mut last_error = "models fetch failed".to_string();

    for index in 0..=CODEX_MODEL_MIRROR_URLS.len() {
        let request = if index == 0 {
            primary_request(access_token, account_id)
        } else {
            UpstreamRequest::get(CODEX_MODEL_MIRROR_URLS[index - 1]).timeout(FETCH_TIMEOUT)
        };
        let response: UpstreamResponse = match cx.transport().send(request).await {
            Ok(r) => r,
            Err(e) => {
                last_error = e.to_string().chars().take(200).collect();
                continue;
            }
        };
        if !response.status.is_success() {
            last_error = format!("models {}", response.status.as_u16());
            continue;
        }
        if !is_json_content_type(response.content_type()) {
            last_error = "models response is not JSON".to_string();
            continue;
        }
        let body = match response.text().await {
            Ok(b) => b,
            Err(e) => {
                last_error = e.to_string().chars().take(200).collect();
                continue;
            }
        };
        let Ok(payload) = serde_json::from_str::<Value>(&body) else {
            last_error = "models JSON parse failed".to_string();
            continue;
        };
        match map_models(&payload) {
            Some(models) => return ListedModels { models, error: None },
            None => last_error = "models response has no models array".to_string(),
        }
    }

    ListedModels { models: Vec::new(), error: Some(last_error) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{skip_without_db, test_pool, test_state};
    use crate::upstream::transport::TransportError;
    use crate::upstream::MockTransport;
    use http::StatusCode;
    use serde_json::json;
    use std::sync::Arc;

    fn json_response(value: Value) -> Result<UpstreamResponse, TransportError> {
        Ok(UpstreamResponse::json(StatusCode::OK, &value))
    }

    fn text_response(status: u16, body: &'static str, content_type: &'static str) -> Result<UpstreamResponse, TransportError> {
        let mut headers = http::HeaderMap::new();
        headers.insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static(content_type));
        Ok(UpstreamResponse::from_bytes(StatusCode::from_u16(status).unwrap(), headers, body))
    }

    fn primary_url() -> String {
        format!("{CODEX_MODELS_ENDPOINT}?client_version={CODEX_CLIENT_VERSION}")
    }

    async fn state_with(transport: Arc<MockTransport>) -> Option<(crate::AppState, Arc<MockTransport>)> {
        let pool = test_pool().await?;
        Some((test_state(pool, transport.clone()), transport))
    }

    #[tokio::test]
    async fn fetches_the_live_catalog_with_the_cli_url_and_headers() {
        let Some((cx, mock)) = state_with(MockTransport::new()).await else { return skip_without_db() };
        mock.expect(|_| {
            json_response(json!({ "models": [
                { "slug": "gpt-5.6-sol", "display_name": "GPT-5.6-Sol", "visibility": "list" },
                { "slug": "gpt-5.5", "display_name": null, "visibility": "list" },
            ] }))
        });

        let result = fetch_codex_models(&cx, "access-token", "account-123").await;

        assert_eq!(
            result.models,
            vec![
                UpstreamModel { id: "gpt-5.6-sol".into(), display_name: Some("GPT-5.6-Sol".into()) },
                UpstreamModel { id: "gpt-5.5".into(), display_name: None },
            ]
        );
        assert_eq!(result.error, None);
        let requests = mock.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url, primary_url());
        assert_eq!(requests[0].method, http::Method::GET);
        assert_eq!(requests[0].header("accept"), Some("application/json"));
        assert_eq!(requests[0].header("authorization"), Some("Bearer access-token"));
        assert_eq!(requests[0].header("originator"), Some("codex_cli_rs"));
        assert_eq!(requests[0].header("user-agent"), Some(CODEX_USER_AGENT));
        assert_eq!(requests[0].header("chatgpt-account-id"), Some("account-123"));
    }

    #[tokio::test]
    async fn omits_chatgpt_account_id_when_the_account_id_is_empty() {
        let Some((cx, mock)) = state_with(MockTransport::new()).await else { return skip_without_db() };
        mock.expect(|_| json_response(json!({ "models": [] })));
        fetch_codex_models(&cx, "access-token", "").await;
        assert_eq!(mock.requests()[0].header("chatgpt-account-id"), None);
    }

    #[tokio::test]
    async fn filters_hidden_models_skips_invalid_slugs_and_deduplicates() {
        let Some((cx, mock)) = state_with(MockTransport::new()).await else { return skip_without_db() };
        mock.expect(|_| {
            json_response(json!({ "models": [
                { "slug": "hidden", "display_name": "Hidden", "visibility": "hide" },
                { "display_name": "Missing slug" },
                { "slug": "", "display_name": "Empty slug" },
                { "slug": 123, "display_name": "Non-string slug" },
                { "slug": "gpt-5.5", "display_name": "First" },
                { "slug": "gpt-5.5", "display_name": "Second" },
                { "slug": "visible", "display_name": "Visible" },
            ] }))
        });

        let result = fetch_codex_models(&cx, "access-token", "account-123").await;

        assert_eq!(
            result.models,
            vec![
                UpstreamModel { id: "gpt-5.5".into(), display_name: Some("First".into()) },
                UpstreamModel { id: "visible".into(), display_name: Some("Visible".into()) },
            ]
        );
        assert_eq!(result.error, None);
    }

    #[tokio::test]
    async fn falls_through_a_403_html_bot_wall_to_mirror_1_without_auth_headers() {
        let Some((cx, mock)) = state_with(MockTransport::new()).await else { return skip_without_db() };
        mock.expect(|_| text_response(403, "<html>Just a moment...</html>", "text/html"));
        mock.expect(|_| json_response(json!({ "models": [{ "slug": "mirror-model", "display_name": "Mirror" }] })));

        let result = fetch_codex_models(&cx, "access-token", "account-123").await;

        assert_eq!(
            result.models,
            vec![UpstreamModel { id: "mirror-model".into(), display_name: Some("Mirror".into()) }]
        );
        assert_eq!(result.error, None);
        let urls: Vec<String> = mock.requests().iter().map(|r| r.url.clone()).collect();
        assert_eq!(urls, vec![primary_url(), CODEX_MODEL_MIRROR_URLS[0].to_string()]);
        assert_eq!(mock.requests()[1].header("authorization"), None);
        assert_eq!(mock.requests()[1].header("chatgpt-account-id"), None);
    }

    #[tokio::test]
    async fn uses_mirror_2_when_mirror_1_fails() {
        let Some((cx, mock)) = state_with(MockTransport::new()).await else { return skip_without_db() };
        mock.expect(|_| text_response(403, "blocked", "text/plain"));
        mock.expect(|_| text_response(503, "unavailable", "text/plain"));
        mock.expect(|_| json_response(json!({ "models": [{ "slug": "mirror-2" }] })));

        let result = fetch_codex_models(&cx, "access-token", "account-123").await;

        assert_eq!(result.models, vec![UpstreamModel { id: "mirror-2".into(), display_name: None }]);
        assert_eq!(result.error, None);
        let urls: Vec<String> = mock.requests().iter().map(|r| r.url.clone()).collect();
        assert_eq!(
            urls,
            vec![primary_url(), CODEX_MODEL_MIRROR_URLS[0].to_string(), CODEX_MODEL_MIRROR_URLS[1].to_string()]
        );
    }

    #[tokio::test]
    async fn returns_an_error_instead_of_failing_when_every_source_fails() {
        let Some((cx, mock)) = state_with(MockTransport::new()).await else { return skip_without_db() };
        mock.expect(|_| Err(TransportError::Connect("network down".into())));
        mock.expect(|_| text_response(502, "not json", "text/plain"));
        mock.expect(|_| text_response(200, "still not json", "text/plain"));

        let result = fetch_codex_models(&cx, "access-token", "account-123").await;

        assert_eq!(mock.requests().len(), 3);
        assert!(result.models.is_empty());
        assert_eq!(result.error.as_deref(), Some("models response is not JSON"));
    }

    #[tokio::test]
    async fn falls_through_malformed_json_without_failing() {
        let Some((cx, mock)) = state_with(MockTransport::new()).await else { return skip_without_db() };
        mock.expect(|_| text_response(200, "{\"models\":", "application/json"));
        mock.expect(|_| json_response(json!({ "models": [{ "slug": "after-malformed" }] })));

        let result = fetch_codex_models(&cx, "access-token", "account-123").await;

        assert_eq!(result.models, vec![UpstreamModel { id: "after-malformed".into(), display_name: None }]);
        assert_eq!(result.error, None);
        let urls: Vec<String> = mock.requests().iter().map(|r| r.url.clone()).collect();
        assert_eq!(urls, vec![primary_url(), CODEX_MODEL_MIRROR_URLS[0].to_string()]);
    }

    #[tokio::test]
    async fn falls_through_a_response_without_a_models_array() {
        let Some((cx, mock)) = state_with(MockTransport::new()).await else { return skip_without_db() };
        for _ in 0..3 {
            mock.expect(|_| json_response(json!({ "models": {} })));
        }

        let result = fetch_codex_models(&cx, "access-token", "account-123").await;

        assert!(result.models.is_empty());
        assert_eq!(result.error.as_deref(), Some("models response has no models array"));
    }

    #[test]
    fn json_content_type_accepts_suffixed_media_types() {
        assert!(is_json_content_type(Some("application/json; charset=utf-8")));
        assert!(is_json_content_type(Some("application/vnd.api+json")));
        assert!(!is_json_content_type(Some("text/html")));
        assert!(!is_json_content_type(None));
    }
}
