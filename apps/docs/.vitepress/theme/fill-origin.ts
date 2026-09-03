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
 * Shiki splits a highlighted line into many spans, and in a bash block the
 * `<`, `your-domain`, and `>` of the placeholder land in different ones. The
 * match is therefore run over a code element's concatenated text and spliced
 * back across the text nodes it spans: the replacement goes into the node
 * where the match starts, the matched characters are cut from the rest.
 * Unmatched text keeps its node, so the highlighting around it is unchanged.
 *
 * Runs after hydration (onAfterRouteChanged fires after the page is mounted)
 * and only touches text nodes inside code, so nothing else is affected. The
 * copy button reads the DOM at click time, so it copies the filled value.
 */
const PLACEHOLDER = "<your-domain>"

function textNodes(root: Node): Text[] {
  const nodes: Text[] = []
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT)
  for (let node = walker.nextNode(); node; node = walker.nextNode()) nodes.push(node as Text)
  return nodes
}

/** Replaces [start, end) of the nodes' concatenated text with `replacement`. */
function splice(nodes: Text[], start: number, end: number, replacement: string): void {
  let offset = 0
  let first = true
  for (const node of nodes) {
    const text = node.nodeValue ?? ""
    const nodeStart = offset
    const nodeEnd = offset + text.length
    offset = nodeEnd
    if (nodeEnd <= start || nodeStart >= end) continue
    const from = Math.max(start, nodeStart) - nodeStart
    const to = Math.min(end, nodeEnd) - nodeStart
    node.nodeValue = text.slice(0, from) + (first ? replacement : "") + text.slice(to)
    first = false
  }
}

function replaceAll(code: HTMLElement, needle: string, replacement: string): void {
  // Re-read after every splice: node lengths changed. Terminates because the
  // replacement never contains the needle.
  for (;;) {
    const nodes = textNodes(code)
    const index = nodes.map((n) => n.nodeValue ?? "").join("").indexOf(needle)
    if (index === -1) return
    splice(nodes, index, index + needle.length, replacement)
  }
}

function fill(): void {
  const { origin, host } = window.location
  for (const code of document.querySelectorAll<HTMLElement>(".vp-doc code")) {
    if (!code.textContent?.includes(PLACEHOLDER)) continue
    replaceAll(code, `https://${PLACEHOLDER}`, origin)
    replaceAll(code, PLACEHOLDER, host)
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
