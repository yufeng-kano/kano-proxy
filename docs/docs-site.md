# Public documentation site (`/docs/`)

The one public, indexable surface of a kano-proxy instance. It tells an end user how to sign in, connect a subscription, issue a key, and point a coding agent at the proxy. Everything else on the host is behind Google sign-in and is deliberately kept out of search indexes (§ SEO).

## Why a separate static site

The admin UI is a client-rendered SPA behind a login wall; search engines and link-preview scrapers see an empty shell. A docs site needs real HTML, a sidebar, and full-text search. VitePress gives all three as static files with no runtime, no D1, and no server rendering of the admin app. Nuxt was considered and rejected for now: it solves problems this site does not have (per-viewer content, one framework for the whole product) at the cost of a second runtime. Visual alignment with the admin UI is **not** a goal: the docs use the VitePress default theme as is (operator decision 2026-09-04).

## Location and build

| Path | Role |
|------|------|
| `apps/docs/` | VitePress project; pnpm workspace member `docs` |
| `apps/docs/.vitepress/config.ts` | Site config: `base: "/docs/"`, locales, sidebar, local search, sitemap |
| `apps/docs/.vitepress/theme/` | Default theme plus the origin fill (below) |
| `apps/docs/*.md`, `apps/docs/zh-TW/*.md` | English (root) and Traditional Chinese content, one file per page in each tree |
| `apps/docs/.vitepress/dist/` | Build output (gitignored, like every `dist/`) |
| `apps/docs/.vitepress/cache/` | Dev cache (gitignored) |

The docs are served from the **same Pages project and hostname** as the admin UI, under `/docs/`. Root `pnpm build:site` builds the web app, builds the docs, and copies the docs output into `apps/web/dist/docs/`; that single directory is what `wrangler pages deploy` uploads.

**No `_redirects` file.** The old `/* /index.html 200` rule is gone: Cloudflare applies `_redirects` rules before it looks for a matching asset ("Redirects are always followed, regardless of whether or not an asset matches the incoming request"), so that rule would have swallowed every docs page. The SPA fallback now comes from Pages' built-in behavior instead: a project with no top-level `404.html` is treated as a single-page app and unknown paths serve `/index.html`. VitePress emits its own `404.html` inside `/docs/`. Cloudflare documents both a per-directory `404.html` lookup and the SPA fallback, but not which one wins when a project has a nested `404.html` and no root one, so it was unknown which an unknown `/docs/` path would show. **Observed on the v4.7.0 deploy (2026-09-04): the per-directory lookup wins.** `/docs/no-such-page` returns HTTP 404 with the VitePress 404 page, while `/keys` still serves the SPA with `X-Robots-Tag: noindex`. A scoped `_redirects` rule cannot fix this either way, since redirects run before asset lookup and would swallow the real docs pages. Pages also serves `/docs/guide/x.html` at `/docs/guide/x`, which is what VitePress `cleanUrls` expects. Pinned versions: `vitepress@^1.6.4` (the 2.x line is still alpha as of 2026-09).

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
| `apps/web/public/robots.txt` | site root | Allows crawling, except the Worker-routed API prefixes (`/openai/`, `/anthropic/`, `/g/`, `/api/`, `/agent/`): those never reach Pages, so `_headers` cannot mark them, and an API surface is what `Disallow` is for. No `Sitemap:` line because the directive needs an absolute URL and tracked files carry no hostname; submit `/docs/sitemap.xml` in Search Console instead |
| `apps/web/public/_headers` | site root | `X-Robots-Tag: noindex` on `/*`, detached again (`! X-Robots-Tag`) for `/docs/*` and `/login`. A catch-all rather than a route list, because any unknown path also serves the SPA shell. Nothing to maintain when a route is added |
| `apps/web/index.html` | SPA shell | `description`, Open Graph and Twitter card tags, so a shared link to the app gets a preview card. Copy repeats the login pitch from the message catalog; keep them in sync |
| Router `afterEach` | SPA | Sets `document.title` to `<page> · <site name>` from the route's `titleKey` (a catalog key), so tabs and history are readable. `<html lang>` is already set by `setLocale()` ([i18n.md](./i18n.md)) |
| VitePress config | docs | Per-page `<title>` and `description`, Open Graph tags, `sitemap.xml` under `/docs/` when `APP_URL` is set at build time |

Not done, on purpose: `apple-touch-icon` and a web manifest (nobody installs an admin panel to a home screen), `llms.txt` (revisit once the docs have settled), and any structured data.

## Links into the docs

- Login page footer: a "Docs" link beside the contact address.
- Signed-in sidebar: a "Docs" entry above Changelog, opening in a new tab. Both use the `nav.docs` catalog key.
