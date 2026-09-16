# Changelog

Signed-in operators see **what changed** and **which version they are running**, sourced from this repo's published GitHub Releases. An edition may leave the whole surface out of its web shell (`ShellOptions.changelog`, [cloud-edition.md](./cloud-edition.md) § Web shell); this document describes the standalone build.

There is no hand-maintained `CHANGELOG.md` and no changelog table in the database. The release notes written at release time (see [deployment.md](./deployment.md) — Releases) are the single source of truth; the admin UI reads them through the server.

Which makes those notes a **user-facing surface with no fallback**: whatever the release body says is exactly what this page shows, and an empty body renders an empty card. They are hand-written for that reason — `gh release create --generate-notes` summarizes merged pull requests, and this repo lands work as direct commits, so it yields a bare compare link. See [deployment.md](./deployment.md) § Release notes are hand-written.

## Surfaces

| Surface | Content |
|---------|---------|
| `GET /api/changelog` | Session-auth JSON: running version, latest published version, update flag, sanitized release list |
| `/changelog` (web) | One entry per release, newest first, as a two-column timeline (version rail + notes — [admin-ui.md](./admin-ui.md) § Changelog page); the running version is marked |
| Sidebar badge | Running version on every signed-in page; a dot appears when a newer release exists |

## Data flow

```
GitHub Releases API ──► server (in-process cache + sanitize) ──► web (localStorage cache-first)
```

## Running version

The server reports the core crate's own version, compiled in:

```rust
env!("CARGO_PKG_VERSION")
```

This needs no configuration: the release process already requires that version to equal the release tag (see [deployment.md](./deployment.md)), so the compiled value is correct by construction — and a local build reports a real version instead of a blank. An edition that ships on its own cycle overrides it with its own.

Only releases whose tag is a product version (`vMAJOR.MINOR.PATCH`) are listed or considered for `latest`. The CLI's `cli-v…` releases live in the same GitHub repo ([deployment.md](./deployment.md) § CLI release) and are skipped here: they say nothing about the server an operator is running, and letting one become `latest` would flag a phantom update on every page.

`updateAvailable` is computed server-side by numeric SemVer comparison. When the local version is **ahead** of the newest published release — normal between a version bump and its release — the flag is `false`. Only strictly-behind reports an update.

## Configuration

| Var | Where | Required | Purpose |
|-----|-------|----------|---------|
| `GITHUB_REPO` | environment (public) | No | `owner/repo` to read releases from. Forks point this at their own repo. Unset ⇒ the feature reports unavailable instead of showing upstream's releases. |
| `GITHUB_TOKEN` | environment (secret) | No | Raises the GitHub rate limit from 60/hr to 5000/hr. The cache alone keeps a deployment well inside the unauthenticated budget, so this is a safety valve, not a requirement. |

`GITHUB_REPO` is public information and belongs in the tracked example. `GITHUB_TOKEN` is a secret and must never be committed — see [deployment.md](./deployment.md).

A missing or misconfigured `GITHUB_REPO` degrades gracefully: the endpoint still returns the running version so the sidebar version badge keeps working, with an error field and an empty release list.

## Caching and rate limits

Unauthenticated GitHub API is **60 requests/hr per IP**. Conditional requests do **not** help: a `304` still decrements the quota (measured). Only the cache does.

Two time constants, one cache entry:

| Knob | Value | Role |
|------|-------|------|
| Entry lifetime | 7 days | How long the entry survives |
| `fetchedAt` freshness window | 1 hour | When a refetch is attempted |

The cache key is **global** (`changelog:v1`), deliberately **not** user-scoped like `models:v1:<userId>:…`. Release notes are identical for every operator; a per-user key would multiply GitHub calls by the number of signed-in users and exhaust the quota. Global key ⇒ at most one upstream call per hour for the whole deployment.

`?refresh=true` bypasses the freshness window (same convention as `/api/models`), but still writes back to the shared entry.

### Stale-serve — a deliberate deviation

When a refetch fails, this endpoint returns the **last good data** with `stale: true` rather than an error.

The other cache in this codebase (the model catalog) treats an expired entry as a miss and surfaces upstream errors. Changelog differs on purpose: **stale release notes are harmless, stale usage numbers are misleading**. It also means a quota exhausted by a co-tenant IP degrades to slightly-old notes instead of a broken page.

## HTML sanitization

Requests use `Accept: application/vnd.github.html+json`, so GitHub returns `body_html` — already rendered and already sanitized. No markdown parser ships in the web bundle.

The server sanitizes **again** before storing and serving, and the page renders the result with `v-html`. GitHub's sanitizer is the first layer, not the only one.

Strategy is **escape-then-allowlist**: escape everything, then re-emit only tags this codebase constructs itself. Attribute strings are never passed through verbatim.

- Allowed: `a code em h2 h3 li p strong tt ul`
- `a` keeps only `href`, which must be `https://`; `rel="noopener noreferrer" target="_blank"` is written by us, and GitHub's own `class`/`rel` are dropped
- Every other allowed tag is emitted bare (that is how GitHub sends them)
- A tag outside the allowlist is dropped but **its text is kept**, so a future GitHub addition (tables, images) degrades to readable text rather than vanishing

A full HTML parser would be stronger than string work, but the inputs justify the string approach: authored by the repo owner, pre-sanitized upstream, and a small closed tag set. The sanitizer carries the densest unit tests in this feature.

## Web caching

`localStorage` under `kano-proxy:changelog`, cache-first like every other page: paint cache, refresh in the background, keep cache and show a non-blocking error on failure. TTL is **1 hour** here rather than the 2 min used for accounts/models/usage — release notes change on deploy, not continuously.

Unlike the other cached domains this key carries **no user id** (the data is identical for everyone and contains nothing user-identifying), so the logout sweep clears it unconditionally.
