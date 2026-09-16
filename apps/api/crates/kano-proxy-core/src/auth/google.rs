//! Google OIDC sign-in (docs/auth.md). Admin sign-in is Google
//! only: the proxy starts an authorization-code flow bound to a stored one-shot state row,
//! exchanges the code server-side and reads the profile from the userinfo endpoint. The
//! client secret never leaves the process and no password login exists.

use serde::Deserialize;

use crate::db::accounts::iso_from_ms;
use crate::db::oauth_states::{self, LOGIN_STATE_TTL_MS};
use crate::db::users::GoogleProfile;
use crate::ids::new_id;
use crate::upstream::UpstreamRequest;
use crate::AppState;

const GOOGLE_AUTH: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_USERINFO: &str = "https://openidconnect.googleapis.com/v1/userinfo";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleLogin {
    pub url: String,
    pub state: String,
}

fn form(pairs: &[(&str, &str)]) -> String {
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        ser.append_pair(k, v);
    }
    ser.finish()
}

/// Stores a one-shot state row and returns the authorization URL to redirect to.
/// `prompt=select_account` so a shared browser can switch accounts; `access_type=online`
/// because the proxy never needs a Google refresh token for sign-in.
pub async fn begin_google_login(state: &AppState) -> anyhow::Result<GoogleLogin> {
    let client_id = state
        .config()
        .google_client_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("GOOGLE_CLIENT_ID is not configured"))?;
    let id = new_id("gstate");
    let expires = iso_from_ms(state.now_ms() + LOGIN_STATE_TTL_MS);
    oauth_states::insert_google_state(state.pool(), &id, &expires).await?;
    let query = form(&[
        ("client_id", client_id),
        ("redirect_uri", &state.config().google_redirect_uri),
        ("response_type", "code"),
        ("scope", "openid email profile"),
        ("state", &id),
        ("access_type", "online"),
        ("prompt", "select_account"),
    ]);
    Ok(GoogleLogin { url: format!("{GOOGLE_AUTH}?{query}"), state: id })
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(Debug, Deserialize)]
struct UserInfo {
    #[serde(default)]
    sub: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    picture: Option<String>,
}

/// Consumes the state row, exchanges the code and returns the Google profile. The state row
/// is deleted before the exchange so a replayed callback cannot mint a second session, and an
/// expired state is refused rather than trusted.
pub async fn complete_google_login(state: &AppState, code: &str, login_state: &str) -> anyhow::Result<GoogleProfile> {
    let row = oauth_states::get_google_state(state.pool(), login_state).await?;
    let fresh = row
        .as_ref()
        .and_then(|r| crate::db::accounts::parse_iso_ms(&r.expires_at))
        .is_some_and(|at| at >= state.now_ms());
    if !fresh {
        anyhow::bail!("Invalid or expired OAuth state");
    }
    oauth_states::delete_state(state.pool(), login_state).await?;

    let (Some(client_id), Some(client_secret)) =
        (state.config().google_client_id.as_deref(), state.config().google_client_secret.as_deref())
    else {
        anyhow::bail!("Google OAuth is not configured");
    };

    let body = form(&[
        ("code", code),
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("redirect_uri", &state.config().google_redirect_uri),
        ("grant_type", "authorization_code"),
    ]);
    let token_res = state
        .transport()
        .send(
            UpstreamRequest::post(GOOGLE_TOKEN)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(body.into()),
        )
        .await?;
    if !token_res.status.is_success() {
        anyhow::bail!("Google token exchange failed: {}", token_res.status.as_u16());
    }
    let token: TokenResponse = serde_json::from_slice(&token_res.bytes().await?)?;

    let profile_res = state
        .transport()
        .send(UpstreamRequest::get(GOOGLE_USERINFO).header("authorization", &format!("Bearer {}", token.access_token)))
        .await?;
    if !profile_res.status.is_success() {
        anyhow::bail!("Google userinfo failed: {}", profile_res.status.as_u16());
    }
    let info: UserInfo = serde_json::from_slice(&profile_res.bytes().await?)?;
    if info.sub.is_empty() || info.email.is_empty() {
        anyhow::bail!("Google profile missing sub/email");
    }
    Ok(GoogleProfile { sub: info.sub, email: info.email, name: info.name, picture: info.picture })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{skip_without_db, test_pool, test_state};
    use crate::upstream::{MockTransport, UpstreamResponse};
    use http::StatusCode;

    #[tokio::test]
    async fn begin_stores_a_state_row_and_builds_the_authorize_url() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let login = begin_google_login(&state).await.unwrap();
        assert!(login.state.starts_with("gstate_"));
        assert!(login.url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        for expected in [
            "client_id=test-google-client",
            "response_type=code",
            "scope=openid+email+profile",
            "access_type=online",
            "prompt=select_account",
            "redirect_uri=https%3A%2F%2Fapi.example.com%2Fapi%2Fauth%2Fcallback",
        ] {
            assert!(login.url.contains(expected), "missing {expected} in {}", login.url);
        }
        assert!(login.url.contains(&format!("state={}", login.state)));
        assert!(oauth_states::get_google_state(state.pool(), &login.state).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn complete_exchanges_the_code_and_consumes_the_state() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, serde_json::json!({ "access_token": "google-access" }));
        transport.respond_json(
            StatusCode::OK,
            serde_json::json!({ "sub": "sub-1", "email": "a@example.com", "name": "A", "picture": "p" }),
        );
        let state = test_state(pool, transport.clone());
        let login = begin_google_login(&state).await.unwrap();
        let profile = complete_google_login(&state, "the-code", &login.state).await.unwrap();
        assert_eq!(profile, GoogleProfile { sub: "sub-1".into(), email: "a@example.com".into(), name: Some("A".into()), picture: Some("p".into()) });

        let requests = transport.requests();
        assert_eq!(requests[0].url, GOOGLE_TOKEN);
        let body = String::from_utf8(requests[0].body.clone().unwrap().to_vec()).unwrap();
        assert!(body.contains("code=the-code"));
        assert!(body.contains("grant_type=authorization_code"));
        assert!(body.contains("client_secret=test-google-secret"));
        assert_eq!(requests[1].url, GOOGLE_USERINFO);
        assert_eq!(requests[1].header("authorization"), Some("Bearer google-access"));

        // The state is one-shot: a replayed callback finds nothing.
        assert!(complete_google_login(&state, "the-code", &login.state).await.is_err());
    }

    #[tokio::test]
    async fn an_unknown_or_expired_state_never_reaches_google() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        let state = test_state(pool, transport.clone());
        assert!(complete_google_login(&state, "code", "gstate_nope").await.is_err());

        let login = begin_google_login(&state).await.unwrap();
        sqlx::query("UPDATE oauth_login_states SET expires_at = $1 WHERE id = $2")
            .bind("2020-01-01T00:00:00.000Z")
            .bind(&login.state)
            .execute(state.pool())
            .await
            .unwrap();
        assert!(complete_google_login(&state, "code", &login.state).await.is_err());
        assert!(transport.requests().is_empty(), "no upstream call for a bad state");
    }

    #[tokio::test]
    async fn upstream_failures_and_a_profile_without_a_sub_are_errors() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.expect(|_| Ok(UpstreamResponse::json(StatusCode::BAD_REQUEST, &serde_json::json!({ "error": "invalid_grant" }))));
        transport.respond_json(StatusCode::OK, serde_json::json!({ "access_token": "t" }));
        transport.respond_json(StatusCode::OK, serde_json::json!({ "email": "a@example.com" }));
        let state = test_state(pool, transport);
        let first = begin_google_login(&state).await.unwrap();
        let err = complete_google_login(&state, "code", &first.state).await.unwrap_err();
        assert!(err.to_string().contains("token exchange failed"));

        let second = begin_google_login(&state).await.unwrap();
        let err = complete_google_login(&state, "code", &second.state).await.unwrap_err();
        assert!(err.to_string().contains("missing sub/email"));
    }
}
