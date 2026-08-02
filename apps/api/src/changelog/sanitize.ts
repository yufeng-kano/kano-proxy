/**
 * Allowlist sanitizer for GitHub release HTML.
 *
 * GitHub already sanitizes `body_html`; this is the second layer, applied
 * before the markup is stored in KV and handed to the admin UI's `v-html`.
 *
 * Strategy is **escape-then-allowlist**: every byte is escaped as text, and
 * only tags this module constructs itself are re-emitted. No attribute string
 * from the input is ever passed through, so an attribute cannot break out of
 * its quotes or smuggle an event handler — safety comes from what is built,
 * not from what is filtered away.
 *
 * `HTMLRewriter` would be a real parser rather than string work, but it does
 * not exist in the Node test environment, and this file is the part of the
 * changelog feature that most needs unit tests.
 *
 * Pure — no Worker APIs.
 */

/** Tags GitHub actually emits for these release notes, plus `ol` alongside `ul`. */
const ALLOWED = new Set([
  "a",
  "code",
  "em",
  "h2",
  "h3",
  "li",
  "ol",
  "p",
  "strong",
  "tt",
  "ul",
])

/** Matches one tag; the body is only ever inspected, never re-emitted verbatim. */
const TAG_RE = /<\/?([a-zA-Z][a-zA-Z0-9]*)((?:"[^"]*"|'[^']*'|[^>"'])*)>?/g

const HREF_RE = /\bhref\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))/i

/** A character reference that is already well-formed, e.g. `&amp;` `&#39;` `&#x27;`. */
const ENTITY_RE = /&(?:[a-zA-Z][a-zA-Z0-9]{1,31}|#\d{1,7}|#[xX][0-9a-fA-F]{1,6});/

function escapeText(s: string): string {
  return (
    s
      // The input is already HTML, so its `&` are mostly entities GitHub wrote
      // (release prose says `<your-slug>/<model>`, which arrives as
      // `&lt;your-slug&gt;/…`). Re-escaping those would render the entity
      // itself — `&amp;lt;` — so a well-formed reference is left intact. It
      // stays safe: a character reference is decoded after tags and attribute
      // values are delimited, so it can neither open a tag nor escape a quote.
      .replace(new RegExp(`&(?!${ENTITY_RE.source.slice(1)})`, "g"), "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
  )
}

/**
 * Only absolute https links survive. Anything else (`javascript:`, `data:`,
 * protocol-relative, a bare path) loses the anchor — the link text is kept by
 * the caller, so the words are never lost, only the navigation.
 */
function safeHref(attrs: string): string | null {
  const m = HREF_RE.exec(attrs)
  if (!m) return null
  const raw = (m[1] ?? m[2] ?? m[3] ?? "").trim()
  if (!/^https:\/\/\w/i.test(raw)) return null
  // Control characters would let a terminal or parser see a different string.
  if (/[\s<>"'\\]|[\u0000-\u001f]/.test(raw)) return null
  return raw
}

/**
 * Returns markup containing only allowlisted tags with attributes this
 * function wrote. Disallowed tags are dropped but their text is kept, so a
 * future GitHub addition (tables, images) degrades to readable prose instead
 * of vanishing.
 */
export function sanitizeReleaseHtml(html: string): string {
  if (!html) return ""

  let out = ""
  let last = 0
  // Anchors are only reopened for links we accepted, so `</a>` can't leak out
  // of a dropped one.
  let openAnchors = 0

  TAG_RE.lastIndex = 0
  let m: RegExpExecArray | null
  while ((m = TAG_RE.exec(html))) {
    out += escapeText(html.slice(last, m.index))
    last = m.index + m[0].length

    const tag = m[1].toLowerCase()
    const closing = m[0].startsWith("</")

    if (!ALLOWED.has(tag)) continue

    if (tag === "a") {
      if (closing) {
        if (openAnchors > 0) {
          out += "</a>"
          openAnchors--
        }
        continue
      }
      const href = safeHref(m[2] ?? "")
      if (!href) continue
      out += `<a href="${escapeText(href)}" rel="noopener noreferrer" target="_blank">`
      openAnchors++
      continue
    }

    out += closing ? `</${tag}>` : `<${tag}>`
  }

  out += escapeText(html.slice(last))
  while (openAnchors-- > 0) out += "</a>"

  return out
}
