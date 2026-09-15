//! SSRF / loop guard for user-supplied custom-provider base URLs
//! (apps/api/src/utils/upstream_url.ts, docs/providers.md § Custom providers).
//!
//! https only, no embedded credentials, no query/fragment, and the hostname must not be
//! localhost, a private/loopback/link-local literal, or this deploy's own host. Applied on
//! create, update, and the test-connection endpoint — never trust a stored value that skipped
//! this check.

use once_cell::sync::Lazy;
use regex::Regex;
use url::Url;

/// Request context for the own-host rule; both hostnames are bare (no port).
#[derive(Debug, Clone, Default)]
pub struct UpstreamUrlCheckOpts {
    /// Bare hostname of the incoming admin request.
    pub request_host: Option<String>,
    /// Bare hostname parsed from `APP_URL`, when set.
    pub app_url_host: Option<String>,
    /// Field name used in error messages — `None` means `base_url`.
    pub field_name: Option<String>,
}

impl UpstreamUrlCheckOpts {
    pub fn field(name: &str) -> Self {
        Self { field_name: Some(name.to_string()), ..Self::default() }
    }
}

static IPV4_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$").expect("ipv4 regex"));
static TRAILING_SLASHES_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"/+$").expect("trailing slash regex"));
static IPV6_LINK_LOCAL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^fe[89ab]").expect("ipv6 link-local regex"));
static IPV6_UNIQUE_LOCAL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^f[cd]").expect("ipv6 ULA regex"));

/// `Ok(url)` is the normalized value to store; `Err(message)` is the wire error text.
pub fn validate_upstream_base_url(input: &str, opts: &UpstreamUrlCheckOpts) -> Result<String, String> {
    let field = opts.field_name.as_deref().unwrap_or("base_url");
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(format!("{field} is required"));
    }

    let Ok(url) = Url::parse(trimmed) else {
        return Err(format!("{field} must be a valid URL"));
    };

    if url.scheme() != "https" {
        return Err(format!("{field} must use https"));
    }
    if !url.username().is_empty() || url.password().is_some_and(|p| !p.is_empty()) {
        return Err(format!("{field} must not contain credentials"));
    }
    if url.query().is_some_and(|q| !q.is_empty()) {
        return Err(format!("{field} must not contain a query string"));
    }
    if url.fragment().is_some_and(|f| !f.is_empty()) {
        return Err(format!("{field} must not contain a fragment"));
    }

    let hostname = url.host_str().unwrap_or("").to_lowercase();
    if let Some(err) = check_hostname(&hostname, opts, field) {
        return Err(err);
    }

    // Strip trailing slash(es): endpoints are built by literal concatenation
    // (`{base}/chat/completions`), so a stored trailing slash would double up.
    Ok(TRAILING_SLASHES_RE.replace(url.as_str(), "").into_owned())
}

fn check_hostname(hostname: &str, opts: &UpstreamUrlCheckOpts, field: &str) -> Option<String> {
    if hostname == "localhost" || hostname.ends_with(".localhost") || hostname.ends_with(".local") {
        return Some(format!("{field} must not point at localhost"));
    }

    let ipv4 = parse_ipv4(hostname);
    if ipv4.is_some_and(is_private_or_loopback_ipv4) {
        return Some(format!("{field} must not point at a private or loopback address"));
    }
    if ipv4.is_none() && hostname.starts_with('[') && hostname.ends_with(']') {
        let inner = &hostname[1..hostname.len() - 1];
        if is_blocked_ipv6(inner) {
            return Some(format!("{field} must not point at a private or loopback address"));
        }
    }

    let request_host = opts.request_host.as_ref().map(|h| h.to_lowercase()).filter(|h| !h.is_empty());
    let app_url_host = opts.app_url_host.as_ref().map(|h| h.to_lowercase()).filter(|h| !h.is_empty());
    if request_host.as_deref() == Some(hostname) || app_url_host.as_deref() == Some(hostname) {
        return Some(format!("{field} must not point at this deploy's own host"));
    }

    None
}

fn parse_ipv4(hostname: &str) -> Option<[u16; 4]> {
    let caps = IPV4_RE.captures(hostname)?;
    let mut parts = [0u16; 4];
    for (i, part) in parts.iter_mut().enumerate() {
        *part = caps[i + 1].parse::<u16>().ok()?;
    }
    if parts.iter().any(|n| *n > 255) {
        return None;
    }
    Some(parts)
}

fn is_private_or_loopback_ipv4(parts: [u16; 4]) -> bool {
    let (a, b) = (parts[0], parts[1]);
    a == 127 // 127.0.0.0/8 loopback
        || a == 0 // 0.0.0.0/8 ("this network", includes 0.0.0.0 itself)
        || a == 10 // 10.0.0.0/8
        || (a == 172 && (16..=31).contains(&b)) // 172.16.0.0/12
        || (a == 192 && b == 168) // 192.168.0.0/16
        || (a == 169 && b == 254) // 169.254.0.0/16 link-local
}

/// `addr` is already stripped of the `[]` brackets a URL keeps for IPv6.
fn is_blocked_ipv6(addr: &str) -> bool {
    let a = addr.to_lowercase();
    if a == "::1" {
        return true; // loopback
    }
    let first_hextet = a.split(':').next().unwrap_or("");
    IPV6_LINK_LOCAL_RE.is_match(first_hextet) // fe80::/10 link-local
        || IPV6_UNIQUE_LOCAL_RE.is_match(first_hextet) // fc00::/7 unique-local
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(input: &str) -> Result<String, String> {
        validate_upstream_base_url(input, &UpstreamUrlCheckOpts::default())
    }

    #[test]
    fn accepts_a_plain_https_url_unchanged() {
        assert_eq!(check("https://api.example.com/v1"), Ok("https://api.example.com/v1".into()));
        assert_eq!(check("https://openrouter.example.com/api/v1"), Ok("https://openrouter.example.com/api/v1".into()));
    }

    #[test]
    fn strips_trailing_slashes_on_save() {
        assert_eq!(check("https://api.example.com/v1/"), Ok("https://api.example.com/v1".into()));
        assert_eq!(check("https://api.example.com/v1//"), Ok("https://api.example.com/v1".into()));
        assert_eq!(check("https://api.example.com"), Ok("https://api.example.com".into()));
    }

    #[test]
    fn rejects_empty_and_non_url_input() {
        assert!(check("").is_err());
        assert!(check("not a url").is_err());
    }

    #[test]
    fn rejects_non_https_schemes() {
        assert_eq!(check("http://api.example.com/v1"), Err("base_url must use https".into()));
        assert!(check("ftp://api.example.com/v1").is_err());
    }

    #[test]
    fn rejects_credentials_query_and_fragment() {
        assert_eq!(
            check("https://user:pass@api.example.com/v1"),
            Err("base_url must not contain credentials".into())
        );
        assert_eq!(
            check("https://api.example.com/v1?x=1"),
            Err("base_url must not contain a query string".into())
        );
        assert_eq!(
            check("https://api.example.com/v1#frag"),
            Err("base_url must not contain a fragment".into())
        );
    }

    #[test]
    fn rejects_localhost_shaped_hostnames() {
        assert!(check("https://localhost/v1").is_err());
        assert!(check("https://foo.localhost/v1").is_err());
        assert!(check("https://my-box.local/v1").is_err());
    }

    #[test]
    fn rejects_private_and_loopback_ipv4() {
        for bad in [
            "https://127.0.0.1/v1",
            "https://127.10.20.30/v1",
            "https://0.0.0.0/v1",
            "https://10.1.2.3/v1",
            "https://172.16.0.1/v1",
            "https://172.31.255.255/v1",
            "https://192.168.1.1/v1",
            "https://169.254.169.254/v1",
        ] {
            assert!(check(bad).is_err(), "expected {bad} to be rejected");
        }
        assert!(check("https://172.15.0.1/v1").is_ok());
        assert!(check("https://172.32.0.1/v1").is_ok());
    }

    #[test]
    fn rejects_loopback_link_local_and_unique_local_ipv6() {
        for bad in ["https://[::1]/v1", "https://[fe80::1]/v1", "https://[fc00::1]/v1", "https://[fd12::1]/v1"] {
            assert!(check(bad).is_err(), "expected {bad} to be rejected");
        }
        assert!(check("https://[2001:db8::1]/v1").is_ok());
    }

    #[test]
    fn rejects_the_deploys_own_hosts_case_insensitively() {
        let request = UpstreamUrlCheckOpts { request_host: Some("kano.example.com".into()), ..Default::default() };
        assert_eq!(
            validate_upstream_base_url("https://kano.example.com/v1", &request),
            Err("base_url must not point at this deploy's own host".into())
        );
        let app = UpstreamUrlCheckOpts { app_url_host: Some("kano.example.com".into()), ..Default::default() };
        assert!(validate_upstream_base_url("https://kano.example.com/v1", &app).is_err());
        assert!(validate_upstream_base_url("https://KANO.example.com/v1", &request).is_err());
    }

    #[test]
    fn allows_a_different_host_even_with_request_context() {
        let opts = UpstreamUrlCheckOpts {
            request_host: Some("kano.example.com".into()),
            app_url_host: Some("kano.example.com".into()),
            field_name: None,
        };
        assert_eq!(
            validate_upstream_base_url("https://upstream.example.com/v1", &opts),
            Ok("https://upstream.example.com/v1".into())
        );
    }

    #[test]
    fn uses_the_field_name_in_error_messages_and_keeps_the_host_guard() {
        let opts = UpstreamUrlCheckOpts::field("count_tokens_url");
        assert_eq!(
            validate_upstream_base_url("http://api.example.com/v1", &opts),
            Err("count_tokens_url must use https".into())
        );
        assert_eq!(
            validate_upstream_base_url("https://count.example.com/anthropic/count_tokens", &opts),
            Ok("https://count.example.com/anthropic/count_tokens".into())
        );
        assert_eq!(
            validate_upstream_base_url("https://127.0.0.1/count_tokens", &opts),
            Err("count_tokens_url must not point at a private or loopback address".into())
        );
    }
}
