/**
 * The shell's scrolling element, shared with the handful of things that need
 * to move it.
 *
 * The signed-in app is a fixed frame — sidebar, page header, and section nav
 * all stay put while only the content region scrolls (docs/admin-ui.md
 * § Layout). That makes `window.scrollTo` and `window.scrollY` inert, so
 * anything that moves the page needs this element instead: scroll restore,
 * a modal's scroll lock, and the reset-to-top on navigation.
 *
 * A module-level variable rather than provide/inject because there is exactly
 * one shell per app, and its readers need not sit inside the shell's subtree.
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
