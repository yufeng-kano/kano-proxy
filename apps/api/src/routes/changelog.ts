import { Hono } from "hono"
import type { Context } from "hono"
import type { HonoEnv } from "../auth/session"
import { loadSessionUser } from "../auth/session"
import {
  readChangelogCache,
  writeChangelogCache,
  isChangelogFresh,
  type ChangelogRelease,
} from "../changelog/cache"
import { sanitizeReleaseHtml } from "../changelog/sanitize"
import { isUpdateAvailable } from "../changelog/version"
import type { UserRow } from "../db/users"
import { version } from "../../../../package.json"

export const changelogRoutes = new Hono<HonoEnv>()

async function requireUser(c: Context<HonoEnv>): Promise<UserRow | null> {
  const loaded = await loadSessionUser(c.env, c.req.header("cookie"))
  if (!loaded) return null
  return loaded.user
}

/**
 * `owner/repo`, both halves URL-safe — the value is interpolated straight into
 * the fetch URL, so anything outside this shape is rejected before it gets
 * there (no path traversal, no URL injection).
 */
const REPO_RE = /^[\w.-]+\/[\w.-]+$/

/** Shape the admin UI reads (see docs/changelog.md). */
function changelogResponse(opts: {
  latest: string | null
  releases: ChangelogRelease[]
  available: boolean
  cached: boolean
  stale: boolean
  error: string | null
}) {
  return {
    current: version,
    latest: opts.latest,
    updateAvailable: isUpdateAvailable(version, opts.latest),
    releases: opts.releases,
    available: opts.available,
    cached: opts.cached,
    stale: opts.stale,
    error: opts.error,
  }
}

/**
 * GET /api/changelog — running version + sanitized release notes from this
 * repo's GitHub Releases, cached in KV. One global cache entry: the notes are
 * identical for every operator, and a per-user key would multiply upstream
 * calls against GitHub's 60/hr unauthenticated budget.
 */
changelogRoutes.get("/", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)

  const repo = (c.env.GITHUB_REPO ?? "").trim()
  if (!REPO_RE.test(repo)) {
    // Graceful degradation: the running version is still useful in the topbar
    // badge, so report the config gap instead of failing the request.
    return c.json(
      changelogResponse({
        latest: null,
        releases: [],
        available: false,
        cached: false,
        stale: false,
        error: repo ? "GITHUB_REPO must be owner/repo" : "GITHUB_REPO is not configured",
      }),
    )
  }

  const refresh = c.req.query("refresh") === "true"
  const cached = await readChangelogCache(c.env)

  // Fresh entry within the hour window — no upstream call.
  if (cached && !refresh && isChangelogFresh(cached)) {
    return c.json(
      changelogResponse({
        latest: cached.latest,
        releases: cached.releases,
        available: true,
        cached: true,
        stale: false,
        error: cached.error,
      }),
    )
  }

  const releases: ChangelogRelease[] = []
  let latest: string | null = null
  let fetchError: string | null = null

  const controller = new AbortController()
  const timeout = setTimeout(() => controller.abort(), 10_000)
  try {
    const headers: Record<string, string> = {
      // Rendered HTML instead of markdown body.
      accept: "application/vnd.github.html+json",
      // GitHub hard-403s a UA-less request.
      "user-agent": "kano-proxy",
      "x-github-api-version": "2022-11-28",
    }
    if (c.env.GITHUB_TOKEN) headers.authorization = `Bearer ${c.env.GITHUB_TOKEN}`

    const res = await fetch(`https://api.github.com/repos/${repo}/releases?per_page=30`, {
      headers,
      signal: controller.signal,
    })
    if (!res.ok) {
      fetchError = `HTTP ${res.status}`
    } else {
      const json = (await res.json().catch(() => null)) as Array<Record<string, unknown>> | null
      if (!json) {
        fetchError = "invalid response"
      } else {
        for (const r of json) {
          if (r.draft === true || r.prerelease === true) continue
          if (typeof r.tag_name !== "string" || !r.tag_name) continue
          releases.push({
            tag: r.tag_name,
            name: typeof r.name === "string" ? r.name : "",
            published_at: typeof r.published_at === "string" ? r.published_at : "",
            url: typeof r.html_url === "string" ? r.html_url : "",
            body_html: sanitizeReleaseHtml(typeof r.body_html === "string" ? r.body_html : ""),
          })
        }
        // GitHub orders by created_at desc, not semver — newest surviving
        // release is first, and nothing is re-sorted.
        latest = releases[0]?.tag ?? null
      }
    }
  } catch {
    fetchError = "unreachable/timeout"
  } finally {
    clearTimeout(timeout)
  }

  if (fetchError && cached) {
    // Stale-serve — deliberate deviation from the rest of the codebase
    // (usage/models caches treat an aged entry as a miss and surface the
    // upstream error): stale release notes are harmless, stale usage numbers
    // are misleading. A failed refetch (quota exhausted, upstream down)
    // degrades to the last good data instead of a broken page.
    return c.json(
      changelogResponse({
        latest: cached.latest,
        releases: cached.releases,
        available: true,
        cached: true,
        stale: true,
        error: fetchError,
      }),
    )
  }
  if (fetchError) {
    // No cache to fall back on — nothing fabricated, the error says why.
    return c.json(
      changelogResponse({
        latest: null,
        releases: [],
        available: true,
        cached: false,
        stale: false,
        error: fetchError,
      }),
    )
  }

  await writeChangelogCache(c.env, { releases, latest, error: null })
  return c.json(
    changelogResponse({
      latest,
      releases,
      available: true,
      cached: false,
      stale: false,
      error: null,
    }),
  )
})
