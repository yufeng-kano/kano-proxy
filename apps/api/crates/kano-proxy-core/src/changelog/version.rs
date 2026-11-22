//! SemVer comparison for the running-version badge (docs/admin-ui.md § Changelog).
//!
//! Pure — no server APIs — so the update-available rules stay unit-testable.

use once_cell::sync::Lazy;
use regex::Regex;

static SEMVER_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^v?(\d+)\.(\d+)\.(\d+)$").expect("semver regex"));

/// `MAJOR.MINOR.PATCH` with an optional leading `v`; anything else is not a version.
pub fn parse_semver(v: &str) -> Option<(u64, u64, u64)> {
    let caps = SEMVER_RE.captures(v.trim())?;
    Some((caps[1].parse().ok()?, caps[2].parse().ok()?, caps[3].parse().ok()?))
}

/// True only when `current` is strictly behind `latest`, compared per numeric component (so
/// `1.10.0` sorts above `1.9.0`). Unparseable input is never an update: the caller cannot act
/// on an ordering it cannot trust.
///
/// A local version *ahead* of the newest release is the normal state between a version bump
/// and its release, so it must not read as "update available".
pub fn is_update_available(current: &str, latest: Option<&str>) -> bool {
    match (parse_semver(current), latest.and_then(parse_semver)) {
        (Some(current), Some(latest)) => current < latest,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_and_v_prefixed_versions() {
        assert_eq!(parse_semver("1.11.0"), Some((1, 11, 0)));
        assert_eq!(parse_semver(" v1.11.0 "), Some((1, 11, 0)));
        assert_eq!(parse_semver("cli-v1.2.0"), None);
        assert_eq!(parse_semver("1.11"), None);
        assert_eq!(parse_semver("banana"), None);
    }

    #[test]
    fn an_update_is_available_only_when_current_is_strictly_behind_latest() {
        for (current, latest, expected) in [
            ("1.10.0", Some("1.11.0"), true),
            ("1.11.0", Some("1.11.0"), false),
            ("1.12.0", Some("1.11.0"), false),
            // Numeric, not lexical.
            ("1.9.0", Some("1.10.0"), true),
            ("1.10.0", Some("1.9.0"), false),
            // A v-prefixed tag against the bare version.
            ("1.11.0", Some("v1.11.0"), false),
            ("1.10.0", Some("v1.11.0"), true),
            // Unparseable or absent input is never an update.
            ("banana", Some("1.11.0"), false),
            ("1.11.0", Some("banana"), false),
            ("1.11.0", None, false),
        ] {
            assert_eq!(is_update_available(current, latest), expected, "{current} vs {latest:?}");
        }
    }
}
