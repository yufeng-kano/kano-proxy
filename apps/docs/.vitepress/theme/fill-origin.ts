import { inBrowser, type Router } from "vitepress"

/**
 * Tracked files never carry a real hostname, so every URL in the docs is
 * written as `https://<your-domain>/...`. The docs are served from the same
 * host as the proxy, so in the browser that placeholder can be filled with the
 * page's own origin. The static HTML crawlers see keeps the placeholder.
 *
 * Skipped in `vitepress dev`: there the docs server (port 5174) is not the
 * proxy, so filling would point every sample at the docs server itself. The
 * placeholder stays visible, which is also the honest rendering.
 *
 * Runs after hydration (onAfterRouteChanged fires after the page is mounted)
 * and only touches text nodes inside code, so nothing else is affected. The
 * copy button reads the DOM at click time, so it copies the filled value.
 */
const PLACEHOLDER = "<your-domain>"

function fill(): void {
  const { origin, host } = window.location
  for (const code of document.querySelectorAll<HTMLElement>(".vp-doc code")) {
    const walker = document.createTreeWalker(code, NodeFilter.SHOW_TEXT)
    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      const text = node.nodeValue ?? ""
      if (!text.includes(PLACEHOLDER)) continue
      node.nodeValue = text
        .replaceAll(`https://${PLACEHOLDER}`, origin)
        .replaceAll(PLACEHOLDER, host)
    }
  }
}

export function installOriginFill(router: Router): void {
  if (!inBrowser || import.meta.env.DEV) return
  // requestAnimationFrame: the route hook fires before the new page's DOM is
  // fully committed on the first paint of a client-side navigation.
  const schedule = () => requestAnimationFrame(fill)
  router.onAfterRouteChanged = schedule
  schedule()
}
