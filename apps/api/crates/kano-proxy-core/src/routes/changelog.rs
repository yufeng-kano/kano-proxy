//! `/api/changelog` — the running version plus this repo's GitHub release notes
//! (docs/product.md § Changelog, docs/admin-ui.md).
//!
//! Cache-first: one global entry (the notes are identical for every operator, and a per-user
//! key would multiply calls against GitHub's 60/hr unauthenticated budget). A failed refetch
//! degrades to the last good data marked `stale` rather than a broken page; an unset or
//! malformed `GITHUB_REPO` still serves the running version with `available: false`.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use crate::auth::session::SessionUser;
use crate::changelog::cache::{
    fetch_releases, is_changelog_fresh, is_valid_repo, read_changelog_cache, write_changelog_cache, ChangelogRelease,
};
use crate::changelog::version::is_update_available;
use crate::AppState;

/// Exposed on its own so an edition can shadow this group with its own `/api/changelog`
/// (the hosted build reports the deployed release, not this repo's).
pub fn routes() -> Router<AppState> {
    Router::new().route("/", get(changelog))
}

/// The running version: this crate's own, which is the product's. An edition that ships on a
/// different cycle overrides it through its changelog route.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

struct Body<'a> {
    latest: Option<&'a str>,
    releases: &'a [ChangelogRelease],
    available: bool,
    cached: bool,
    stale: bool,
    error: Option<&'a str>,
}

/// The shape the admin UI reads (docs/product.md § Changelog).
fn changelog_response(body: Body<'_>) -> Response {
    Json(json!({
        "current": current_version(),
        "latest": body.latest,
        "updateAvailable": is_update_available(current_version(), body.latest),
        "releases": body.releases,
        "available": body.available,
        "cached": body.cached,
        "stale": body.stale,
        "error": body.error,
    }))
    .into_response()
}

async fn changelog(
    State(state): State<AppState>,
    _session: SessionUser,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let repo = state.config().github_repo.clone().unwrap_or_default().trim().to_string();
    if !is_valid_repo(&repo) {
        // Graceful degradation: the running version is still useful in the topbar badge, so
        // report the configuration gap instead of failing the request.
        let error = if repo.is_empty() { "GITHUB_REPO is not configured" } else { "GITHUB_REPO must be owner/repo" };
        return changelog_response(Body {
            latest: None,
            releases: &[],
            available: false,
            cached: false,
            stale: false,
            error: Some(error),
        });
    }

    let refresh = params.get("refresh").map(String::as_str) == Some("true");
    let cached = read_changelog_cache(&state).await;

    // A fresh entry inside the hour window makes no upstream call at all.
    if let Some(snapshot) = cached.as_ref().filter(|s| !refresh && is_changelog_fresh(s)) {
        return changelog_response(Body {
            latest: snapshot.latest.as_deref(),
            releases: &snapshot.releases,
            available: true,
            cached: true,
            stale: false,
            error: snapshot.error.as_deref(),
        });
    }

    match fetch_releases(&state, &repo).await {
        Ok(fetched) => {
            write_changelog_cache(&state, &fetched.releases, fetched.latest.as_deref(), None).await;
            changelog_response(Body {
                latest: fetched.latest.as_deref(),
                releases: &fetched.releases,
                available: true,
                cached: false,
                stale: false,
                error: None,
            })
        }
        // Stale-serve — a deliberate deviation from the usage/models caches, which treat an
        // aged entry as a miss: stale release notes are harmless, stale usage numbers are
        // misleading. A failed refetch degrades to the last good data instead of a broken page.
        Err(error) => match cached {
            Some(snapshot) => changelog_response(Body {
                latest: snapshot.latest.as_deref(),
                releases: &snapshot.releases,
                available: true,
                cached: true,
                stale: true,
                error: Some(&error),
            }),
            // No cache to fall back on — nothing fabricated, the error says why.
            None => changelog_response(Body {
                latest: None,
                releases: &[],
                available: true,
                cached: false,
                stale: false,
                error: Some(&error),
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use crate::auth::session::create_session;
    use crate::changelog::cache::{changelog_cache_key, CachedChangelog};
    use crate::config::CoreConfig;
    use crate::db::test_support::{insert_user, skip_without_db, test_config, test_pool};
    use crate::extensions::Extensions;
    use crate::upstream::transport::TransportError;
    use crate::upstream::{MockTransport, UpstreamResponse};
    use axum::body::Body as AxumBody;
    use axum::http::{header, Request, StatusCode};
    use http_body_util::BodyExt;
    use sqlx::PgPool;
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;

    const REPO: &str = "yufeng-kano/kano-proxy";

    fn state_with(pool: PgPool, transport: Arc<MockTransport>, github_repo: Option<&str>) -> AppState {
        let config = CoreConfig { github_repo: github_repo.map(str::to_string), ..test_config() };
        AppState::builder(config, pool).transport(transport).build()
    }

    async fn body_json(response: Response) -> Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn signed_in(state: &AppState, email: &str) -> String {
        let user = insert_user(state.pool(), email).await;
        let (_, cookie) = create_session(state, &user.id, false).await.unwrap();
        cookie.split(';').next().unwrap().to_string()
    }

    async fn get(state: &AppState, cookie: &str, uri: &str) -> Response {
        crate::build_router(state.clone(), Extensions::default())
            .oneshot(Request::builder().uri(uri).header(header::COOKIE, cookie).body(AxumBody::empty()).unwrap())
            .await
            .unwrap()
    }

    fn release(overrides: Value) -> Value {
        let mut base = json!({
            "tag_name": "v1.11.0",
            "name": "v1.11.0",
            "published_at": "2026-07-30T00:00:00Z",
            "html_url": "https://github.com/yufeng-kano/kano-proxy/releases/tag/v1.11.0",
            "body_html": "<p>Release notes</p>",
            "draft": false,
            "prerelease": false,
        });
        for (k, v) in overrides.as_object().expect("overrides object") {
            base[k] = v.clone();
        }
        base
    }

    /// An entry past the one-hour freshness window, so a refetch is attempted.
    async fn seed_aged_cache(state: &AppState) {
        let snapshot = CachedChangelog {
            releases: vec![ChangelogRelease {
                tag: "v1.10.0".into(),
                name: "v1.10.0".into(),
                published_at: "2026-06-01T00:00:00Z".into(),
                url: "https://github.com/yufeng-kano/kano-proxy/releases/tag/v1.10.0".into(),
                body_html: "<p>old notes</p>".into(),
            }],
            latest: Some("v1.10.0".into()),
            error: None,
            fetched_at: crate::app::now_ms() - 2 * 60 * 60 * 1000,
        };
        state.cache().put_json(changelog_cache_key(), &snapshot, Duration::from_secs(600)).await;
    }

    #[tokio::test]
    async fn requires_a_session() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = state_with(pool, MockTransport::new(), Some(REPO));
        let response = crate::build_router(state, Extensions::default())
            .oneshot(Request::builder().uri("/api/changelog").body(AxumBody::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn returns_sanitized_releases_newest_first_and_computes_update_available() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let mock = MockTransport::new();
        // Stay ahead of the bundled package version so this survives every real SemVer bump.
        let parts: Vec<u64> = current_version().split('.').map(|p| p.parse().unwrap_or(0)).collect();
        let newer_tag = format!("v{}.{}.0", parts[0], parts.get(1).copied().unwrap_or(0) + 1);
        let older_tag = format!("v{}.{}.0", parts[0], parts.get(1).copied().unwrap_or(0));
        mock.respond_json(
            StatusCode::OK,
            json!([
                release(json!({
                    "tag_name": newer_tag,
                    "body_html": "<p>new <a href=\"javascript:alert(1)\">notes</a></p><script>bad()</script>",
                })),
                release(json!({ "tag_name": older_tag })),
            ]),
        );
        let state = state_with(pool, mock, Some(REPO));
        let cookie = signed_in(&state, "changelog@example.com").await;

        let response = get(&state, &cookie, "/api/changelog").await;
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["current"], current_version());
        assert_eq!(json["latest"], newer_tag);
        assert_eq!(json["updateAvailable"], true);
        assert_eq!(json["available"], true);
        assert_eq!(json["cached"], false);
        assert_eq!(json["stale"], false);
        assert_eq!(json["error"], Value::Null);
        assert_eq!(
            json["releases"].as_array().unwrap().iter().map(|r| r["tag"].clone()).collect::<Vec<_>>(),
            vec![json!(newer_tag), json!(older_tag)]
        );
        assert_eq!(json["releases"][0]["body_html"], "<p>new notes</p>bad()");
    }

    #[tokio::test]
    async fn serves_a_fresh_cache_entry_without_refetching_and_refresh_bypasses_it() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let mock = MockTransport::new();
        for _ in 0..4 {
            mock.expect(|_| Ok(UpstreamResponse::json(StatusCode::OK, &json!([release(json!({}))]))));
        }
        let state = state_with(pool, mock.clone(), Some(REPO));
        let cookie = signed_in(&state, "cache@example.com").await;

        let first = body_json(get(&state, &cookie, "/api/changelog").await).await;
        assert_eq!(first["cached"], false);

        let second = body_json(get(&state, &cookie, "/api/changelog").await).await;
        assert_eq!(second["cached"], true);
        assert_eq!(second["stale"], false);
        // End-to-end v-prefix check: tag v1.11.0 must not read as an update over 1.11.0.
        assert_eq!(second["latest"], "v1.11.0");
        assert_eq!(second["updateAvailable"], is_update_available(current_version(), Some("v1.11.0")));
        assert_eq!(mock.requests().len(), 1);

        let refreshed = body_json(get(&state, &cookie, "/api/changelog?refresh=true").await).await;
        assert_eq!(refreshed["cached"], false);
        assert_eq!(mock.requests().len(), 2);
    }

    #[tokio::test]
    async fn an_upstream_failure_with_a_warm_cache_serves_stale_data() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let mock = MockTransport::new();
        mock.expect(|_| Ok(UpstreamResponse::from_bytes(StatusCode::INTERNAL_SERVER_ERROR, http::HeaderMap::new(), "boom")));
        let state = state_with(pool, mock, Some(REPO));
        let cookie = signed_in(&state, "stale@example.com").await;
        seed_aged_cache(&state).await;

        let json = body_json(get(&state, &cookie, "/api/changelog").await).await;
        assert_eq!(json["releases"].as_array().unwrap().iter().map(|r| r["tag"].clone()).collect::<Vec<_>>(), vec![json!("v1.10.0")]);
        assert_eq!(json["latest"], "v1.10.0");
        assert_eq!(json["cached"], true);
        assert_eq!(json["stale"], true);
        assert_eq!(json["error"], "HTTP 500");
    }

    #[tokio::test]
    async fn a_network_failure_with_a_warm_cache_serves_stale_data() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let mock = MockTransport::new();
        mock.expect(|_| Err(TransportError::Connect("network down".into())));
        let state = state_with(pool, mock, Some(REPO));
        let cookie = signed_in(&state, "down@example.com").await;
        seed_aged_cache(&state).await;

        let json = body_json(get(&state, &cookie, "/api/changelog").await).await;
        assert_eq!(json["latest"], "v1.10.0");
        assert_eq!(json["cached"], true);
        assert_eq!(json["stale"], true);
        assert_eq!(json["error"], "unreachable/timeout");
    }

    #[tokio::test]
    async fn an_upstream_failure_with_no_cache_returns_an_empty_list_and_the_error() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let mock = MockTransport::new();
        mock.expect(|_| Ok(UpstreamResponse::from_bytes(StatusCode::INTERNAL_SERVER_ERROR, http::HeaderMap::new(), "boom")));
        let state = state_with(pool, mock, Some(REPO));
        let cookie = signed_in(&state, "cold@example.com").await;

        let json = body_json(get(&state, &cookie, "/api/changelog").await).await;
        assert_eq!(json["available"], true);
        assert_eq!(json["releases"], json!([]));
        assert_eq!(json["latest"], Value::Null);
        assert_eq!(json["updateAvailable"], false);
        assert_eq!(json["cached"], false);
        assert_eq!(json["stale"], false);
        assert_eq!(json["error"], "HTTP 500");
    }

    #[tokio::test]
    async fn an_unset_blank_or_malformed_repo_degrades_without_fetching() {
        for (repo, expected) in [
            (None, "GITHUB_REPO is not configured"),
            (Some(""), "GITHUB_REPO is not configured"),
            (Some("evil/../../etc"), "GITHUB_REPO must be owner/repo"),
        ] {
            let Some(pool) = test_pool().await else { return skip_without_db() };
            let mock = MockTransport::new();
            let state = state_with(pool, mock.clone(), repo);
            let cookie = signed_in(&state, "repo@example.com").await;
            let response = get(&state, &cookie, "/api/changelog").await;
            assert_eq!(response.status(), StatusCode::OK);
            let json = body_json(response).await;
            assert_eq!(json["current"], current_version(), "the running version is still served");
            assert_eq!(json["available"], false);
            assert_eq!(json["releases"], json!([]));
            assert_eq!(json["updateAvailable"], false);
            assert_eq!(json["error"], expected);
            assert!(mock.requests().is_empty(), "an unusable repo never reaches the network");
        }
    }

    #[tokio::test]
    async fn drafts_prereleases_and_cli_tags_never_become_latest() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let mock = MockTransport::new();
        mock.respond_json(
            StatusCode::OK,
            json!([
                release(json!({ "tag_name": "v1.11.0", "draft": true })),
                release(json!({ "tag_name": "v1.10.5", "prerelease": true })),
                release(json!({ "tag_name": "cli-v1.2.0", "name": "cli-v1.2.0" })),
                release(json!({ "tag_name": "v1.10.0" })),
                release(json!({ "tag_name": "v1.9.0" })),
            ]),
        );
        let state = state_with(pool, mock, Some(REPO));
        let cookie = signed_in(&state, "filters@example.com").await;

        let json = body_json(get(&state, &cookie, "/api/changelog").await).await;
        assert_eq!(
            json["releases"].as_array().unwrap().iter().map(|r| r["tag"].clone()).collect::<Vec<_>>(),
            vec![json!("v1.10.0"), json!("v1.9.0")]
        );
        assert_eq!(json["latest"], "v1.10.0");
    }

    #[test]
    fn the_current_version_comes_from_the_root_package_json() {
        // Same value the TypeScript route imported; three dot-separated numbers.
        let parts: Vec<&str> = current_version().split('.').collect();
        assert_eq!(parts.len(), 3, "{}", current_version());
        assert!(parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())), "{}", current_version());
    }
}
