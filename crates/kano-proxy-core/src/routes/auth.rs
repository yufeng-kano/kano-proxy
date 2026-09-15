//! `/api/auth` — Google sign-in, sign-out and "who am I" (apps/api/src/routes/auth.ts,
//! docs/auth.md). The callback sets the session cookie and sends the browser back to the SPA
//! root, so the SPA's own boot restores the view the user was last on.

use axum::extract::{Query, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::auth::google::{begin_google_login, complete_google_login};
use crate::auth::session::{
    cookie_session_id, create_session, destroy_session, is_secure_request, logout_cookie, MaybeSessionUser,
};
use crate::db::users::upsert_google_user;
use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/login", get(login))
        .route("/callback", get(callback))
        .route("/logout", post(logout))
        .route("/me", get(me))
}

async fn login(State(state): State<AppState>) -> Response {
    match begin_google_login(&state).await {
        Ok(login) => match HeaderValue::from_str(&login.url) {
            Ok(location) => (StatusCode::FOUND, [(header::LOCATION, location)]).into_response(),
            Err(_) => server_error("login failed"),
        },
        Err(err) => server_error(&err.to_string()),
    }
}

fn server_error(message: &str) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": message }))).into_response()
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
}

async fn callback(State(app): State<AppState>, parts: Parts, Query(query): Query<CallbackQuery>) -> Response {
    let (Some(code), Some(login_state)) = (query.code, query.state) else {
        return (StatusCode::BAD_REQUEST, "Missing code/state").into_response();
    };
    let secure = is_secure_request(&parts);
    let result = async {
        let profile = complete_google_login(&app, &code, &login_state).await?;
        let user = upsert_google_user(app.pool(), &profile).await?;
        create_session(&app, &user.id, secure).await
    }
    .await;
    match result {
        // The admin SPA lives on APP_URL (Vite locally, the built site in production); never
        // leave the user on the API root. A bare "/" rather than a page path, so the SPA
        // decides where to land (docs/admin-ui.md § View preferences).
        Ok((_, cookie)) => {
            let location = format!("{}/", app.config().app_url.trim_end_matches('/'));
            match (HeaderValue::from_str(&location), HeaderValue::from_str(&cookie)) {
                (Ok(location), Ok(cookie)) => {
                    (StatusCode::FOUND, [(header::LOCATION, location), (header::SET_COOKIE, cookie)]).into_response()
                }
                _ => (StatusCode::BAD_REQUEST, "callback failed").into_response(),
            }
        }
        Err(err) => (StatusCode::BAD_REQUEST, err.to_string()).into_response(),
    }
}

async fn logout(State(state): State<AppState>, parts: Parts) -> Response {
    if let Some(session_id) = cookie_session_id(&parts) {
        if let Err(err) = destroy_session(&state, &session_id).await {
            tracing::warn!(error = %err, "session row not deleted on logout");
        }
    }
    // The cookie is cleared whatever the row did: a browser must never keep a session cookie
    // it asked to drop.
    let cookie = HeaderValue::from_str(&logout_cookie(is_secure_request(&parts))).expect("cookie is ASCII");
    ([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response()
}

async fn me(MaybeSessionUser(session): MaybeSessionUser) -> Response {
    match session {
        // Only the profile fields the SPA renders — never the google_sub or timestamps.
        Some(found) => Json(json!({
            "user": {
                "id": found.user.id,
                "email": found.user.email,
                "name": found.user.name,
                "picture_url": found.user.picture_url,
            }
        }))
        .into_response(),
        None => (StatusCode::UNAUTHORIZED, Json(json!({ "user": null }))).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::session::COOKIE_NAME;
    use crate::db::test_support::{insert_user, skip_without_db, test_pool, test_router, test_state};
    use crate::upstream::MockTransport;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn body_json(response: Response) -> serde_json::Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn login_redirects_to_google() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let response = test_router(state)
            .oneshot(Request::builder().uri("/api/auth/login").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        let location = response.headers().get(header::LOCATION).unwrap().to_str().unwrap();
        assert!(location.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
    }

    #[tokio::test]
    async fn the_callback_sets_a_session_and_returns_to_the_app() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let transport = MockTransport::new();
        transport.respond_json(StatusCode::OK, json!({ "access_token": "at" }));
        transport.respond_json(StatusCode::OK, json!({ "sub": "sub-1", "email": "a@example.com", "name": "A" }));
        let state = test_state(pool, transport);
        let login = begin_google_login(&state).await.unwrap();

        let response = test_router(state.clone())
            .oneshot(
                Request::builder()
                    .uri(format!("/api/auth/callback?code=the-code&state={}", login.state))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(response.headers().get(header::LOCATION).unwrap(), "https://app.example.com/");
        let cookie = response.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().to_string();
        assert!(cookie.starts_with(&format!("{COOKIE_NAME}=sess_")));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Lax"));

        // The session the cookie names really signs the new user in.
        let response = test_router(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/auth/me")
                    .header(header::COOKIE, cookie.split(';').next().unwrap())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["user"]["email"], "a@example.com");
        assert_eq!(json["user"]["name"], "A");
        assert!(json["user"].get("google_sub").is_none(), "the sign-in identity is not exposed");
    }

    #[tokio::test]
    async fn a_callback_without_code_or_state_is_rejected() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let response = test_router(state.clone())
            .oneshot(Request::builder().uri("/api/auth/callback?code=only").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        // An unknown state is a 400 too, and never reaches Google.
        let response = test_router(state)
            .oneshot(Request::builder().uri("/api/auth/callback?code=c&state=gstate_nope").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// The TypeScript "constructing two apps must not leak" contract (apps/api/tests/
    /// application.test.ts): extensions and state are composition-time, never a module-level
    /// registry, so two routers in one process share nothing.
    #[tokio::test]
    async fn two_apps_built_in_one_process_share_no_state() {
        let Some(pool_a) = test_pool().await else { return skip_without_db() };
        let Some(pool_b) = test_pool().await else { return skip_without_db() };
        // `service_name` is part of the app's own state, set at composition time.
        let state_a = AppState::builder(crate::db::test_support::test_config(), pool_a)
            .transport(MockTransport::new())
            .service_name("kano-proxy-hosted")
            .build();
        let state_b = test_state(pool_b, MockTransport::new());

        // Only the first app gets an edition route, and only it serves it.
        let hosted = crate::build_router(
            state_a.clone(),
            crate::Extensions {
                routes: Some(Router::new().route("/api/edition", get(|| async { Json(json!({ "hosted": true })) }))),
                ..Default::default()
            },
        );
        let standalone = test_router(state_b.clone());
        let edition = || Request::builder().uri("/api/edition").body(Body::empty()).unwrap();
        assert_eq!(hosted.clone().oneshot(edition()).await.unwrap().status(), StatusCode::OK);
        assert_eq!(standalone.clone().oneshot(edition()).await.unwrap().status(), StatusCode::NOT_FOUND);

        // `service_name` is per app, not global.
        let health = || Request::builder().uri("/health").body(Body::empty()).unwrap();
        let reported = |v: serde_json::Value| v["service"].as_str().unwrap().to_string();
        assert_eq!(reported(body_json(hosted.clone().oneshot(health()).await.unwrap()).await), "kano-proxy-hosted");
        assert_eq!(reported(body_json(standalone.clone().oneshot(health()).await.unwrap()).await), "kano-proxy");

        // A session minted against one app's storage signs nobody in on the other.
        let user = insert_user(state_a.pool(), "shared@example.com").await;
        let (_, cookie) = create_session(&state_a, &user.id, false).await.unwrap();
        let header_value = cookie.split(';').next().unwrap().to_string();
        let me = || Request::builder().uri("/api/auth/me").header(header::COOKIE, &header_value).body(Body::empty()).unwrap();
        assert_eq!(hosted.oneshot(me()).await.unwrap().status(), StatusCode::OK);
        assert_eq!(standalone.oneshot(me()).await.unwrap().status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn me_is_401_without_a_session_and_logout_clears_the_cookie() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "logout@example.com").await;
        let (session_id, cookie) = create_session(&state, &user.id, false).await.unwrap();
        let header_value = cookie.split(';').next().unwrap().to_string();

        let response = test_router(state.clone())
            .oneshot(Request::builder().uri("/api/auth/me").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(response).await, json!({ "user": null }));

        let response = test_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/logout")
                    .header(header::COOKIE, &header_value)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().contains("Max-Age=0"));
        assert_eq!(body_json(response).await, json!({ "ok": true }));
        assert!(crate::db::sessions::get_session(state.pool(), &session_id).await.unwrap().is_none());

        // The old cookie no longer signs anyone in.
        let response = test_router(state)
            .oneshot(Request::builder().uri("/api/auth/me").header(header::COOKIE, &header_value).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
