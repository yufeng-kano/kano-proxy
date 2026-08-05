import { afterEach, describe, expect, it } from "vitest"
import { createSession } from "../src/auth/session"
import { encryptJson } from "../src/crypto/token_crypto"
import type { Env } from "../src/env"
import { providerRoutes } from "../src/routes/providers"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

const SESSION_SECRET = "test-session-secret-not-real"
const TOKEN_KEY = "test-token-encryption-key-not-secret"
const APP_URL = "https://app.example.com"
const NOW = "2026-01-01T00:00:00.000Z"

function buildEnv(db: FakeD1): Env {
  return {
    DB: db as unknown as D1Database,
    BENCH: fakeKV(),
    CACHE: fakeKV(),
    APP_URL,
    SESSION_SECRET,
    TOKEN_ENCRYPTION_KEY: TOKEN_KEY,
  } as unknown as Env
}

function seedUser(db: FakeD1, id: string): void {
  db.seed("users", [
    {
      id,
      google_sub: `sub-${id}`,
      email: `${id}@example.com`,
      name: "Test User",
      picture_url: null,
      created_at: NOW,
      updated_at: NOW,
    },
  ])
}

async function cookieFor(env: Env, userId: string): Promise<string> {
  const { cookie } = await createSession(env, userId)
  return cookie.split(";")[0]!
}

function req(method: string, cookie?: string, body?: unknown): RequestInit {
  return {
    method,
    headers: {
      "content-type": "application/json",
      host: "app.example.com",
      ...(cookie ? { cookie } : {}),
    },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  }
}

async function readJson(res: Response): Promise<any> {
  return res.json()
}

async function seedAccount(
  db: FakeD1,
  userId: string,
  opts: { id?: string; label?: string | null; customLabel?: string | null } = {},
): Promise<string> {
  const id = opts.id ?? "acc_1"
  const encryptedPayload = await encryptJson(TOKEN_KEY, { access_token: "tok_test" })
  db.seed("upstream_accounts", [
    {
      id,
      user_id: userId,
      provider: "claude-code",
      external_account_id: null,
      label: opts.label ?? "old-upstream-label",
      custom_label: opts.customLabel ?? null,
      priority: 7,
      encrypted_payload: encryptedPayload,
      account_meta_json: JSON.stringify({ email: "stored@example.com" }),
      created_at: NOW,
      updated_at: NOW,
    },
  ])
  return id
}

function stubClaudeUsage(email: string): void {
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    const url = String(input)
    if (url.endsWith("/api/oauth/usage")) {
      return new Response(JSON.stringify({ five_hour: { utilization: 10 } }), { status: 200 })
    }
    if (url.endsWith("/api/oauth/profile")) {
      return new Response(JSON.stringify({ account: { email } }), { status: 200 })
    }
    throw new Error(`unexpected fetch: ${url}`)
  }) as typeof fetch
}

/**
 * Same as `stubClaudeUsage`, but counts usage calls — the usage cache's whole
 * point is how many of these reach upstream. `utilization` varies per call so
 * a cached response is distinguishable from a fresh one.
 */
function countingClaudeUsage(): { calls: () => number } {
  let n = 0
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    const url = String(input)
    if (url.endsWith("/api/oauth/usage")) {
      n++
      return new Response(JSON.stringify({ five_hour: { utilization: n * 10 } }), { status: 200 })
    }
    if (url.endsWith("/api/oauth/profile")) {
      return new Response(JSON.stringify({ account: { email: "u@example.com" } }), { status: 200 })
    }
    throw new Error(`unexpected fetch: ${url}`)
  }) as typeof fetch
  return { calls: () => n }
}

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

describe("PATCH /api/providers/:provider/accounts/:id custom_label", () => {
  it("sets a custom_label and GET /accounts returns it as label and custom_label", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const accountId = await seedAccount(db, "user_1")
    const before = { ...db.rows("upstream_accounts")[0] }

    const patch = await providerRoutes.request(
      `/claude-code/accounts/${accountId}`,
      req("PATCH", cookie, { custom_label: "  Work Claude  " }),
      env,
    )
    expect(patch.status).toBe(200)
    expect(await readJson(patch)).toEqual({ ok: true, custom_label: "Work Claude" })

    const rowAfterPatch = db.rows("upstream_accounts")[0]!
    expect(rowAfterPatch).toMatchObject({
      custom_label: "Work Claude",
      label: before.label,
      priority: before.priority,
      encrypted_payload: before.encrypted_payload,
      account_meta_json: before.account_meta_json,
    })

    stubClaudeUsage("upstream@example.com")
    const get = await providerRoutes.request(
      "/claude-code/accounts",
      req("GET", cookie),
      env,
    )
    expect(get.status).toBe(200)
    const json = await readJson(get)
    expect(json.accounts[0]).toMatchObject({
      label: "Work Claude",
      custom_label: "Work Claude",
      account: { email: "upstream@example.com" },
    })
  })

  it("survives a subsequent accounts read that syncs a changed upstream identity", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const accountId = await seedAccount(db, "user_1")

    const patch = await providerRoutes.request(
      `/claude-code/accounts/${accountId}`,
      req("PATCH", cookie, { custom_label: "Keep this name" }),
      env,
    )
    expect(patch.status).toBe(200)

    stubClaudeUsage("first@example.com")
    await providerRoutes.request("/claude-code/accounts", req("GET", cookie), env)
    stubClaudeUsage("changed@example.com")
    // ?refresh=true: the identity sync under test only runs on a live fetch,
    // and a plain second read inside 60s is served from the usage cache.
    const get = await providerRoutes.request(
      "/claude-code/accounts?refresh=true",
      req("GET", cookie),
      env,
    )

    const json = await readJson(get)
    expect(json.accounts[0]).toMatchObject({
      label: "Keep this name",
      custom_label: "Keep this name",
      account: { email: "changed@example.com" },
    })
    expect(db.rows("upstream_accounts")[0]).toMatchObject({
      label: "changed@example.com",
      custom_label: "Keep this name",
    })
  })

  it("clears custom_label with null and empty string and falls back to upstream identity", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const accountId = await seedAccount(db, "user_1")
    stubClaudeUsage("fallback@example.com")

    const set = await providerRoutes.request(
      `/claude-code/accounts/${accountId}`,
      req("PATCH", cookie, { custom_label: "Temporary name" }),
      env,
    )
    expect(set.status).toBe(200)

    const clearWithNull = await providerRoutes.request(
      `/claude-code/accounts/${accountId}`,
      req("PATCH", cookie, { custom_label: null }),
      env,
    )
    expect(clearWithNull.status).toBe(200)
    expect(await readJson(clearWithNull)).toEqual({ ok: true, custom_label: null })
    const afterNull = await providerRoutes.request(
      "/claude-code/accounts",
      req("GET", cookie),
      env,
    )
    expect((await readJson(afterNull)).accounts[0]).toMatchObject({
      label: "fallback@example.com",
      custom_label: null,
    })

    const setAgain = await providerRoutes.request(
      `/claude-code/accounts/${accountId}`,
      req("PATCH", cookie, { custom_label: "Temporary again" }),
      env,
    )
    expect(setAgain.status).toBe(200)
    const clearWithEmpty = await providerRoutes.request(
      `/claude-code/accounts/${accountId}`,
      req("PATCH", cookie, { custom_label: "" }),
      env,
    )
    expect(clearWithEmpty.status).toBe(200)
    expect(await readJson(clearWithEmpty)).toEqual({ ok: true, custom_label: null })

    const afterEmpty = await providerRoutes.request(
      "/claude-code/accounts",
      req("GET", cookie),
      env,
    )
    expect((await readJson(afterEmpty)).accounts[0]).toMatchObject({
      label: "fallback@example.com",
      custom_label: null,
    })
  })

  it("rejects a custom_label longer than 64 characters", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const accountId = await seedAccount(db, "user_1")

    const res = await providerRoutes.request(
      `/claude-code/accounts/${accountId}`,
      req("PATCH", cookie, { custom_label: "x".repeat(65) }),
      env,
    )
    expect(res.status).toBe(400)
    expect(await readJson(res)).toEqual({ error: "custom_label too long" })
  })

  it("rejects a non-string non-null custom_label", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const accountId = await seedAccount(db, "user_1")

    const res = await providerRoutes.request(
      `/claude-code/accounts/${accountId}`,
      req("PATCH", cookie, { custom_label: 42 }),
      env,
    )
    expect(res.status).toBe(400)
    expect(await readJson(res)).toEqual({ error: "invalid custom_label" })
  })

  it("returns 404 for another user's account id", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    seedUser(db, "user_2")
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const accountId = await seedAccount(db, "user_2", { id: "acc_user_2" })

    const res = await providerRoutes.request(
      `/claude-code/accounts/${accountId}`,
      req("PATCH", cookie, { custom_label: "not yours" }),
      env,
    )
    expect(res.status).toBe(404)
    expect(db.rows("upstream_accounts")[0]!.custom_label).toBeNull()
  })

  it("requires authentication", async () => {
    const db = new FakeD1()
    const env = buildEnv(db)

    const res = await providerRoutes.request(
      "/claude-code/accounts/acc_1",
      req("PATCH", undefined, { custom_label: "no session" }),
      env,
    )
    expect(res.status).toBe(401)
  })
})

/**
 * Server-side 60s usage cache (docs/providers.md § Usage cache). The point is
 * that N devices cost one upstream call, not N, so most of these assert on the
 * upstream call count rather than on the response body.
 */
describe("GET /api/providers/:provider/accounts usage cache", () => {
  it("serves a second read from cache without calling upstream again", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    await seedAccount(db, "user_1")
    const usage = countingClaudeUsage()

    const first = await readJson(
      await providerRoutes.request("/claude-code/accounts", req("GET", cookie), env),
    )
    expect(usage.calls()).toBe(1)
    expect(first.accounts[0].usage.windows[0]).toMatchObject({ utilization: 10 })

    // A second device polling inside the TTL must not reach upstream, and must
    // see the same numbers the first one saw.
    const second = await readJson(
      await providerRoutes.request("/claude-code/accounts", req("GET", cookie), env),
    )
    expect(usage.calls()).toBe(1)
    expect(second.accounts[0].usage.windows[0]).toMatchObject({ utilization: 10 })
  })

  it("refetches once the snapshot is older than the TTL", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    await seedAccount(db, "user_1")
    const usage = countingClaudeUsage()

    await providerRoutes.request("/claude-code/accounts", req("GET", cookie), env)
    expect(usage.calls()).toBe(1)

    // Age the snapshot past 60s. A stale read fetches synchronously and returns
    // the fresh value — not the previous one with a background refresh.
    db.rows("upstream_accounts")[0]!.usage_fetched_at = new Date(
      Date.now() - 61_000,
    ).toISOString()
    const after = await readJson(
      await providerRoutes.request("/claude-code/accounts", req("GET", cookie), env),
    )
    expect(usage.calls()).toBe(2)
    expect(after.accounts[0].usage.windows[0]).toMatchObject({ utilization: 20 })
    expect(after.accounts[0].stale).toBe(false)
  })

  it("bypasses the cache for an explicit ?refresh=true", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    await seedAccount(db, "user_1")
    const usage = countingClaudeUsage()

    await providerRoutes.request("/claude-code/accounts", req("GET", cookie), env)
    const refreshed = await readJson(
      await providerRoutes.request("/claude-code/accounts?refresh=true", req("GET", cookie), env),
    )
    expect(usage.calls()).toBe(2)
    expect(refreshed.accounts[0].usage.windows[0]).toMatchObject({ utilization: 20 })
  })

  it("serves the stored snapshot when another request holds the lock", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    await seedAccount(db, "user_1")
    const usage = countingClaudeUsage()

    await providerRoutes.request("/claude-code/accounts", req("GET", cookie), env)
    expect(usage.calls()).toBe(1)

    // Stale snapshot + a lock someone else is holding: the loser must return
    // the old value rather than queue up a second upstream call.
    const row = db.rows("upstream_accounts")[0]!
    row.usage_fetched_at = new Date(Date.now() - 61_000).toISOString()
    row.usage_fetching_at = new Date().toISOString()

    const res = await readJson(
      await providerRoutes.request("/claude-code/accounts", req("GET", cookie), env),
    )
    expect(usage.calls()).toBe(1)
    expect(res.accounts[0].usage.windows[0]).toMatchObject({ utilization: 10 })
    expect(res.accounts[0].stale).toBe(true)
  })

  it("breaks a lock left behind by a dead request", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    await seedAccount(db, "user_1")
    const usage = countingClaudeUsage()

    // A lock older than 30s means its holder is gone; it must not wedge the
    // account's usage forever.
    db.rows("upstream_accounts")[0]!.usage_fetching_at = new Date(
      Date.now() - 31_000,
    ).toISOString()

    await providerRoutes.request("/claude-code/accounts", req("GET", cookie), env)
    expect(usage.calls()).toBe(1)
    expect(db.rows("upstream_accounts")[0]!.usage_fetching_at).toBeNull()
  })

  it("keeps the previous snapshot and releases the lock when upstream fails", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    await seedAccount(db, "user_1")
    countingClaudeUsage()

    await providerRoutes.request("/claude-code/accounts", req("GET", cookie), env)
    const stored = db.rows("upstream_accounts")[0]!.usage_snapshot_json
    expect(stored).toBeTruthy()

    db.rows("upstream_accounts")[0]!.usage_fetched_at = new Date(
      Date.now() - 61_000,
    ).toISOString()
    globalThis.fetch = (async () => {
      throw new Error("upstream down")
    }) as typeof fetch

    const res = await readJson(
      await providerRoutes.request("/claude-code/accounts", req("GET", cookie), env),
    )
    expect(res.accounts[0].error).toBe("upstream down")
    expect(res.accounts[0].usage.windows[0]).toMatchObject({ utilization: 10 })
    expect(res.accounts[0].stale).toBe(true)
    // One hiccup must not blank the bars for every device sharing this cache,
    // and must not leave the lock held.
    expect(db.rows("upstream_accounts")[0]!.usage_snapshot_json).toBe(stored)
    expect(db.rows("upstream_accounts")[0]!.usage_fetching_at).toBeNull()
  })

  it("keeps a cached unusable account unusable", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    await seedAccount(db, "user_1")

    globalThis.fetch = (async (input: RequestInfo | URL) => {
      const url = String(input)
      if (url.endsWith("/api/oauth/usage")) return new Response("nope", { status: 401 })
      if (url.endsWith("/api/oauth/profile")) return new Response("nope", { status: 401 })
      throw new Error(`unexpected fetch: ${url}`)
    }) as typeof fetch

    const live = await readJson(
      await providerRoutes.request("/claude-code/accounts", req("GET", cookie), env),
    )
    expect(live.accounts[0].status).toBe("unusable")

    // The snapshot stores error/edgeBlocked precisely so the cached read
    // re-derives the same status instead of silently reporting "active".
    const cached = await readJson(
      await providerRoutes.request("/claude-code/accounts", req("GET", cookie), env),
    )
    expect(cached.accounts[0].status).toBe("unusable")
  })
})
