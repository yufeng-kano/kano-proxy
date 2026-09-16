//! Release-notes cache and the GitHub Releases fetch.
//!
//! Two knobs, deliberately different from the 1h catalog cache:
//!   - entries live 7 days, so a failed refetch always has something to fall back on
//!   - a refetch is only attempted once the entry is an hour old
//!
//! The key is **global** — no user id, unlike the user-scoped catalog cache. Release notes are
//! identical for every operator, and a per-user key would multiply GitHub calls by the number
//! of signed-in users against a 60/hr unauthenticated budget.

use std::time::Duration;

use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::app::now_ms;
use crate::cache::Cache;
use crate::changelog::sanitize::sanitize_release_html;
use crate::changelog::version::parse_semver;
use crate::upstream::{UpstreamRequest, UpstreamTransport};
use crate::AppState;

pub const CHANGELOG_CACHE_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;
pub const CHANGELOG_FRESH_MS: i64 = 60 * 60 * 1000;
/// Wait for GitHub's response headers, matching the Worker's 10s abort.
const GITHUB_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangelogRelease {
    pub tag: String,
    pub name: String,
    pub published_at: String,
    pub url: String,
    /// Already sanitized by [`sanitize_release_html`] before it reaches the cache.
    pub body_html: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedChangelog {
    pub releases: Vec<ChangelogRelease>,
    pub latest: Option<String>,
    pub error: Option<String>,
    #[serde(rename = "fetchedAt")]
    pub fetched_at: i64,
}

pub fn changelog_cache_key() -> &'static str {
    "changelog:v1"
}

/// `owner/repo`, both halves URL-safe — the value is interpolated straight into the fetch URL,
/// so anything outside this shape is rejected before it gets there (no path traversal, no URL
/// injection).
static REPO_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[\w.-]+/[\w.-]+$").expect("repo regex"));

pub fn is_valid_repo(repo: &str) -> bool {
    REPO_RE.is_match(repo)
}

/// Returns the entry even when it is past the freshness window — the caller decides whether
/// to refetch, and needs the old value to fall back on when that refetch fails. (The
/// per-account caches discard an aged entry here; doing that would make stale-serve
/// impossible.)
pub async fn read_changelog_cache(state: &AppState) -> Option<CachedChangelog> {
    read_changelog_cache_with(state.cache()).await
}

pub async fn read_changelog_cache_with(cache: &Cache) -> Option<CachedChangelog> {
    cache.get_json::<CachedChangelog>(changelog_cache_key()).await
}

pub fn is_changelog_fresh(snap: &CachedChangelog) -> bool {
    now_ms() - snap.fetched_at < CHANGELOG_FRESH_MS
}

pub async fn write_changelog_cache(
    state: &AppState,
    releases: &[ChangelogRelease],
    latest: Option<&str>,
    error: Option<&str>,
) {
    write_changelog_cache_with(state.cache(), releases, latest, error).await;
}

pub async fn write_changelog_cache_with(
    cache: &Cache,
    releases: &[ChangelogRelease],
    latest: Option<&str>,
    error: Option<&str>,
) {
    let payload = CachedChangelog {
        releases: releases.to_vec(),
        latest: latest.map(str::to_string),
        error: error.map(str::to_string),
        fetched_at: now_ms(),
    };
    cache
        .put_json(changelog_cache_key(), &payload, Duration::from_secs(CHANGELOG_CACHE_TTL_SECONDS))
        .await;
}

/// Product releases, newest first, as GitHub ordered them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchedReleases {
    pub releases: Vec<ChangelogRelease>,
    pub latest: Option<String>,
}

/// GitHub Releases for `repo`, sanitized. `Err` carries the same short reason strings the
/// TypeScript route reports: `HTTP <status>`, `invalid response`, `unreachable/timeout`.
pub async fn fetch_releases(state: &AppState, repo: &str) -> Result<FetchedReleases, String> {
    fetch_releases_with(state.transport().as_ref(), state.config().github_token.as_deref(), repo).await
}

pub async fn fetch_releases_with(
    transport: &dyn UpstreamTransport,
    github_token: Option<&str>,
    repo: &str,
) -> Result<FetchedReleases, String> {
    if !is_valid_repo(repo) {
        return Err("GITHUB_REPO must be owner/repo".into());
    }
    let mut request = UpstreamRequest::get(format!("https://api.github.com/repos/{repo}/releases?per_page=30"))
        // Rendered HTML instead of the markdown body.
        .header("accept", "application/vnd.github.html+json")
        // GitHub hard-403s a UA-less request.
        .header("user-agent", "kano-proxy")
        .header("x-github-api-version", "2022-11-28")
        .timeout(GITHUB_TIMEOUT);
    if let Some(token) = github_token.filter(|t| !t.is_empty()) {
        request = request.header("authorization", &format!("Bearer {token}"));
    }

    let response = transport.send(request).await.map_err(|_| "unreachable/timeout".to_string())?;
    if !response.status.is_success() {
        return Err(format!("HTTP {}", response.status.as_u16()));
    }
    let json = response.json_value().await.map_err(|_| "invalid response".to_string())?;
    let Some(items) = json.as_array() else {
        return Err("invalid response".into());
    };

    let mut releases = Vec::new();
    for r in items {
        if r.get("draft") == Some(&Value::Bool(true)) || r.get("prerelease") == Some(&Value::Bool(true)) {
            continue;
        }
        let tag = match r.get("tag_name").and_then(Value::as_str) {
            Some(t) if !t.is_empty() => t,
            _ => continue,
        };
        // Product releases only. The CLI's cli-vX.Y.Z releases share the repo
        // (docs/deployment.md § CLI release) but say nothing about the server, and one of
        // them becoming `latest` would flag a phantom update on every page.
        if parse_semver(tag).is_none() {
            continue;
        }
        releases.push(ChangelogRelease {
            tag: tag.to_string(),
            name: r.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
            published_at: r.get("published_at").and_then(Value::as_str).unwrap_or("").to_string(),
            url: r.get("html_url").and_then(Value::as_str).unwrap_or("").to_string(),
            body_html: sanitize_release_html(r.get("body_html").and_then(Value::as_str).unwrap_or("")),
        });
    }
    // GitHub orders by created_at desc, not semver — the newest surviving release is first,
    // and nothing is re-sorted.
    let latest = releases.first().map(|r| r.tag.clone());
    Ok(FetchedReleases { releases, latest })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upstream::transport::TransportError;
    use crate::upstream::{MockTransport, UpstreamResponse};
    use http::StatusCode;
    use serde_json::json;

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
        for (k, v) in overrides.as_object().expect("an object of overrides") {
            base[k] = v.clone();
        }
        base
    }

    fn snapshot() -> CachedChangelog {
        CachedChangelog {
            releases: vec![ChangelogRelease {
                tag: "v1.10.0".into(),
                name: "v1.10.0".into(),
                published_at: "2026-06-01T00:00:00Z".into(),
                url: "https://github.com/yufeng-kano/kano-proxy/releases/tag/v1.10.0".into(),
                body_html: "<p>old notes</p>".into(),
            }],
            latest: Some("v1.10.0".into()),
            error: None,
            fetched_at: now_ms(),
        }
    }

    #[test]
    fn the_cache_key_is_global() {
        assert_eq!(changelog_cache_key(), "changelog:v1");
    }

    #[test]
    fn freshness_window_is_one_hour_and_the_ttl_is_seven_days() {
        assert_eq!(CHANGELOG_FRESH_MS, 60 * 60 * 1000);
        assert_eq!(CHANGELOG_CACHE_TTL_SECONDS, 7 * 24 * 60 * 60);
        let fresh = snapshot();
        assert!(is_changelog_fresh(&fresh));
        let aged = CachedChangelog { fetched_at: now_ms() - 2 * 60 * 60 * 1000, ..snapshot() };
        assert!(!is_changelog_fresh(&aged));
    }

    #[test]
    fn repo_shape_is_owner_slash_repo() {
        assert!(is_valid_repo("yufeng-kano/kano-proxy"));
        assert!(!is_valid_repo("evil/../../etc"));
        assert!(!is_valid_repo(""));
        assert!(!is_valid_repo("no-slash"));
    }

    #[tokio::test]
    async fn write_then_read_round_trips_the_snapshot() {
        let cache = Cache::new();
        assert_eq!(read_changelog_cache_with(&cache).await, None);
        let snap = snapshot();
        write_changelog_cache_with(&cache, &snap.releases, snap.latest.as_deref(), None).await;
        let read = read_changelog_cache_with(&cache).await.expect("a cached snapshot");
        assert_eq!(read.releases, snap.releases);
        assert_eq!(read.latest.as_deref(), Some("v1.10.0"));
        assert_eq!(read.error, None);
        assert!(is_changelog_fresh(&read));
    }

    #[tokio::test]
    async fn a_malformed_cache_entry_reads_as_a_miss() {
        let cache = Cache::new();
        cache.put_text(changelog_cache_key(), "not json", Duration::from_secs(60)).await;
        assert_eq!(read_changelog_cache_with(&cache).await, None);
    }

    #[tokio::test]
    async fn fetches_sanitized_releases_newest_first() {
        let mock = MockTransport::new();
        mock.respond_json(
            StatusCode::OK,
            json!([
                release(json!({
                    "tag_name": "v1.11.0",
                    "body_html": "<p>new <a href=\"javascript:alert(1)\">notes</a></p><script>bad()</script>",
                })),
                release(json!({ "tag_name": "v1.10.0" })),
            ]),
        );
        let fetched = fetch_releases_with(mock.as_ref(), None, "yufeng-kano/kano-proxy").await.expect("releases");
        assert_eq!(fetched.latest.as_deref(), Some("v1.11.0"));
        assert_eq!(
            fetched.releases.iter().map(|r| r.tag.clone()).collect::<Vec<_>>(),
            vec!["v1.11.0".to_string(), "v1.10.0".to_string()]
        );
        assert_eq!(fetched.releases[0].body_html, "<p>new notes</p>bad()");
        let request = &mock.requests()[0];
        assert_eq!(request.url, "https://api.github.com/repos/yufeng-kano/kano-proxy/releases?per_page=30");
        assert_eq!(request.header("accept"), Some("application/vnd.github.html+json"));
        assert_eq!(request.header("user-agent"), Some("kano-proxy"));
        assert_eq!(request.header("authorization"), None);
    }

    #[tokio::test]
    async fn sends_the_github_token_when_one_is_configured() {
        let mock = MockTransport::new();
        mock.respond_json(StatusCode::OK, json!([]));
        fetch_releases_with(mock.as_ref(), Some("ghp_test"), "yufeng-kano/kano-proxy").await.expect("releases");
        assert_eq!(mock.requests()[0].header("authorization"), Some("Bearer ghp_test"));
    }

    #[tokio::test]
    async fn filters_drafts_prereleases_and_cli_tags() {
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
        let fetched = fetch_releases_with(mock.as_ref(), None, "yufeng-kano/kano-proxy").await.expect("releases");
        assert_eq!(
            fetched.releases.iter().map(|r| r.tag.clone()).collect::<Vec<_>>(),
            vec!["v1.10.0".to_string(), "v1.9.0".to_string()]
        );
        assert_eq!(fetched.latest.as_deref(), Some("v1.10.0"));
    }

    #[tokio::test]
    async fn reports_the_upstream_status_an_invalid_body_and_an_unreachable_host() {
        let mock = MockTransport::new();
        mock.expect(|_| {
            Ok(UpstreamResponse::from_bytes(StatusCode::INTERNAL_SERVER_ERROR, http::HeaderMap::new(), "boom"))
        });
        assert_eq!(
            fetch_releases_with(mock.as_ref(), None, "yufeng-kano/kano-proxy").await,
            Err("HTTP 500".into())
        );

        let mock = MockTransport::new();
        mock.expect(|_| Ok(UpstreamResponse::from_bytes(StatusCode::OK, http::HeaderMap::new(), "not json")));
        assert_eq!(
            fetch_releases_with(mock.as_ref(), None, "yufeng-kano/kano-proxy").await,
            Err("invalid response".into())
        );

        let mock = MockTransport::new();
        mock.respond_json(StatusCode::OK, json!({ "not": "an array" }));
        assert_eq!(
            fetch_releases_with(mock.as_ref(), None, "yufeng-kano/kano-proxy").await,
            Err("invalid response".into())
        );

        let mock = MockTransport::new();
        mock.expect(|_| Err(TransportError::Connect("network down".into())));
        assert_eq!(
            fetch_releases_with(mock.as_ref(), None, "yufeng-kano/kano-proxy").await,
            Err("unreachable/timeout".into())
        );
    }

    #[tokio::test]
    async fn an_invalid_repo_never_reaches_the_network() {
        let mock = MockTransport::new();
        assert_eq!(
            fetch_releases_with(mock.as_ref(), None, "evil/../../etc").await,
            Err("GITHUB_REPO must be owner/repo".into())
        );
        assert!(mock.requests().is_empty());
    }
}
