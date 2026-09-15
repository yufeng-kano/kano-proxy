//! Admin sessions (apps/api/src/auth/session.ts, docs/auth.md). The cookie is
//! `kano-proxy_session=<id>.<hex HMAC-SHA256(SESSION_SECRET, id)>`; the signature proves the
//! id was issued here and the `sessions` row carries the 14-day expiry. There is no password
//! login — a session only ever comes from Google OIDC.

use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::crypto::session::{clear_session_cookie, session_cookie, unverified_session_id, verified_session_id};
use crate::db::sessions;
use crate::db::users::{find_user_by_id, UserRow};
use crate::AppState;

/// The signed-in admin, put into the request extensions by [`session_middleware`] and taken
/// out by the [`SessionUser`] extractor.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionUser {
    pub user: UserRow,
    pub session_id: String,
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

/// Verifies the cookie signature, then the row and its expiry, then loads the user. An
/// expired session is deleted on the way out so a stale cookie stops costing a read.
pub async fn load_session_user(state: &AppState, cookie: Option<&str>) -> Option<SessionUser> {
    let secret = state.config().session_secret.as_deref()?;
    let session_id = verified_session_id(secret, cookie?)?;
    let row = sessions::get_session(state.pool(), &session_id).await.ok().flatten()?;
    let expires = crate::db::accounts::parse_iso_ms(&row.expires_at);
    if expires.is_none_or(|at| at < state.now_ms()) {
        let _ = sessions::delete_session(state.pool(), &session_id).await;
        return None;
    }
    let user = find_user_by_id(state.pool(), &row.user_id).await.ok().flatten()?;
    Some(SessionUser { user, session_id })
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
/// able to log out (`getCookieSessionId`).
pub fn cookie_session_id(parts: &Parts) -> Option<String> {
    unverified_session_id(cookie_header(parts)?).map(str::to_string)
}

pub fn logout_cookie(secure: bool) -> String {
    clear_session_cookie(secure)
}

/// Loads the session once per request and puts it in the extensions, the way the TypeScript
/// application factory set `c.set("user", …)` for every route. Handlers that require a
/// session use the [`SessionUser`] extractor, which reads what this left behind.
pub async fn session_middleware(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let cookie = req.headers().get(header::COOKIE).and_then(|v| v.to_str().ok()).map(str::to_string);
    if let Some(loaded) = load_session_user(&state, cookie.as_deref()).await {
        req.extensions_mut().insert(loaded);
    }
    next.run(req).await
}

/// Extractor for a route that requires a signed-in admin. Rejects with the admin 401 body.
/// Works with or without [`session_middleware`] in front: it reads the extension when one is
/// there and otherwise loads the session itself.
impl FromRequestParts<AppState> for SessionUser {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        if let Some(found) = parts.extensions.get::<SessionUser>() {
            return Ok(found.clone());
        }
        match load_session_user(state, cookie_header(parts)).await {
            Some(loaded) => {
                parts.extensions.insert(loaded.clone());
                Ok(loaded)
            }
            None => Err(unauthorized_response()),
        }
    }
}

/// Extractor that never rejects — for surfaces that render differently when signed in
/// (`GET /api/auth/me`).
#[derive(Debug, Clone, PartialEq)]
pub struct MaybeSessionUser(pub Option<SessionUser>);

impl FromRequestParts<AppState> for MaybeSessionUser {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        Ok(MaybeSessionUser(SessionUser::from_request_parts(parts, state).await.ok()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::session::COOKIE_NAME;
    use crate::db::test_support::{insert_user, skip_without_db, test_pool, test_state, TEST_SESSION_SECRET};
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
        assert_eq!(loaded.session_id, id);

        // A tampered signature, a foreign secret and a missing cookie are all "no session".
        let tampered = format!("{header}0");
        assert!(load_session_user(&state, Some(&tampered)).await.is_none());
        assert!(load_session_user(&state, Some(&format!("{COOKIE_NAME}={id}"))).await.is_none());
        assert!(load_session_user(&state, None).await.is_none());
        assert_ne!(TEST_SESSION_SECRET, "");

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

    #[test]
    fn the_unauthorized_body_is_the_admin_shape() {
        let response = unauthorized_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
