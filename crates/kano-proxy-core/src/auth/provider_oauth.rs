//! Provider OAuth helpers (apps/api/src/auth/provider_oauth.ts, docs/auth.md). Claude Code
//! uses browser PKCE, Codex device PKCE is server-side, Antigravity is a plain
//! confidential-client code flow. The provider-specific route handlers live in
//! `routes::providers`; what is here is the URL building, the code exchange and the token
//! parsing they share.

use serde::{Deserialize, Serialize};

use crate::upstream::UpstreamRequest;
use crate::AppState;

use super::pkce::{build_pkce_pair, build_state_token};

pub struct ClaudeOAuth;

impl ClaudeOAuth {
    pub const CLIENT_ID: &'static str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
    pub const AUTHORIZE_URL: &'static str = "https://claude.ai/oauth/authorize";
    pub const TOKEN_URL: &'static str = "https://console.anthropic.com/v1/oauth/token";
    pub const REDIRECT_URI: &'static str = "https://console.anthropic.com/oauth/code/callback";
    pub const SCOPE: &'static str = "org:create_api_key user:profile user:inference";
}

/// Must match the public Codex CLI OAuth client registration.
pub struct CodexDeviceAuth;

impl CodexDeviceAuth {
    pub const CLIENT_ID: &'static str = "app_EMoamEEZ73f0CkXaXp7hrann";
    pub const USER_CODE_URL: &'static str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
    pub const DEVICE_TOKEN_URL: &'static str = "https://auth.openai.com/api/accounts/deviceauth/token";
    pub const TOKEN_URL: &'static str = "https://auth.openai.com/oauth/token";
    pub const REDIRECT_URI: &'static str = "https://auth.openai.com/deviceauth/callback";
}

/// Antigravity is a *confidential* Google OAuth client: no PKCE, and the client secret is
/// required on the code exchange and on every refresh.
///
/// **The credential pair is not in this repository, on purpose.** Both halves come from
/// `ANTIGRAVITY_OAUTH_CLIENT_ID` / `ANTIGRAVITY_OAUTH_CLIENT_SECRET`; with either unset the
/// provider refuses to start a login rather than falling back to something baked in.
/// Endpoints, scopes and the callback port are configuration, not secrets.
pub struct AntigravityOAuth;

impl AntigravityOAuth {
    pub const AUTHORIZE_URL: &'static str = "https://accounts.google.com/o/oauth2/v2/auth";
    pub const TOKEN_URL: &'static str = "https://oauth2.googleapis.com/token";
    /// The only redirect this client has registered: Google rejects any other value, so the
    /// proxy cannot host its own callback and the user pastes the code back instead.
    pub const REDIRECT_URI: &'static str = "http://localhost:51121/oauth-callback";
    pub const SCOPE: &'static str = "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs";
}

/// Stored between login start and complete for a PKCE provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingOAuth {
    pub client_id: String,
    pub code_verifier: String,
    pub oauth_state: String,
    pub redirect_uri: String,
}

/// Stored for Antigravity — no verifier, this client has no PKCE.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingAntigravityOAuth {
    pub client_id: String,
    pub oauth_state: String,
    pub redirect_uri: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
}

fn query(pairs: &[(&str, &str)]) -> String {
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        ser.append_pair(k, v);
    }
    ser.finish()
}

pub struct BeginAuthorization<P> {
    pub authorization_url: String,
    pub pending: P,
}

pub fn begin_claude_authorization(client_id: Option<&str>) -> BeginAuthorization<PendingOAuth> {
    let pkce = build_pkce_pair();
    let oauth_state = build_state_token();
    let cid = client_id.filter(|c| !c.is_empty()).unwrap_or(ClaudeOAuth::CLIENT_ID).to_string();
    let q = query(&[
        ("code", "true"),
        ("client_id", &cid),
        ("response_type", "code"),
        ("redirect_uri", ClaudeOAuth::REDIRECT_URI),
        ("scope", ClaudeOAuth::SCOPE),
        ("code_challenge", &pkce.code_challenge),
        ("code_challenge_method", "S256"),
        ("state", &oauth_state),
    ]);
    BeginAuthorization {
        authorization_url: format!("{}?{q}", ClaudeOAuth::AUTHORIZE_URL),
        pending: PendingOAuth {
            client_id: cid,
            code_verifier: pkce.code_verifier,
            oauth_state,
            redirect_uri: ClaudeOAuth::REDIRECT_URI.to_string(),
        },
    }
}

pub async fn exchange_claude_code(
    state: &AppState,
    code: &str,
    returned_state: &str,
    pending: &PendingOAuth,
) -> anyhow::Result<OAuthTokens> {
    if returned_state != pending.oauth_state {
        anyhow::bail!("Authorization state mismatch. Restart the login flow.");
    }
    let body = serde_json::json!({
        "grant_type": "authorization_code",
        "code": code,
        "state": pending.oauth_state,
        "client_id": pending.client_id,
        "code_verifier": pending.code_verifier,
        "redirect_uri": pending.redirect_uri,
    });
    let response = state.transport().send(UpstreamRequest::post(ClaudeOAuth::TOKEN_URL).json(&body)).await?;
    if !response.status.is_success() {
        let status = response.status.as_u16();
        let detail = response.text().await.unwrap_or_default().trim().to_string();
        let detail = if detail.is_empty() { format!("HTTP {status}") } else { detail };
        anyhow::bail!("Claude OAuth token exchange failed: {detail}");
    }
    Ok(serde_json::from_slice(&response.bytes().await?)?)
}

/// `None` unless the operator configured **both** halves — a client id without its secret
/// cannot complete the exchange, so half a pair is treated as none rather than failing later
/// with an opaque Google error.
pub fn antigravity_oauth_client(state: &AppState) -> Option<(String, String)> {
    let config = state.config();
    let id = config.antigravity_oauth_client_id.as_deref().map(str::trim).filter(|s| !s.is_empty())?;
    let secret = config.antigravity_oauth_client_secret.as_deref().map(str::trim).filter(|s| !s.is_empty())?;
    Some((id.to_string(), secret.to_string()))
}

pub fn begin_antigravity_authorization(client_id: &str) -> BeginAuthorization<PendingAntigravityOAuth> {
    let oauth_state = build_state_token();
    let q = query(&[
        ("access_type", "offline"),
        ("client_id", client_id),
        ("prompt", "consent"),
        ("redirect_uri", AntigravityOAuth::REDIRECT_URI),
        ("response_type", "code"),
        ("scope", AntigravityOAuth::SCOPE),
        ("state", &oauth_state),
    ]);
    BeginAuthorization {
        authorization_url: format!("{}?{q}", AntigravityOAuth::AUTHORIZE_URL),
        pending: PendingAntigravityOAuth {
            client_id: client_id.to_string(),
            oauth_state,
            redirect_uri: AntigravityOAuth::REDIRECT_URI.to_string(),
        },
    }
}

/// The user's browser lands on `http://localhost:51121/oauth-callback?...`, which nothing
/// serves, so they paste either the whole failed URL or just the `code` value. Both are
/// accepted; `state` comes back only in the URL form, and its absence is reported rather than
/// waved through — a bare code carries no CSRF binding of its own.
pub fn parse_antigravity_callback(raw: &str) -> Result<(String, Option<String>), String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err("Paste the code (or the full callback URL) from the browser.".into());
    }
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        let url = url::Url::parse(value).map_err(|_| "That does not look like the callback URL. Paste it again.".to_string())?;
        let mut code = None;
        let mut state = None;
        for (k, v) in url.query_pairs() {
            match k.as_ref() {
                "error" => return Err(format!("Google returned \"{v}\". Restart the login flow.")),
                "code" => code = Some(v.into_owned()),
                "state" => state = Some(v.into_owned()),
                _ => {}
            }
        }
        let code = code.ok_or_else(|| "The callback URL carries no code. Restart the login flow.".to_string())?;
        return Ok((code, state));
    }
    Ok((value.to_string(), None))
}

/// `returned_state` is `None` when the user pasted a bare code; only a returned state is
/// checked against the pending one.
pub async fn exchange_antigravity_code(
    state: &AppState,
    code: &str,
    returned_state: Option<&str>,
    pending: &PendingAntigravityOAuth,
    client_secret: &str,
) -> anyhow::Result<OAuthTokens> {
    if returned_state.is_some_and(|s| s != pending.oauth_state) {
        anyhow::bail!("Authorization state mismatch. Restart the login flow.");
    }
    let body = query(&[
        ("grant_type", "authorization_code"),
        ("code", code),
        ("client_id", &pending.client_id),
        ("client_secret", client_secret),
        ("redirect_uri", &pending.redirect_uri),
    ]);
    let response = state
        .transport()
        .send(
            UpstreamRequest::post(AntigravityOAuth::TOKEN_URL)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(body.into()),
        )
        .await?;
    if !response.status.is_success() {
        let status = response.status.as_u16();
        let detail = response.text().await.unwrap_or_default().trim().to_string();
        let detail = if detail.is_empty() { format!("HTTP {status}") } else { detail };
        anyhow::bail!("Antigravity OAuth token exchange failed: {detail}");
    }
    Ok(serde_json::from_slice(&response.bytes().await?)?)
}

fn jwt_claims(access_token: &str) -> Option<serde_json::Value> {
    use base64::Engine;
    let mid = access_token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(mid).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn extract_chatgpt_account_id(access_token: &str) -> Option<String> {
    jwt_claims(access_token)?
        .get("https://api.openai.com/auth")?
        .get("chatgpt_account_id")?
        .as_str()
        .map(str::to_string)
}

pub fn extract_jwt_expiry_iso(access_token: &str) -> Option<String> {
    let exp = jwt_claims(access_token)?.get("exp")?.as_f64()?;
    Some(crate::db::accounts::iso_from_ms((exp * 1000.0) as i64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{skip_without_db, test_pool, test_state};
    use crate::upstream::MockTransport;
    use base64::Engine;
    use http::StatusCode;

    fn token(claims: serde_json::Value) -> String {
        let mid = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
        format!("header.{mid}.signature")
    }

    #[test]
    fn claude_authorize_url_carries_the_pkce_challenge() {
        let begun = begin_claude_authorization(None);
        assert!(begun.authorization_url.starts_with("https://claude.ai/oauth/authorize?"));
        assert!(begun.authorization_url.contains("code_challenge_method=S256"));
        assert!(begun.authorization_url.contains(&format!("client_id={}", ClaudeOAuth::CLIENT_ID)));
        assert!(begun.authorization_url.contains(&format!("state={}", begun.pending.oauth_state)));
        assert_eq!(begun.pending.redirect_uri, ClaudeOAuth::REDIRECT_URI);
        assert!(!begun.pending.code_verifier.is_empty());
        // An operator-configured client id overrides the public default; an empty one does not.
        assert_eq!(begin_claude_authorization(Some("custom")).pending.client_id, "custom");
        assert_eq!(begin_claude_authorization(Some("")).pending.client_id, ClaudeOAuth::CLIENT_ID);
    }

    #[test]
    fn antigravity_authorize_url_asks_for_offline_consent() {
        let begun = begin_antigravity_authorization("client-1");
        assert!(begun.authorization_url.contains("access_type=offline"));
        assert!(begun.authorization_url.contains("prompt=consent"));
        assert!(!begun.authorization_url.contains("code_challenge"), "this client has no PKCE");
        assert_eq!(begun.pending.redirect_uri, AntigravityOAuth::REDIRECT_URI);
    }

    #[test]
    fn the_antigravity_paste_accepts_a_url_or_a_bare_code() {
        assert_eq!(
            parse_antigravity_callback("http://localhost:51121/oauth-callback?code=abc&state=s1").unwrap(),
            ("abc".to_string(), Some("s1".to_string()))
        );
        assert_eq!(parse_antigravity_callback("  abc  ").unwrap(), ("abc".to_string(), None));
        assert!(parse_antigravity_callback("").is_err());
        assert!(parse_antigravity_callback("http://localhost:51121/oauth-callback?error=access_denied")
            .unwrap_err()
            .contains("access_denied"));
        assert!(parse_antigravity_callback("http://localhost:51121/oauth-callback").unwrap_err().contains("no code"));
    }

    #[test]
    fn jwt_claims_are_read_tolerantly() {
        let t = token(serde_json::json!({ "https://api.openai.com/auth": { "chatgpt_account_id": "acc-1" }, "exp": 1_700_000_000 }));
        assert_eq!(extract_chatgpt_account_id(&t).as_deref(), Some("acc-1"));
        assert_eq!(extract_jwt_expiry_iso(&t).as_deref(), Some("2023-11-14T22:13:20.000Z"));
        assert_eq!(extract_chatgpt_account_id("not-a-jwt"), None);
        assert_eq!(extract_jwt_expiry_iso("not-a-jwt"), None);
        assert_eq!(extract_jwt_expiry_iso(&token(serde_json::json!({ "exp": "soon" }))), None);
        assert_eq!(extract_chatgpt_account_id(&token(serde_json::json!({}))), None);
    }

    #[tokio::test]
    async fn the_claude_exchange_binds_state_and_posts_the_verifier() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, serde_json::json!({ "access_token": "at", "refresh_token": "rt", "expires_in": 3600 }));
        let state = test_state(pool, transport.clone());
        let begun = begin_claude_authorization(None);

        assert!(
            exchange_claude_code(&state, "code", "not-the-state", &begun.pending).await.is_err(),
            "a mismatched state never reaches the provider"
        );
        assert!(transport.requests().is_empty());

        let tokens = exchange_claude_code(&state, "code", &begun.pending.oauth_state, &begun.pending).await.unwrap();
        assert_eq!(tokens, OAuthTokens { access_token: "at".into(), refresh_token: Some("rt".into()), expires_in: Some(3600) });
        let body = transport.requests()[0].json();
        assert_eq!(body["code_verifier"], begun.pending.code_verifier);
        assert_eq!(body["grant_type"], "authorization_code");
    }

    #[tokio::test]
    async fn an_antigravity_bare_code_skips_the_state_check_but_a_wrong_state_does_not() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, serde_json::json!({ "access_token": "at" }));
        let state = test_state(pool, transport.clone());
        let begun = begin_antigravity_authorization("client-1");

        assert!(exchange_antigravity_code(&state, "code", Some("wrong"), &begun.pending, "secret").await.is_err());
        let tokens = exchange_antigravity_code(&state, "code", None, &begun.pending, "secret").await.unwrap();
        assert_eq!(tokens.access_token, "at");
        assert_eq!(tokens.refresh_token, None);
        let body = String::from_utf8(transport.requests()[0].body.clone().unwrap().to_vec()).unwrap();
        assert!(body.contains("client_secret=secret"));
        assert!(body.contains("grant_type=authorization_code"));
    }

    #[tokio::test]
    async fn the_client_pair_is_all_or_nothing() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let mut config = crate::db::test_support::test_config();
        config.antigravity_oauth_client_id = Some("id".into());
        let state = crate::AppState::builder(config, pool).transport(MockTransport::new()).build();
        assert_eq!(antigravity_oauth_client(&state), None, "an id without a secret is no client");
    }
}
