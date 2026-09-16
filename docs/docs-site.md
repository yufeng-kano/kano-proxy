# Public documentation site (`/docs/`)

The one public, indexable surface of a kano-proxy instance. It tells an end user how to sign in, connect a subscription, issue a key, and point a coding agent at the proxy. Everything else on the host is behind Google sign-in and is deliberately kept out of search indexes (§ SEO).

## Why a separate static site

The admin UI is a client-rendered SPA behind a login wall; search engines and link-preview scrapers see an empty shell. A docs site needs real HTML, a sidebar, and full-text search. VitePress gives all three as static files with no runtime, no database, and no server rendering of the admin app. Nuxt was considered and rejected for now: it solves problems this site does not have (per-viewer content, one framework for the whole product) at the cost of a second runtime. Visual alignment with the admin UI is **not** a goal: the docs use the VitePress default theme as is (operator decision 2026-09-04).

## Location and build

| Path | Role |
|------|------|
| `apps/docs/` | VitePress project; installs and builds on its own |
| `apps/docs/.vitepress/site.ts` | `defineDocsConfig(edition)`: `base: "/docs/"`, locales, sidebar, local search, sitemap, plus whatever an edition appends (§ Editions) |
| `apps/docs/.vitepress/config.ts` | The standalone site: `defineDocsConfig()` plus `srcDir: "pages"` |
| `apps/docs/.vitepress/theme/` | Default theme plus the origin fill (below) |
| `apps/docs/pages/*.md`, `apps/docs/pages/zh-TW/*.md` | English (root) and Traditional Chinese content, one file per page in each tree |
| `apps/docs/.vitepress/dist/` | Build output (gitignored, like every `dist/`) |
| `apps/docs/.vitepress/cache/` | Dev cache (gitignored) |

The docs are served from the **same hostname** as the admin UI, under `/docs/`. Root `pnpm build:site` builds the web app, builds the docs, and copies the docs output into `apps/web/dist/docs/`; that single directory is what the web image serves.

**SPA fallback and the docs.** The static server answers an unknown path under `/docs/` with VitePress's own `404.html` and a real `404` status, and every other unknown path with the SPA's `index.html`. A blanket rewrite to `index.html` would swallow the documentation pages, so the two rules are separate and `/docs/` is matched first. Extensionless docs URLs (`/docs/guide/x`) resolve to `x.html`, which is what VitePress's own links expect.

Checked by hand after the v4.7.0 deploy: `/docs/` and a docs page return 200 with their own titles, `/keys` returns the SPA shell with `noindex`, `/docs/no-such-page` returns 404. `/robots.txt` also carries `noindex` from the `/*` rule; harmless, since the file is read, not indexed.

Local: `pnpm --filter docs dev` serves the docs alone at `http://127.0.0.1:5174/docs/`.

CI (`ci.yml`, `release-deploy.yml`) runs `pnpm build:site` in place of the old web-only build. The release job passes `APP_URL` into the build so the sitemap carries absolute URLs; when `APP_URL` is unset (local builds, PR CI) no sitemap is emitted and the build still succeeds. The release job also checks out with full history (`fetch-depth: 0`): VitePress reads each page's "last updated" from `git log`, and a shallow clone would date every page to the release commit.

## The real hostname without hardcoding it

Tracked files keep `<your-domain>` placeholders ([deployment.md](./deployment.md)). A public docs page that only ever said `<your-domain>` would be useless to the reader, so the site fills the placeholder in the browser: a theme enhancement walks the rendered page's `code` elements after each route render and replaces `https://<your-domain>` with `location.origin` and any remaining `<your-domain>` with `location.host`. The static HTML that crawlers index still contains the placeholder; a person reading the page sees their instance's real URL, and the copy button copies what is shown. The fill is skipped under `vitepress dev`, where the docs server is not the proxy and filling would point every sample at port 5174.

Write every URL in the docs as `https://<your-domain>/...` exactly, so the fill matches. Do not invent other spellings of the placeholder.

## Content

English is the reference tree; `zh-TW/` mirrors it page for page with the same file names, so the language switcher (`i18nRouting`) lands on the same topic. A page added to one tree is added to the other in the same change.

| Page | Covers |
|------|--------|
| `index.md` | What the proxy is, the two base URLs, where to go next |
| `guide/getting-started.md` | Sign in, connect a provider, create a key, first request |
| `guide/endpoints.md` | Base URLs, auth header, `provider/model` ids, model groups, listing models |
| `guide/local-models.md` | Exposing a local LLM with the `kano-proxy` CLI ([cli.md](./cli.md)) |
| `agents/<tool>.md` | One page per coding agent: Claude Code, Codex CLI, Cursor, Cline, OpenCode, Gemini CLI |

Rules for the agent pages:

- **Verified, dated, sourced.** Third-party config keys change. Every agent page ends with the official source it was checked against and the check date. A setting that could not be verified against an official or otherwise reliable source is not written; the page says what is unknown instead.
- **Honest about limits.** If a tool cannot use the proxy for some feature, or at all, the page says so plainly at the top. Gemini CLI is the standing example: it speaks only Google's own API shapes, which this proxy does not expose.
- **Placeholders only for secrets and hosts.** `<your-api-key>` and `<your-domain>`; never a real key, never a real hostname.
- **Model ids are examples, marked as such.** The live list is the Models page and `GET /openai/v1/models`; the docs never claim a model exists.

Writing style, both languages:

- Answer first, then the steps. One idea per sentence, short sentences.
- No metaphors, no emoji, no em dashes, no filler such as "note that".
- Bullets for three or more parallel items; prose for fewer.
- Code, config keys, and env var names in backticks; commands and config files in fenced blocks.
- Traditional Chinese only in `zh-TW/`; write it as a native speaker would, not as a translation.

## SEO and indexing

Only `/docs/*` and `/login` are meant to be indexed. The admin routes render the same empty shell to a crawler and are hidden from indexes with a header, not with `robots.txt`: a `Disallow` stops crawling but not indexing of a linked URL, while `noindex` needs the crawler to fetch the page.

| Piece | Where | What it does |
|-------|-------|--------------|
| `apps/web/public/robots.txt` | site root | Allows crawling, except the API prefixes (`/openai/`, `/anthropic/`, `/g/`, `/api/`, `/agent/`): those never reach the static server, so `_headers` cannot mark them, and an API surface is what `Disallow` is for. No `Sitemap:` line because the directive needs an absolute URL and tracked files carry no hostname; submit `/docs/sitemap.xml` in Search Console instead |
| `apps/web/public/_headers` | site root | `X-Robots-Tag: noindex` on `/*`, detached again (`! X-Robots-Tag`) for `/docs/*` and `/login`. A catch-all rather than a route list, because any unknown path also serves the SPA shell. Nothing to maintain when a route is added |
| `apps/web/index.html` | SPA shell | `description`, Open Graph and Twitter card tags, so a shared link to the app gets a preview card. Copy repeats the login pitch from the message catalog; keep them in sync |
| Router `afterEach` | SPA | Sets `document.title` to `<page> · <site name>` from the route's `titleKey` (a catalog key), so tabs and history are readable. `<html lang>` is already set by `setLocale()` ([i18n.md](./i18n.md)) |
| VitePress config | docs | Per-page `<title>` and `description`, Open Graph tags, `sitemap.xml` under `/docs/` when `APP_URL` is set at build time |

Not done, on purpose: `apple-touch-icon` and a web manifest (nobody installs an admin panel to a home screen), `llms.txt` (revisit once the docs have settled), and any structured data.

## Editions

A private edition ([cloud-edition.md](./cloud-edition.md)) documents features the core does not have, on the same site. It does so from its own VitePress project rather than by patching this one:

- Its `.vitepress/config.ts` imports `defineDocsConfig` from this package's `site.ts` and passes the sidebar groups and nav entries for its pages, which are appended after the core's. Its theme re-exports this package's theme so the origin fill still runs.
- Its source tree is **assembled at build time**: the core's `*.md` files copied first, the edition's pages copied over them into the same `guide/`, `agents/`, `zh-TW/…` layout. An edition page may not have the path of a core page; the core's text is the core's to change.
- The assembled copy has no git history, so the edition passes `lastUpdated: false` instead of letting every page date itself to the build.
- The content rules above apply unchanged: an English page has its `zh-TW/` twin in the same change, placeholders only for secrets and hosts, no invented model ids.

`site.ts` and the theme entry are build integration points for editions, like the web app's `@` alias; nothing else under `apps/docs/.vitepress/` is.

## Links into the docs

- Login page footer: a "Documentation" link beside the contact address.
- Signed-in sidebar: a "Documentation" entry above Changelog, opening in a new tab. Both use the `nav.docs` catalog key. An edition may move the sidebar entry into the account menu ([admin-ui.md](./admin-ui.md) § Layout: edition options).
