import { afterEach, beforeEach, describe, expect, it } from "vitest"
import { createSession } from "../src/auth/session"
import type { Env } from "../src/env"
import { logsRoutes } from "../src/routes/logs"
import { _resetPricingForTests } from "../src/pricing/litellm"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

const SESSION_SECRET = "test-session-secret-not-real"

function buildEnv(db: FakeD1): Env {
  return {
    DB: db as unknown as D1Database,
    BENCH: fakeKV(),
    CACHE: fakeKV(),
    APP_URL: "https://app.example.com",
    SESSION_SECRET,
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

let logCounter = 0
function seedLog(
  db: FakeD1,
  overrides: Partial<Record<string, unknown>> & { user_id: string },
): void {
  db.seed("request_logs", [
    {
      id: `log_${++logCounter}`,
      api_key_id: null,
      provider: "claude-code",
      model: "claude-code/claude-opus-5",
      account_id: null,
      group_name: null,
      status_code: 200,
      upstream_status: null,
      error_code: null,
      latency_ms: 100,
      prompt_tokens: null,
      completion_tokens: null,
      cache_read_input_tokens: null,
      cache_creation_input_tokens: null,
      cost: null,
      created_at: "2026-08-14T12:00:00.000Z",
      ...overrides,
    },
  ])
}

function req(cookie?: string): RequestInit {
  return { method: "GET", headers: cookie ? { cookie } : {} }
}

const originalFetch = globalThis.fetch
beforeEach(() => {
  logCounter = 0
  _resetPricingForTests()
  globalThis.fetch = (async () => new Response("offline", { status: 500 })) as typeof fetch
})
afterEach(() => {
  globalThis.fetch = originalFetch
})

describe("GET /api/logs", () => {
  it("requires a valid session", async () => {
    const db = new FakeD1()
    const res = await logsRoutes.request("/", req(), buildEnv(db))
    expect(res.status).toBe(401)
    expect(await res.json()).toEqual({ error: "unauthorized" })
  })

  it("uses the default limit and rejects invalid limits", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    for (let i = 0; i < 51; i++) {
      seedLog(db, { user_id: "user_1", id: `log_${String(i).padStart(3, "0")}`, created_at: `2026-08-14T12:${String(i).padStart(2, "0")}:00.000Z` })
    }

    const defaultRes = await logsRoutes.request("/", req(cookie), env)
    expect(defaultRes.status).toBe(200)
    const json = (await defaultRes.json()) as { rows: unknown[]; next_cursor: string | null }
    expect(json.rows).toHaveLength(50)
    expect(json.next_cursor).toEqual(expect.any(String))

    for (const value of ["0", "101", "1.5", "-1", "abc", ""]) {
      const res = await logsRoutes.request(`/?limit=${encodeURIComponent(value)}`, req(cookie), env)
      expect(res.status).toBe(400)
      expect(await res.json()).toEqual({ error: "invalid_limit" })
    }
  })

  it("cursor-pages stably by created_at then id when timestamps are identical", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    for (const id of ["log_a", "log_c", "log_b", "log_d", "log_e"]) {
      seedLog(db, { user_id: "user_1", id, created_at: "2026-08-14T12:00:00.000Z" })
    }

    const first = await logsRoutes.request("/?limit=2", req(cookie), env)
    const firstJson = (await first.json()) as { rows: Array<{ id: string }>; next_cursor: string | null }
    expect(firstJson.rows.map((row) => row.id)).toEqual(["log_e", "log_d"])
    expect(firstJson.next_cursor).toEqual(expect.any(String))

    const second = await logsRoutes.request(`/?limit=2&cursor=${encodeURIComponent(firstJson.next_cursor!)}`, req(cookie), env)
    const secondJson = (await second.json()) as { rows: Array<{ id: string }>; next_cursor: string | null }
    expect(secondJson.rows.map((row) => row.id)).toEqual(["log_c", "log_b"])

    const third = await logsRoutes.request(`/?limit=2&cursor=${encodeURIComponent(secondJson.next_cursor!)}`, req(cookie), env)
    const thirdJson = (await third.json()) as { rows: Array<{ id: string }>; next_cursor: string | null }
    expect(thirdJson.rows.map((row) => row.id)).toEqual(["log_a"])
    expect(thirdJson.next_cursor).toBeNull()

    const invalid = await logsRoutes.request("/?cursor=not-a-cursor", req(cookie), env)
    expect(invalid.status).toBe(400)
    expect(await invalid.json()).toEqual({ error: "invalid_cursor" })
  })

  it("filters by exact provider and errors without live-provider scoping", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    seedLog(db, { user_id: "user_1", id: "builtin", provider: "claude-code", status_code: 200 })
    seedLog(db, { user_id: "user_1", id: "custom", provider: "deleted-custom", status_code: 200 })
    seedLog(db, { user_id: "user_1", id: "error-code", provider: "deleted-custom", error_code: "invalid_model" })
    seedLog(db, { user_id: "user_1", id: "error-status", provider: "deleted-custom", status_code: 503 })

    const providerRes = await logsRoutes.request("/?provider=deleted-custom", req(cookie), env)
    const providerJson = (await providerRes.json()) as { rows: Array<{ id: string }> }
    expect(providerJson.rows.map((row) => row.id).sort()).toEqual(["custom", "error-code", "error-status"])

    const errorsRes = await logsRoutes.request("/?errors=1", req(cookie), env)
    const errorsJson = (await errorsRes.json()) as { rows: Array<{ id: string }> }
    expect(errorsJson.rows.map((row) => row.id).sort()).toEqual(["error-code", "error-status"])
  })

  it("resolves display fields, derives usage type, and fills null cost at read time", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    await env.CACHE.put(
      "pricing:litellm:v1",
      JSON.stringify({
        fetchedAt: Date.now(),
        table: {
          "claude-opus-5": {
            input: 0.00001,
            output: 0.00005,
            cacheRead: null,
            cacheCreation: null,
          },
        },
        litellmTable: {},
        openRouterTable: {},
      }),
    )
    db.seed("upstream_accounts", [
      { id: "account_live", user_id: "user_1", custom_label: "Primary", label: "Upstream name" },
      { id: "account_fallback", user_id: "user_1", custom_label: null, label: "Fallback name" },
      { id: "other_account", user_id: "user_2", custom_label: "Leaked", label: "Leaked" },
    ])
    db.seed("api_keys", [
      { id: "key_live", user_id: "user_1", name: "Production" },
      { id: "other_key", user_id: "user_2", name: "Leaked" },
    ])
    seedLog(db, {
      user_id: "user_1",
      id: "oauth",
      provider: "claude-code",
      account_id: "account_live",
      api_key_id: "key_live",
      prompt_tokens: 100,
      completion_tokens: 10,
    })
    seedLog(db, {
      user_id: "user_1",
      id: "custom",
      provider: "deleted-custom",
      model: "deleted-custom/model",
      account_id: "account_fallback",
      api_key_id: "deleted_key",
    })
    seedLog(db, {
      user_id: "user_1",
      id: "deleted-account",
      account_id: "deleted_account",
      api_key_id: null,
    })

    const res = await logsRoutes.request("/?limit=10", req(cookie), env)
    expect(res.status).toBe(200)
    const json = (await res.json()) as { rows: Array<Record<string, unknown>>; next_cursor: string | null }
    // The api_keys id is resolved server-side to a name/removed flag and must
    // never leave the Worker (docs/admin-ui.md § Logs page) — assert its
    // absence on every row, not just the ones under test below.
    for (const row of json.rows) {
      expect(row).not.toHaveProperty("api_key_id")
    }

    const oauth = json.rows.find((row) => row.id === "oauth")!
    expect(oauth).toMatchObject({
      account_label: "Primary",
      api_key_name: "Production",
      api_key_removed: false,
      usage_type: "oauth",
    })
    expect(oauth.cost).toBeCloseTo(100 * 0.00001 + 10 * 0.00005, 12)
    // "custom" points at api_key_id "deleted_key", which never appears in
    // api_keys for this user — that is exactly what api_key_removed means.
    const custom = json.rows.find((row) => row.id === "custom")!
    expect(custom).toMatchObject({
      account_label: "Fallback name",
      api_key_name: null,
      api_key_removed: true,
      usage_type: "api",
      cost: null,
    })
    // "deleted-account" has a NULL api_key_id (never had a key attributed) —
    // that is not "removed", it is "not reported", so the flag stays false.
    const deleted = json.rows.find((row) => row.id === "deleted-account")!
    expect(deleted).toMatchObject({ account_label: null, api_key_name: null, api_key_removed: false })
  })
})
