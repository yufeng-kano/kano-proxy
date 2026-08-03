/**
 * The shell's scrolling element, shared with the handful of things that need
 * to move it.
 *
 * The signed-in app is a fixed frame — sidebar, page header, and section nav
 * all stay put while only the content region scrolls (docs/admin-ui.md
 * § Layout). That makes `window.scrollTo` and `window.scrollY` inert, so
 * anything that used to reach for them needs this element instead: scroll
 * restore, the Providers section nav, and the reset-to-top on navigation.
 *
 * A module-level ref rather than provide/inject because two of the three
 * callers are not components (a router hook and a composable used outside the
 * shell's subtree), and there is exactly one shell per app.
 */

let region: HTMLElement | null = null

/** Called by AppShell on mount/unmount. Null while the login route is showing. */
export function setScrollRegion(el: HTMLElement | null): void {
  region = el
}

export function getScrollRegion(): HTMLElement | null {
  return region
}

/**
 * Puts a freshly-navigated page at the top. The router cannot do this itself:
 * its `scrollBehavior` only ever knew how to move the document.
 */
export function resetScroll(): void {
  region?.scrollTo({ top: 0, behavior: "auto" })
}

/** Scrolls a section into view within the region — the Providers section nav. */
export function scrollIntoRegion(el: HTMLElement | null, offset = 0): void {
  if (!el || !region) return
  const top = el.getBoundingClientRect().top - region.getBoundingClientRect().top
  region.scrollTo({
    top: Math.max(0, region.scrollTop + top - offset),
    behavior: prefersReducedMotion() ? "auto" : "smooth",
  })
}

function prefersReducedMotion(): boolean {
  if (typeof matchMedia === "undefined") return false
  return matchMedia("(prefers-reduced-motion: reduce)").matches
}
