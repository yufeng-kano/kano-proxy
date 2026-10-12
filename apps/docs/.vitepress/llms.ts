import { mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { dirname, join } from "node:path"
import type { DefaultTheme, SiteConfig } from "vitepress"

/**
 * llms.txt for the docs (docs/docs-site.md § llms.txt): an index in the
 * llmstxt.org shape, one Markdown copy per English page next to its HTML, and
 * llms-full.txt with every page in sidebar order. Written at build end from the
 * same sidebar the site shows, so an edition's pages are included and the order
 * never drifts from the site's.
 */
export function writeLlmsTxt(
  site: SiteConfig,
  options: { origin: string; name: string; summary: string; sidebar: DefaultTheme.SidebarItem[] },
): void {
  const { origin, name, summary, sidebar } = options
  const index = [`# ${name}`, "", `> ${summary}`, ""]
  index.push(
    "Code samples write the proxy's address as `https://<your-domain>`; the address that serves this file is the one to use.",
    "",
  )
  const full = [`# ${name}`, "", `> ${summary}`, ""]

  const sections: [string, DefaultTheme.SidebarItem[]][] = [["Start here", [{ text: name, link: "/" }]]]
  for (const group of sidebar) sections.push([group.text ?? "", group.items ?? []])

  for (const [title, items] of sections) {
    index.push(`## ${title}`, "")
    for (const item of items) {
      if (!item.link) continue
      const source = item.link === "/" ? "index.md" : `${item.link.slice(1)}.md`
      const page = readPage(join(site.srcDir, source))
      const out = join(site.outDir, source)
      mkdirSync(dirname(out), { recursive: true })
      writeFileSync(out, page.body)
      const description = page.description ? `: ${page.description}` : ""
      index.push(`- [${page.title ?? item.text}](${origin}/docs/${source})${description}`)
      full.push(page.body.trimEnd(), "")
    }
    index.push("")
  }

  writeFileSync(join(site.outDir, "llms.txt"), index.join("\n"))
  writeFileSync(join(site.outDir, "llms-full.txt"), full.join("\n"))
}

/** Splits a page into its frontmatter title/description and the Markdown body. */
function readPage(path: string): { title?: string; description?: string; body: string } {
  const text = readFileSync(path, "utf8")
  const match = /^---\n([\s\S]*?)\n---\n/.exec(text)
  if (!match) return { body: text }
  const field = (key: string) => new RegExp(`^${key}: (.+)$`, "m").exec(match[1])?.[1].trim()
  return { title: field("title"), description: field("description"), body: text.slice(match[0].length) }
}
