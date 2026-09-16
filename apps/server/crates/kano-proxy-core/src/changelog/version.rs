//! SemVer comparison for the running-version badge (//! docs/admin-ui.md § Changelog).
//!
//! Pure — no server APIs — so the update-available rules stay unit-testable.

use std::cmp::Ordering;

use once_cell::sync::Lazy;
use regex::Regex;

static SEMVER_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^v?(\d+)\.(\d+)\.(\d+)$").expect("semver regex"));

/// `MAJOR.MINOR.PATCH` with an optional leading `v`; anything else is not a version.
pub fn parse_semver(v: &str) -> Option<(u64, u64, u64)> {
    let caps = SEMVER_RE.captures(v.trim())?;
    Some((caps[1].parse().ok()?, caps[2].parse().ok()?, caps[3].parse().ok()?))
}

/// Numeric per-component compare, so `1.10.0` sorts above `1.9.0` (a string compare would get
/// that backwards). Unparseable input compares [`Ordering::Equal`] — the caller cannot act on
/// an ordering it cannot trust, and [`is_update_available`] turns that into "no update".
pub fn compare_semver(a: &str, b: &str) -> Ordering {
    match (parse_semver(a), parse_semver(b)) {
        (Some(pa), Some(pb)) => pa.cmp(&pb),
        _ => Ordering::Equal,
    }
}

/// True only when `current` is strictly behind `latest`.
///
/// A local version *ahead* of the newest release is the normal state between a version bump
/// and its release, so it must not read as "update available".
pub fn is_update_available(current: &str, latest: Option<&str>) -> bool {
    let Some(latest) = latest else {
        return false;
    };
    if parse_semver(current).is_none() || parse_semver(latest).is_none() {
        return false;
    }
    compare_semver(current, latest) == Ordering::Less
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
    fn true_only_when_current_is_strictly_behind_latest() {
        assert!(is_update_available("1.10.0", Some("1.11.0")));
        assert!(!is_update_available("1.11.0", Some("1.11.0")));
        assert!(!is_update_available("1.12.0", Some("1.11.0")));
    }

    #[test]
    fn compares_numerically_not_lexically() {
        assert!(is_update_available("1.9.0", Some("1.10.0")));
        assert!(!is_update_available("1.10.0", Some("1.9.0")));
        assert_eq!(compare_semver("1.10.0", "1.9.0"), Ordering::Greater);
    }

    #[test]
    fn is_false_for_unparseable_input() {
        assert!(!is_update_available("banana", Some("1.11.0")));
        assert!(!is_update_available("1.11.0", Some("banana")));
        assert!(!is_update_available("1.11.0", None));
        assert_eq!(compare_semver("banana", "1.11.0"), Ordering::Equal);
    }

    #[test]
    fn matches_a_v_prefixed_tag_against_the_bare_version() {
        assert!(!is_update_available("1.11.0", Some("v1.11.0")));
        assert!(is_update_available("1.10.0", Some("v1.11.0")));
    }
}
