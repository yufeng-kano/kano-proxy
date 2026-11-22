//! Admin sessions (docs/auth.md). The cookie is
//! `kano-proxy_session=<id>.<hex HMAC-SHA256(SESSION_SECRET, id)>`; the signature proves the
//! id was issued here and the `sessions` row carries the 14-day expiry. There is no password
//! login — a session only ever comes from Google OIDC.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::auth::notice::notice_required_response;
use crate::crypto::session::{clear_session_cookie, session_cookie, unverified_session_id, verified_session_id};
use crate::db::sessions;
use crate::db::users::UserRow;
use crate::AppState;

/// The signed-in admin, loaded once per request by the session extractors and kept in the
/// request extensions.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionUser {
    pub user: UserRow,
}

/// The admin surfaces answer an unauthenticated request with a bare
/// `{"error":"unauthorized"}` and 401 — matched exactly, since the SPA keys on it.
pub fn unauthorized_response() -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({ "error": "unauthorized" }))).into_response()
}

fn cookie_header(parts: &Parts) -> Option<&str> {
    parts.headers.get(header::COOKIE).and_then(|v| v.to_str().ok())
}

/// `true` when this request arrived over HTTPS, so the session cookie gets `Secure`. Behind a
/// proxy the scheme survives in `x-forwarded-proto`; a direct plaintext dev server has
/// neither and gets no `Secure` (which would make the cookie unusable on http://localhost).
pub fn is_secure_request(parts: &Parts) -> bool {
    if parts.uri.scheme_str() == Some("https") {
        return true;
    }
    parts
        .headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').next().is_some_and(|s| s.trim().eq_ignore_ascii_case("https")))
}

/// Verifies the cookie signature, then reads the session row with its user and checks the
/// expiry. An expired session is deleted on the way out so a stale cookie stops costing a read.
pub async fn load_session_user(state: &AppState, cookie: Option<&str>) -> Option<SessionUser> {
    let secret = state.config().session_secret.as_deref()?;
    let session_id = verified_session_id(secret, cookie?)?;
    let (user, expires_at) = sessions::get_session_user(state.pool(), &session_id).await.ok().flatten()?;
    if crate::db::accounts::parse_iso_ms(&expires_at).is_none_or(|at| at < state.now_ms()) {
        let _ = sessions::delete_session(state.pool(), &session_id).await;
        return None;
    }
    Some(SessionUser { user })
}

/// Issues a session row and its `Set-Cookie` value. Fails when `SESSION_SECRET` is unset —
/// signing with a default would make every deploy's cookies interchangeable.
pub async fn create_session(state: &AppState, user_id: &str, secure: bool) -> anyhow::Result<(String, String)> {
    let secret = state
        .config()
        .session_secret
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("SESSION_SECRET is not configured"))?;
    let expires = sessions::expiry_from(state.now_ms());
    let id = sessions::insert_session(state.pool(), user_id, &expires).await?;
    let cookie = session_cookie(secret, &id, secure);
    Ok((id, cookie))
}

pub async fn destroy_session(state: &AppState, session_id: &str) -> Result<(), sqlx::Error> {
    sessions::delete_session(state.pool(), session_id).await.map(|_| ())
}

/// The logout path's id: unverified on purpose, because deleting a row named by an
/// unforgeable-but-unsigned id is harmless and a user with a corrupted cookie must still be
/// able to log out.
pub fn cookie_session_id(parts: &Parts) -> Option<String> {
    unverified_session_id(cookie_header(parts)?).map(str::to_string)
}

pub fn logout_cookie(secure: bool) -> String {
    clear_session_cookie(secure)
}

/// The signed-in user whatever their usage-notice state. The first extractor in a request
/// loads the session and leaves it in the extensions; any later one reads it from there.
async fn signed_in_user(parts: &mut Parts, state: &AppState) -> Option<SessionUser> {
    if let Some(found) = parts.extensions.get::<SessionUser>() {
        return Some(found.clone());
    }
    let loaded = load_session_user(state, cookie_header(parts)).await?;
    parts.extensions.insert(loaded.clone());
    Some(loaded)
}

/// Extractor for a route that requires a signed-in admin who accepted the current usage notice
/// (docs/auth.md § Usage notice). Rejects with the admin 401 body without a session, and with
/// `403 notice_required` before acceptance.
impl FromRequestParts<AppState> for SessionUser {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let found = signed_in_user(parts, state).await.ok_or_else(unauthorized_response)?;
        if !state.notice().accepted_by(&found.user) {
            return Err(notice_required_response());
        }
        Ok(found)
    }
}

/// A signed-in admin who may not have accepted the usage notice yet. Only for the route that
/// records the acceptance (`POST /api/auth/notice`); everything else takes [`SessionUser`].
#[derive(Debug, Clone, PartialEq)]
pub struct UngatedSessionUser(pub SessionUser);

impl FromRequestParts<AppState> for UngatedSessionUser {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        signed_in_user(parts, state).await.map(UngatedSessionUser).ok_or_else(unauthorized_response)
    }
}

/// Extractor that never rejects — for surfaces that render differently when signed in
/// (`GET /api/auth/me`). Ungated: `me` is where the SPA learns the notice is due.
#[derive(Debug, Clone, PartialEq)]
pub struct MaybeSessionUser(pub Option<SessionUser>);

impl FromRequestParts<AppState> for MaybeSessionUser {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        Ok(MaybeSessionUser(signed_in_user(parts, state).await))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::session::COOKIE_NAME;
    use crate::db::test_support::{insert_user, skip_without_db, test_pool, test_state};
    use crate::upstream::MockTransport;

    fn parts_with(cookie: Option<&str>) -> Parts {
        let mut builder = axum::http::Request::builder().uri("/api/keys");
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        builder.body(axum::body::Body::empty()).unwrap().into_parts().0
    }

    #[test]
    fn secure_follows_the_scheme_and_the_forwarded_header() {
        assert!(!is_secure_request(&parts_with(None)));
        let parts = axum::http::Request::builder()
            .uri("/api/keys")
            .header("x-forwarded-proto", "https, http")
            .body(axum::body::Body::empty())
            .unwrap()
            .into_parts()
            .0;
        assert!(is_secure_request(&parts));
        let parts = axum::http::Request::builder()
            .uri("https://app.example.com/api/keys")
            .body(axum::body::Body::empty())
            .unwrap()
            .into_parts()
            .0;
        assert!(is_secure_request(&parts));
    }

    #[tokio::test]
    async fn a_session_round_trips_through_its_cookie() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "sess@example.com").await;
        let (id, cookie) = create_session(&state, &user.id, false).await.unwrap();
        assert!(id.starts_with("sess_"));
        assert!(cookie.starts_with(&format!("{COOKIE_NAME}={id}.")));
        assert!(!cookie.contains("Secure"));
        assert!(create_session(&state, &user.id, true).await.unwrap().1.ends_with("; Secure"));

        let header = cookie.split(';').next().unwrap().to_string();
        let loaded = load_session_user(&state, Some(&header)).await.unwrap();
        assert_eq!(loaded.user.id, user.id);

        // A tampered signature, a foreign secret, no signature and no cookie are all "no session".
        let tampered = format!("{header}0");
        assert!(load_session_user(&state, Some(&tampered)).await.is_none());
        let foreign = crate::crypto::session::session_cookie("not-this-deploys-secret", &id, false);
        assert!(load_session_user(&state, foreign.split(';').next()).await.is_none());
        assert!(load_session_user(&state, Some(&format!("{COOKIE_NAME}={id}"))).await.is_none());
        assert!(load_session_user(&state, None).await.is_none());

        destroy_session(&state, &id).await.unwrap();
        assert!(load_session_user(&state, Some(&header)).await.is_none(), "a destroyed session is gone");
    }

    #[tokio::test]
    async fn an_expired_session_is_rejected_and_deleted() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "expired@example.com").await;
        let (id, cookie) = create_session(&state, &user.id, false).await.unwrap();
        sqlx::query("UPDATE sessions SET expires_at = $1 WHERE id = $2")
            .bind("2020-01-01T00:00:00.000Z")
            .bind(&id)
            .execute(state.pool())
            .await
            .unwrap();
        let header = cookie.split(';').next().unwrap().to_string();
        assert!(load_session_user(&state, Some(&header)).await.is_none());
        assert!(crate::db::sessions::get_session(state.pool(), &id).await.unwrap().is_none());
    }
}
