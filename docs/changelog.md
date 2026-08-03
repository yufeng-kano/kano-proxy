# Changelog

Signed-in operators see **what changed** and **which version they are running**, sourced from this repo's published GitHub Releases.

There is no hand-maintained `CHANGELOG.md` and no changelog table in D1. The release notes written at release time (see [deployment.md](./deployment.md) — Releases) are the single source of truth; the admin UI reads them through the Worker.

## Surfaces

| Surface | Content |
|---------|---------|
| `GET /api/changelog` | Session-auth JSON: running version, latest published version, update flag, sanitized release list |
| `/changelog` (web) | One card per release, newest first; the running version is marked |
| Sidebar badge | Running version on every signed-in page; a dot appears when a newer release exists |

## Data flow

```
GitHub Releases API ──► Worker (KV cache + sanitize) ──► Web (localStorage cache-first)
```

## Running version

The Worker reports its own version from the repo root `package.json`, bundled in at build time:

```ts
import { version } from "../../../package.json"
```

This needs no CI variable and no wrangler var: the release process already requires root `package.json` `"version"` to equal the release tag (see [deployment.md](./deployment.md)), so the bundled value is correct by construction — and local dev reports a real version instead of a blank.

Use the **named** import, not `import pkg from`. esbuild tree-shakes the named form to the single string; the default form inlines the entire `package.json` (scripts, devDependencies) into the Worker bundle.

`updateAvailable` is computed server-side by numeric SemVer comparison. When the local version is **ahead** of the newest published release — normal between a version bump and its release — the flag is `false`. Only strictly-behind reports an update.

## Configuration

| Var | Where | Required | Purpose |
|-----|-------|----------|---------|
| `GITHUB_REPO` | `wrangler.toml` `[vars]` (public) | No | `owner/repo` to read releases from. Forks point this at their own repo. Unset ⇒ the feature reports unavailable instead of showing upstream's releases. |
| `GITHUB_TOKEN` | `.dev.vars` / `wrangler secret` | No | Raises the GitHub rate limit from 60/hr to 5000/hr. The KV cache alone keeps a deploy well inside the unauthenticated budget, so this is a safety valve, not a requirement. |

`GITHUB_REPO` is public information and belongs in the committed `[vars]`. `GITHUB_TOKEN` is a secret and must never be committed — see [deployment.md](./deployment.md).

A missing or misconfigured `GITHUB_REPO` degrades gracefully: the endpoint still returns the running version so the sidebar version badge keeps working, with an error field and an empty release list.

## Caching and rate limits

Unauthenticated GitHub API is **60 requests/hr per IP**, and Cloudflare Workers egress from shared addresses — so the budget is not exclusively ours. Conditional requests do **not** help: a `304` still decrements the quota (measured). Only the cache does.

Two time constants, one KV entry:

| Knob | Value | Role |
|------|-------|------|
| KV `expirationTtl` | 7 days | How long the entry survives |
| `fetchedAt` freshness window | 1 hour | When a refetch is attempted |

The KV key is **global** (`changelog:v1`), deliberately **not** user-scoped like `models:v1:<userId>:…`. Release notes are identical for every operator; a per-user key would multiply GitHub calls by the number of signed-in users and exhaust the quota. Global key ⇒ at most one upstream call per hour for the whole deployment.

`?refresh=true` bypasses the freshness window (same convention as `/api/models`), but still writes back to the shared entry.

### Stale-serve — a deliberate deviation

When a refetch fails, this endpoint returns the **last good data** with `stale: true` rather than an error.

The other KV cache in this codebase (`catalog/models.ts`) treats an expired entry as a miss and surfaces upstream errors. Changelog differs on purpose: **stale release notes are harmless, stale usage numbers are misleading**. It also means a quota exhausted by a co-tenant IP degrades to slightly-old notes instead of a broken page.

## HTML sanitization

Requests use `Accept: application/vnd.github.html+json`, so GitHub returns `body_html` — already rendered and already sanitized. No markdown parser ships in the web bundle.

The Worker sanitizes **again** before storing and serving, and the page renders the result with `v-html`. GitHub's sanitizer is the first layer, not the only one.

Strategy is **escape-then-allowlist**: escape everything, then re-emit only tags this codebase constructs itself. Attribute strings are never passed through verbatim.

- Allowed: `a code em h2 h3 li p strong tt ul`
- `a` keeps only `href`, which must be `https://`; `rel="noopener noreferrer" target="_blank"` is written by us, and GitHub's own `class`/`rel` are dropped
- Every other allowed tag is emitted bare (that is how GitHub sends them)
- A tag outside the allowlist is dropped but **its text is kept**, so a future GitHub addition (tables, images) degrades to readable text rather than vanishing

`HTMLRewriter` would be a real parser rather than string work, but it does not exist in the Vitest node environment — the sanitizer is the one part of this feature that most needs unit tests, and reshaping the whole suite around it would be the wrong trade. The inputs justify the string approach: authored by the repo owner, pre-sanitized upstream, and a small closed tag set.

## Web caching

`localStorage` under `kano-proxy:changelog`, cache-first like every other page: paint cache, refresh in the background, keep cache and show a non-blocking error on failure. TTL is **1 hour** here rather than the 90s used for accounts/models/usage — release notes change on deploy, not continuously.

Unlike the other cached domains this key carries **no user id** (the data is identical for everyone and contains nothing user-identifying), so the logout sweep clears it unconditionally.
