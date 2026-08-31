//! HTTP client for the /agent/v1 surface (docs/cli.md § Server routes) plus
//! the local target probes. Never logs tokens or bodies.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::json;

use crate::state::{State, StateFile};

#[derive(Debug, Deserialize)]
pub struct LoginStart {
    pub request_id: String,
    pub verify_url: String,
    #[allow(dead_code)]
    pub expires_at: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginComplete {
    pub device_id: String,
    pub refresh_token: String,
    /// Unused today — commands mint their own via the rotation path, which
    /// also exercises persist-before-use. Kept for wire completeness.
    #[allow(dead_code)]
    pub access_token: String,
    #[allow(dead_code)]
    pub expires_in: u64,
}

#[derive(Debug, Deserialize)]
pub struct TokenPair {
    pub refresh_token: String,
    pub access_token: String,
    #[allow(dead_code)]
    pub expires_in: u64,
}

#[derive(Debug, Deserialize)]
pub struct ProviderItem {
    pub id: String,
    pub slug: String,
    #[allow(dead_code)]
    pub name: String,
    pub format: String,
    pub connected: bool,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub models_updated_at: Option<String>,
    #[serde(default)]
    pub device_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreatedProvider {
    pub id: String,
    pub slug: String,
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("kano-proxy-cli/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("reqwest client builds")
}

fn api_error(status: reqwest::StatusCode, body: &str) -> anyhow::Error {
    #[derive(Deserialize)]
    struct Err1 {
        error: String,
    }
    match serde_json::from_str::<Err1>(body) {
        Ok(e) => anyhow!("server said ({status}): {}", e.error),
        Err(_) => anyhow!("server answered {status}"),
    }
}

async fn post_json<T: for<'de> Deserialize<'de>>(
    url: &str,
    body: serde_json::Value,
    bearer: Option<&str>,
) -> Result<T> {
    let mut req = client().post(url).json(&body);
    if let Some(token) = bearer {
        req = req.bearer_auth(token);
    }
    let res = req.send().await.with_context(|| format!("cannot reach {url}"))?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error(status, &text));
    }
    serde_json::from_str(&text).with_context(|| format!("unexpected response from {url}"))
}

pub async fn login_start(base_url: &str, device_name: &str) -> Result<LoginStart> {
    post_json(
        &format!("{base_url}/agent/v1/login/start"),
        json!({ "device_name": device_name }),
        None,
    )
    .await
}

pub async fn login_complete(base_url: &str, request_id: &str, code: &str) -> Result<LoginComplete> {
    post_json(
        &format!("{base_url}/agent/v1/login/complete"),
        json!({ "request_id": request_id, "code": code }),
        None,
    )
    .await
}

async fn rotate_token(base_url: &str, refresh_token: &str) -> Result<TokenPair> {
    post_json(
        &format!("{base_url}/agent/v1/token"),
        json!({ "refresh_token": refresh_token }),
        None,
    )
    .await
}

/// Rotate the refresh token and return a fresh access token.
///
/// The order is the contract (docs/cli.md § Device auth): take the lock,
/// re-read the state (a sibling process may have rotated first), call the
/// server, persist the new refresh token, and only then hand out the access
/// token. HTTP 401 means the device is revoked or unregistered — callers exit
/// with a message to re-run init rather than retrying.
pub async fn fresh_access_token(file: &StateFile) -> Result<String> {
    let _lock = file.acquire_lock()?;
    let mut state = file.load()?;
    let refresh = state
        .refresh_token
        .clone()
        .context("this device is not signed in — run `kano-proxy init` first")?;
    let base = state.base_url.clone();
    let pair = rotate_token(&base, &refresh).await?;
    state.refresh_token = Some(pair.refresh_token);
    file.save(&state)
        .context("could not persist the rotated refresh token — refusing to continue")?;
    Ok(pair.access_token)
}

pub async fn list_providers(base_url: &str, access_token: &str) -> Result<Vec<ProviderItem>> {
    #[derive(Deserialize)]
    struct Wrapper {
        providers: Vec<ProviderItem>,
    }
    let url = format!("{base_url}/agent/v1/providers");
    let res = client()
        .get(&url)
        .bearer_auth(access_token)
        .send()
        .await
        .with_context(|| format!("cannot reach {url}"))?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error(status, &text));
    }
    let w: Wrapper = serde_json::from_str(&text).context("unexpected providers response")?;
    Ok(w.providers)
}

pub async fn create_provider(
    base_url: &str,
    access_token: &str,
    slug: &str,
    format: &str,
    expose: &[String],
    initial_models: &[String],
) -> Result<CreatedProvider> {
    let mut body = json!({ "slug": slug, "format": format });
    if !expose.is_empty() {
        body["expose"] = json!(expose);
    }
    if !initial_models.is_empty() {
        body["initial_models"] = json!(initial_models);
    }
    post_json(&format!("{base_url}/agent/v1/providers"), body, Some(access_token)).await
}

pub async fn delete_provider(base_url: &str, access_token: &str, id: &str) -> Result<()> {
    let url = format!("{base_url}/agent/v1/providers/{id}");
    let res = client()
        .delete(&url)
        .bearer_auth(access_token)
        .send()
        .await
        .with_context(|| format!("cannot reach {url}"))?;
    let status = res.status();
    if !status.is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(api_error(status, &text));
    }
    Ok(())
}

/// `GET <target>/models` (openai) / `GET <target>/v1/models` (anthropic) →
/// bare model ids. The catalog the agent pushes upstream (docs/cli.md
/// § Model catalog).
pub async fn probe_local_models(format: &str, target: &str, target_key: Option<&str>) -> Result<Vec<String>> {
    let url = match format {
        "anthropic" => format!("{}/v1/models", target.trim_end_matches('/')),
        _ => format!("{}/models", target.trim_end_matches('/')),
    };
    let mut req = client().get(&url).timeout(std::time::Duration::from_secs(10));
    if let Some(key) = target_key {
        req = match format {
            "anthropic" => req.header("x-api-key", key).header("anthropic-version", "2023-06-01"),
            _ => req.bearer_auth(key),
        };
    }
    let res = req.send().await.with_context(|| format!("cannot reach {url}"))?;
    if !res.status().is_success() {
        bail!("models probe answered {}", res.status());
    }
    #[derive(Deserialize)]
    struct Entry {
        id: Option<String>,
    }
    #[derive(Deserialize)]
    struct List {
        #[serde(default)]
        data: Vec<Entry>,
    }
    let list: List = res.json().await.context("models probe returned unexpected JSON")?;
    Ok(list.data.into_iter().filter_map(|e| e.id).filter(|id| !id.is_empty()).collect())
}

/// Base URL sanity for init/add input: absolute http(s), no trailing slash kept.
pub fn normalize_base_url(raw: &str) -> Result<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if !(trimmed.starts_with("https://") || trimmed.starts_with("http://")) {
        bail!("base URL must start with http:// or https://");
    }
    if trimmed.len() <= "https://".len() {
        bail!("base URL is missing a host");
    }
    Ok(trimmed.to_string())
}

/// Load state and require a signed-in device, with a uniform message.
pub fn require_signed_in(state: &State) -> Result<()> {
    if !state.signed_in() {
        bail!("this device is not signed in — run `kano-proxy init` first");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_base_urls() {
        assert_eq!(normalize_base_url(" https://proxy.example.com/ ").unwrap(), "https://proxy.example.com");
        assert_eq!(normalize_base_url("http://127.0.0.1:8787").unwrap(), "http://127.0.0.1:8787");
        assert!(normalize_base_url("proxy.example.com").is_err());
        assert!(normalize_base_url("https://").is_err());
    }
}
