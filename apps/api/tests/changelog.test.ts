import { afterEach, describe, expect, it } from "vitest"
import { createSession } from "../src/auth/session"
import { changelogCacheKey, type CachedChangelog } from "../src/changelog/cache"
import { sanitizeReleaseHtml } from "../src/changelog/sanitize"
import { isUpdateAvailable } from "../src/changelog/version"
import { changelogRoutes } from "../src/routes/changelog"
import type { Env } from "../src/env"
import { FakeD1, fakeKV } from "./helpers/fake_d1"
import { version } from "../../../package.json"

const SESSION_SECRET = "test-session-secret-not-real"
const APP_URL = "https://app.example.com"

function buildEnv(db: FakeD1, overrides: Partial<Env> = {}): Env {
  return {
    DB: db as unknown as D1Database,
    BENCH: fakeKV(),
    CACHE: fakeKV(),
    APP_URL,
    SESSION_SECRET,
    GITHUB_REPO: "yufeng-kano/kano-proxy",
    ...overrides,
  } as unknown as Env
}

function seedUser(db: FakeD1, id = "user_1"): void {
  db.seed("users", [
    {
      id,
      google_sub: `sub-${id}`,
      email: `${id}@example.com`,
      name: "Test User",
      picture_url: null,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
}

async function cookieFor(env: Env, userId: string): Promise<string> {
  const { cookie } = await createSession(env, userId)
  return cookie.split(";")[0]!
}

function req(cookie?: string): RequestInit {
  return { method: "GET", headers: cookie ? { cookie } : {} }
}

/** Seeds a user and returns auth'd env + cookie for route tests. */
async function authed(
  db: FakeD1,
  overrides: Partial<Env> = {},
): Promise<{ env: Env; cookie: string }> {
  seedUser(db)
  const env = buildEnv(db, overrides)
  const cookie = await cookieFor(env, "user_1")
  return { env, cookie }
}

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

/** A GitHub releases API item, with defaults that pass the route's filters. */
function release(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    tag_name: "v1.11.0",
    name: "v1.11.0",
    published_at: "2026-07-30T00:00:00Z",
    html_url: "https://github.com/yufeng-kano/kano-proxy/releases/tag/v1.11.0",
    body_html: "<p>Release notes</p>",
    draft: false,
    prerelease: false,
    ...overrides,
  }
}

function stubReleases(...releases: Record<string, unknown>[]): void {
  globalThis.fetch = (async () =>
    new Response(JSON.stringify(releases), { status: 200 })) as typeof fetch
}

function stubStatus(status: number): void {
  globalThis.fetch = (async () => new Response("boom", { status })) as typeof fetch
}

/**
 * Seed an entry that is past the 1h freshness window. fakeKV().put ignores
 * `expirationTtl`, so staleness must be exercised via `fetchedAt`, not KV
 * expiry.
 */
async function seedAgedCache(env: Env, overrides: Partial<CachedChangelog> = {}): Promise<void> {
  await env.CACHE.put(
    changelogCacheKey(),
    JSON.stringify({
      releases: [
        {
          tag: "v1.10.0",
          name: "v1.10.0",
          published_at: "2026-06-01T00:00:00Z",
          url: "https://github.com/yufeng-kano/kano-proxy/releases/tag/v1.10.0",
          body_html: "<p>old notes</p>",
        },
      ],
      latest: "v1.10.0",
      error: null,
      fetchedAt: Date.now() - 2 * 60 * 60 * 1000,
      ...overrides,
    }),
  )
}

/** Loosely typed — this test asserts shape/values, not the response's own type. */
type ChangelogJson = {
  current: string
  latest: string | null
  updateAvailable: boolean
  releases: Array<{
    tag: string
    name: string
    published_at: string
    url: string
    body_html: string
  }>
  available: boolean
  cached: boolean
  stale: boolean
  error: string | null
}

describe("isUpdateAvailable", () => {
  it("true only when current is strictly behind latest", () => {
    expect(isUpdateAvailable("1.10.0", "1.11.0")).toBe(true) // behind
    expect(isUpdateAvailable("1.11.0", "1.11.0")).toBe(false) // equal
    expect(isUpdateAvailable("1.12.0", "1.11.0")).toBe(false) // ahead — normal between bump and release
  })

  it("compares numerically, not lexically — 1.10.0 beats 1.9.0", () => {
    expect(isUpdateAvailable("1.9.0", "1.10.0")).toBe(true)
    expect(isUpdateAvailable("1.10.0", "1.9.0")).toBe(false)
  })

  it("is false for unparseable input", () => {
    expect(isUpdateAvailable("banana", "1.11.0")).toBe(false)
    expect(isUpdateAvailable("1.11.0", "banana")).toBe(false)
    expect(isUpdateAvailable("1.11.0", null)).toBe(false)
  })

  it("matches a v-prefixed tag against the bare version — v1.11.0 is not an update over 1.11.0", () => {
    expect(isUpdateAvailable("1.11.0", "v1.11.0")).toBe(false)
    expect(isUpdateAvailable("1.10.0", "v1.11.0")).toBe(true)
  })
})

describe("sanitizeReleaseHtml", () => {
  it("drops a <script> tag but keeps its text", () => {
    expect(sanitizeReleaseHtml("<script>alert(1)</script>")).toBe("alert(1)")
  })

  it("drops an event handler whole — <img onerror=...> vanishes with its attribute", () => {
    expect(sanitizeReleaseHtml('<img src="x" onerror="alert(1)">')).toBe("")
  })

  it("drops a javascript: href but keeps the link text", () => {
    expect(sanitizeReleaseHtml('<a href="javascript:alert(1)">click me</a>')).toBe("click me")
  })

  it("preserves allowlist tags", () => {
    expect(sanitizeReleaseHtml("<p><strong>hi</strong> <em>there</em></p>")).toBe(
      "<p><strong>hi</strong> <em>there</em></p>",
    )
  })

  it("drops a non-allowlist tag but keeps its text", () => {
    expect(sanitizeReleaseHtml("<table><tr><td>cell</td></tr></table>")).toBe("cell")
  })

  it("writes rel/target on an accepted link", () => {
    expect(sanitizeReleaseHtml('<a href="https://example.com/x">link</a>')).toBe(
      '<a href="https://example.com/x" rel="noopener noreferrer" target="_blank">link</a>',
    )
  })

  // The input is already HTML, so its entities are GitHub's. Re-escaping them
  // would render the entity itself: release prose about `<your-slug>/<model>`
  // would read as "&lt;your-slug&gt;" on the page.
  it("leaves an existing character reference alone instead of double-escaping it", () => {
    expect(sanitizeReleaseHtml("<code>&lt;your-slug&gt;/&lt;model&gt;</code>")).toBe(
      "<code>&lt;your-slug&gt;/&lt;model&gt;</code>",
    )
    expect(sanitizeReleaseHtml("<p>Q&amp;A</p>")).toBe("<p>Q&amp;A</p>")
    expect(sanitizeReleaseHtml("<p>&#39; &nbsp;</p>")).toBe("<p>&#39; &nbsp;</p>")
  })

  it("still escapes a bare ampersand that is not a character reference", () => {
    expect(sanitizeReleaseHtml("<p>R&D &notanentity</p>")).toBe(
      "<p>R&amp;D &amp;notanentity</p>",
    )
  })

  it("keeps an escaped tag inert rather than reviving it", () => {
    expect(sanitizeReleaseHtml("<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>")).toBe(
      "<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>",
    )
  })
})

describe("GET /api/changelog", () => {
  it("requires auth", async () => {
    const db = new FakeD1()
    const res = await changelogRoutes.request("/", req(), buildEnv(db))
    expect(res.status).toBe(401)
  })

  it("returns sanitized releases newest-first and computes updateAvailable from the package version", async () => {
    const db = new FakeD1()
    const { env, cookie } = await authed(db)
    // Stay ahead of the bundled package version so this assertion survives
    // every real SemVer bump (hardcoding the next tag breaks on that release).
    const [maj, min] = version.split(".").map((p) => Number(p))
    const newerTag = `v${maj}.${(min ?? 0) + 1}.0`
    const olderTag = `v${maj}.${min ?? 0}.0`
    stubReleases(
      release({
        tag_name: newerTag,
        body_html: '<p>new <a href="javascript:alert(1)">notes</a></p><script>bad()</script>',
      }),
      release({ tag_name: olderTag }),
    )

    const res = await changelogRoutes.request("/", req(cookie), env)
    expect(res.status).toBe(200)
    const json = (await res.json()) as ChangelogJson
    expect(json.current).toBe(version)
    expect(json.latest).toBe(newerTag)
    expect(json.updateAvailable).toBe(true)
    expect(json.available).toBe(true)
    expect(json.cached).toBe(false)
    expect(json.stale).toBe(false)
    expect(json.error).toBeNull()
    expect(json.releases.map((r) => r.tag)).toEqual([newerTag, olderTag])
    expect(json.releases[0]!.body_html).toBe("<p>new notes</p>bad()")
  })

  it("serves a fresh cache entry without refetching", async () => {
    const db = new FakeD1()
    const { env, cookie } = await authed(db)
    let calls = 0
    globalThis.fetch = (async () => {
      calls++
      return new Response(JSON.stringify([release()]), { status: 200 })
    }) as typeof fetch

    const first = await changelogRoutes.request("/", req(cookie), env)
    expect(first.status).toBe(200)
    expect(((await first.json()) as ChangelogJson).cached).toBe(false)

    const second = await changelogRoutes.request("/", req(cookie), env)
    expect(second.status).toBe(200)
    const json = (await second.json()) as ChangelogJson
    expect(json.cached).toBe(true)
    expect(json.stale).toBe(false)
    // End-to-end v-prefix check: tag v1.11.0 must not read as an update over 1.11.0.
    expect(json.latest).toBe("v1.11.0")
    expect(json.updateAvailable).toBe(false)
    expect(calls).toBe(1)
  })

  it("?refresh=true bypasses the freshness window and refetches", async () => {
    const db = new FakeD1()
    const { env, cookie } = await authed(db)
    let calls = 0
    globalThis.fetch = (async () => {
      calls++
      return new Response(JSON.stringify([release()]), { status: 200 })
    }) as typeof fetch

    await changelogRoutes.request("/", req(cookie), env)
    const res = await changelogRoutes.request("/?refresh=true", req(cookie), env)
    expect(res.status).toBe(200)
    const json = (await res.json()) as ChangelogJson
    expect(json.cached).toBe(false)
    expect(calls).toBe(2)
  })

  it("upstream 500 with a warm cache serves the old data as stale, not an error", async () => {
    const db = new FakeD1()
    const { env, cookie } = await authed(db)
    await seedAgedCache(env)
    stubStatus(500)

    const res = await changelogRoutes.request("/", req(cookie), env)
    expect(res.status).toBe(200)
    const json = (await res.json()) as ChangelogJson
    expect(json.releases.map((r) => r.tag)).toEqual(["v1.10.0"])
    expect(json.latest).toBe("v1.10.0")
    expect(json.cached).toBe(true)
    expect(json.stale).toBe(true)
    expect(json.error).toBe("HTTP 500")
  })

  it("upstream network failure with a warm cache serves the old data as stale", async () => {
    const db = new FakeD1()
    const { env, cookie } = await authed(db)
    await seedAgedCache(env)
    globalThis.fetch = (async () => {
      throw new Error("network down")
    }) as typeof fetch

    const res = await changelogRoutes.request("/", req(cookie), env)
    expect(res.status).toBe(200)
    const json = (await res.json()) as ChangelogJson
    expect(json.latest).toBe("v1.10.0")
    expect(json.cached).toBe(true)
    expect(json.stale).toBe(true)
    expect(json.error).toBe("unreachable/timeout")
  })

  it("upstream failure with no cache returns an empty list and the error, without throwing", async () => {
    const db = new FakeD1()
    const { env, cookie } = await authed(db)
    stubStatus(500)

    const res = await changelogRoutes.request("/", req(cookie), env)
    expect(res.status).toBe(200)
    const json = (await res.json()) as ChangelogJson
    expect(json.available).toBe(true)
    expect(json.releases).toEqual([])
    expect(json.latest).toBeNull()
    expect(json.updateAvailable).toBe(false)
    expect(json.cached).toBe(false)
    expect(json.stale).toBe(false)
    expect(json.error).toBe("HTTP 500")
  })

  it("GITHUB_REPO unset degrades gracefully — current still served, no throw, no fetch", async () => {
    const db = new FakeD1()
    const { env, cookie } = await authed(db, { GITHUB_REPO: undefined })
    globalThis.fetch = (async () => {
      throw new Error("must not fetch without GITHUB_REPO")
    }) as typeof fetch

    const res = await changelogRoutes.request("/", req(cookie), env)
    expect(res.status).toBe(200)
    const json = (await res.json()) as ChangelogJson
    expect(json.current).toBe(version)
    expect(json.available).toBe(false)
    expect(json.releases).toEqual([])
    expect(json.updateAvailable).toBe(false)
    expect(json.error).toBe("GITHUB_REPO is not configured")
  })

  it("GITHUB_REPO blank behaves like unset", async () => {
    const db = new FakeD1()
    const { env, cookie } = await authed(db, { GITHUB_REPO: "" })

    const res = await changelogRoutes.request("/", req(cookie), env)
    const json = (await res.json()) as ChangelogJson
    expect(json.available).toBe(false)
    expect(json.error).toBe("GITHUB_REPO is not configured")
  })

  it("rejects a GITHUB_REPO that is not owner/repo before fetching", async () => {
    const db = new FakeD1()
    const { env, cookie } = await authed(db, { GITHUB_REPO: "evil/../../etc" })
    globalThis.fetch = (async () => {
      throw new Error("must not fetch an invalid repo")
    }) as typeof fetch

    const res = await changelogRoutes.request("/", req(cookie), env)
    expect(res.status).toBe(200)
    const json = (await res.json()) as ChangelogJson
    expect(json.available).toBe(false)
    expect(json.error).toBe("GITHUB_REPO must be owner/repo")
  })

  it("filters out draft and prerelease releases", async () => {
    const db = new FakeD1()
    const { env, cookie } = await authed(db)
    stubReleases(
      release({ tag_name: "v1.11.0", draft: true }),
      release({ tag_name: "v1.10.5", prerelease: true }),
      release({ tag_name: "v1.10.0" }),
      release({ tag_name: "v1.9.0" }),
    )

    const res = await changelogRoutes.request("/", req(cookie), env)
    expect(res.status).toBe(200)
    const json = (await res.json()) as ChangelogJson
    expect(json.releases.map((r) => r.tag)).toEqual(["v1.10.0", "v1.9.0"])
    expect(json.latest).toBe("v1.10.0")
  })

  it("skips cli-v releases so they never become latest", async () => {
    const db = new FakeD1()
    const { env, cookie } = await authed(db)
    stubReleases(
      release({ tag_name: "cli-v1.2.0", name: "cli-v1.2.0" }),
      release({ tag_name: "v1.10.0" }),
      release({ tag_name: "cli-v1.1.0", name: "cli-v1.1.0" }),
      release({ tag_name: "v1.9.0" }),
    )

    const res = await changelogRoutes.request("/", req(cookie), env)
    expect(res.status).toBe(200)
    const json = (await res.json()) as ChangelogJson
    expect(json.releases.map((r) => r.tag)).toEqual(["v1.10.0", "v1.9.0"])
    expect(json.latest).toBe("v1.10.0")
  })
})
