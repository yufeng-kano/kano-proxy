//! CORS for the admin surfaces (apps/api/src/application.ts). `/api/*` is the cookie-
//! credentialed admin SPA, so the allowed origin is locked to `APP_URL`: a request from any
//! other origin gets no `Access-Control-Allow-Origin` header at all and the browser refuses
//! the response. The LLM surfaces use a plain permissive CORS instead — they authenticate
//! with a project API key and must never be credentialed.

use axum::http::{header, HeaderName, HeaderValue, Method};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::set_header::SetResponseHeaderLayer;

/// `https://app.example.com` for `https://app.example.com/` — origin comparison ignores the
/// path and any trailing slash, as `new URL(...).origin` does.
pub fn origin_of(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim_end_matches('/')).ok()?;
    let host = parsed.host_str()?;
    Some(match parsed.port() {
        Some(port) => format!("{}://{host}:{port}", parsed.scheme()),
        None => format!("{}://{host}", parsed.scheme()),
    })
}

/// True when `origin` is exactly the app's own origin.
pub fn is_app_origin(app_url: &str, origin: &str) -> bool {
    match (origin_of(app_url), origin_of(origin)) {
        (Some(app), Some(other)) => app == other,
        _ => false,
    }
}

/// The `/api/*` layer: credentials allowed, origin locked to `APP_URL`. A non-matching
/// `Origin` is answered without any `Access-Control-Allow-Origin`.
pub fn admin_cors(app_url: &str) -> CorsLayer {
    let app_url = app_url.to_string();
    CorsLayer::new()
        .allow_credentials(true)
        .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::PUT, Method::DELETE, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION, HeaderName::from_static("x-api-key")])
        .allow_origin(AllowOrigin::predicate(move |origin: &HeaderValue, _parts: &_| {
            origin.to_str().is_ok_and(|origin| is_app_origin(&app_url, origin))
        }))
}

/// Admin JSON is per-user and must not be held by a shared cache. The TypeScript relied on
/// Hono's default (no `Cache-Control` at all); the header is set here because an admin
/// response served from an intermediary cache would be a cross-account leak. The admin UI's
/// own cache-first rules live in the client (docs/admin-ui.md) and are unaffected.
pub fn no_store_layer() -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::overriding(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_compare_by_scheme_host_and_port() {
        assert_eq!(origin_of("https://app.example.com/"), Some("https://app.example.com".into()));
        assert_eq!(origin_of("http://127.0.0.1:5173"), Some("http://127.0.0.1:5173".into()));
        assert_eq!(origin_of("not a url"), None);
        assert_eq!(origin_of(""), None);

        assert!(is_app_origin("https://app.example.com/", "https://app.example.com"));
        assert!(is_app_origin("https://app.example.com", "https://app.example.com/"));
        assert!(is_app_origin("http://127.0.0.1:5173", "http://127.0.0.1:5173"));
        // A different host, scheme, port or a null origin never matches.
        assert!(!is_app_origin("https://app.example.com", "https://evil.example.com"));
        assert!(!is_app_origin("https://app.example.com", "http://app.example.com"));
        assert!(!is_app_origin("http://127.0.0.1:5173", "http://127.0.0.1:5174"));
        assert!(!is_app_origin("https://app.example.com", "null"));
        assert!(!is_app_origin("", "https://app.example.com"));
    }
}
