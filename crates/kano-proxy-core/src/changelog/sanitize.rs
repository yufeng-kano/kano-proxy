//! Allowlist sanitizer for GitHub release HTML (apps/api/src/changelog/sanitize.ts).
//!
//! GitHub already sanitizes `body_html`; this is the second layer, applied before the markup
//! is cached and handed to the admin UI's `v-html`.
//!
//! Strategy is **escape-then-allowlist**: every byte is escaped as text, and only tags this
//! module constructs itself are re-emitted. No attribute string from the input is ever passed
//! through, so an attribute cannot break out of its quotes or smuggle an event handler —
//! safety comes from what is built, not from what is filtered away.

use once_cell::sync::Lazy;
use regex::Regex;

/// Tags GitHub actually emits for these release notes, plus `ol` alongside `ul`.
const ALLOWED: [&str; 11] = ["a", "code", "em", "h2", "h3", "li", "ol", "p", "strong", "tt", "ul"];

/// Matches one tag; the body is only ever inspected, never re-emitted verbatim.
static TAG_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"</?([a-zA-Z][a-zA-Z0-9]*)((?:"[^"]*"|'[^']*'|[^>"'])*)>?"#).expect("tag regex")
});

static HREF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?i)\bhref\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))"#).expect("href regex")
});

/// A character reference that is already well-formed, e.g. `&amp;` `&#39;` `&#x27;`.
static ENTITY_AT_START_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^&(?:[a-zA-Z][a-zA-Z0-9]{1,31}|#\d{1,7}|#[xX][0-9a-fA-F]{1,6});").expect("entity regex")
});

static SAFE_HREF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)^https://\w").expect("safe href regex"));
static UNSAFE_HREF_CHAR_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"[\s<>"'\\]|[\x00-\x1f]"#).expect("unsafe href char regex"));

/// The input is already HTML, so its `&` are mostly entities GitHub wrote (release prose says
/// `<your-slug>/<model>`, which arrives as `&lt;your-slug&gt;/…`). Re-escaping those would
/// render the entity itself — `&amp;lt;` — so a well-formed reference is left intact. It stays
/// safe: a character reference is decoded after tags and attribute values are delimited, so it
/// can neither open a tag nor escape a quote.
fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < s.len() {
        let c = bytes[i];
        match c {
            b'&' => {
                if ENTITY_AT_START_RE.is_match(&s[i..]) {
                    out.push('&');
                } else {
                    out.push_str("&amp;");
                }
                i += 1;
            }
            b'<' => {
                out.push_str("&lt;");
                i += 1;
            }
            b'>' => {
                out.push_str("&gt;");
                i += 1;
            }
            b'"' => {
                out.push_str("&quot;");
                i += 1;
            }
            _ => {
                let ch = s[i..].chars().next().expect("a char boundary");
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

/// Only absolute https links survive. Anything else (`javascript:`, `data:`,
/// protocol-relative, a bare path) loses the anchor — the link text is kept by the caller, so
/// the words are never lost, only the navigation.
fn safe_href(attrs: &str) -> Option<String> {
    let caps = HREF_RE.captures(attrs)?;
    let raw = caps
        .get(1)
        .or_else(|| caps.get(2))
        .or_else(|| caps.get(3))
        .map(|m| m.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if !SAFE_HREF_RE.is_match(&raw) {
        return None;
    }
    // Control characters would let a terminal or parser see a different string.
    if UNSAFE_HREF_CHAR_RE.is_match(&raw) {
        return None;
    }
    Some(raw)
}

/// Returns markup containing only allowlisted tags with attributes this function wrote.
/// Disallowed tags are dropped but their text is kept, so a future GitHub addition (tables,
/// images) degrades to readable prose instead of vanishing.
pub fn sanitize_release_html(html: &str) -> String {
    if html.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let mut last = 0usize;
    // Anchors are only reopened for links we accepted, so `</a>` cannot leak out of a dropped
    // one.
    let mut open_anchors = 0usize;

    for m in TAG_RE.captures_iter(html) {
        let whole = m.get(0).expect("group 0");
        out.push_str(&escape_text(&html[last..whole.start()]));
        last = whole.end();

        let tag = m[1].to_lowercase();
        let closing = whole.as_str().starts_with("</");

        if !ALLOWED.contains(&tag.as_str()) {
            continue;
        }

        if tag == "a" {
            if closing {
                if open_anchors > 0 {
                    out.push_str("</a>");
                    open_anchors -= 1;
                }
                continue;
            }
            let attrs = m.get(2).map(|g| g.as_str()).unwrap_or("");
            let Some(href) = safe_href(attrs) else {
                continue;
            };
            out.push_str(&format!(
                "<a href=\"{}\" rel=\"noopener noreferrer\" target=\"_blank\">",
                escape_text(&href)
            ));
            open_anchors += 1;
            continue;
        }

        if closing {
            out.push_str(&format!("</{tag}>"));
        } else {
            out.push_str(&format!("<{tag}>"));
        }
    }

    out.push_str(&escape_text(&html[last..]));
    while open_anchors > 0 {
        out.push_str("</a>");
        open_anchors -= 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_a_script_tag_but_keeps_its_text() {
        assert_eq!(sanitize_release_html("<script>alert(1)</script>"), "alert(1)");
    }

    #[test]
    fn drops_an_event_handler_whole() {
        assert_eq!(sanitize_release_html(r#"<img src="x" onerror="alert(1)">"#), "");
    }

    #[test]
    fn drops_a_javascript_href_but_keeps_the_link_text() {
        assert_eq!(sanitize_release_html(r#"<a href="javascript:alert(1)">click me</a>"#), "click me");
    }

    #[test]
    fn preserves_allowlist_tags() {
        assert_eq!(
            sanitize_release_html("<p><strong>hi</strong> <em>there</em></p>"),
            "<p><strong>hi</strong> <em>there</em></p>"
        );
    }

    #[test]
    fn drops_a_non_allowlist_tag_but_keeps_its_text() {
        assert_eq!(sanitize_release_html("<table><tr><td>cell</td></tr></table>"), "cell");
    }

    #[test]
    fn writes_rel_and_target_on_an_accepted_link() {
        assert_eq!(
            sanitize_release_html(r#"<a href="https://example.com/x">link</a>"#),
            r#"<a href="https://example.com/x" rel="noopener noreferrer" target="_blank">link</a>"#
        );
    }

    #[test]
    fn leaves_an_existing_character_reference_alone() {
        assert_eq!(
            sanitize_release_html("<code>&lt;your-slug&gt;/&lt;model&gt;</code>"),
            "<code>&lt;your-slug&gt;/&lt;model&gt;</code>"
        );
        assert_eq!(sanitize_release_html("<p>Q&amp;A</p>"), "<p>Q&amp;A</p>");
        assert_eq!(sanitize_release_html("<p>&#39; &nbsp;</p>"), "<p>&#39; &nbsp;</p>");
    }

    #[test]
    fn still_escapes_a_bare_ampersand() {
        assert_eq!(sanitize_release_html("<p>R&D &notanentity</p>"), "<p>R&amp;D &amp;notanentity</p>");
    }

    #[test]
    fn keeps_an_escaped_tag_inert() {
        assert_eq!(
            sanitize_release_html("<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>"),
            "<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>"
        );
    }

    #[test]
    fn empty_input_is_empty_output() {
        assert_eq!(sanitize_release_html(""), "");
    }

    #[test]
    fn a_stray_closing_anchor_never_leaks_out() {
        assert_eq!(sanitize_release_html("text</a>more"), "textmore");
    }
}
