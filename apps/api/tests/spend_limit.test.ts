import { afterEach, beforeEach, describe, expect, it } from "vitest"
import { app } from "../src/index"
import {
  _resetSpendMemoForTests,
  keyWindowSpend,
  keyWindowSpendCached,
  spendWindowStart,
} from "../src/auth/spend_limit"
import { hashApiKey } from "../src/crypto/keys"
import type { Env } from "../src/env"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

const API_KEY_PLAINTEXT = "sk-kano-proxy-test-client-key-0001"

const execCtx = {
  waitUntil: (p: Promise<unknown>) => {
    p.catch(() => {})
  },
  passThroughOnException: () => {},
} as unknown as ExecutionContext

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

beforeEach(() => {
  _resetSpendMemoForTests()
})

function buildEnv(db: FakeD1): Env {
  return {
    DB: db as unknown as D1Database,
    BENCH: fakeKV(),
    CACHE: fakeKV(),
    APP_URL: "https://app.example.com",
  } as unknown as Env
}

async function seedApiKey(
  db: FakeD1,
  opts?: { spendLimit?: number | null; interval?: string; includeOauth?: number },
): Promise<void> {
  db.seed("api_keys", [
    {
      id: "key_1",
      user_id: "user_1",
      name: "test key",
      key_prefix: API_KEY_PLAINTEXT.slice(0, 20),
      key_hash: await hashApiKey(API_KEY_PLAINTEXT),
      created_at: "2026-01-01T00:00:00.000Z",
      last_used_at: null,
      spend_limit: opts?.spendLimit ?? null,
      spend_limit_interval: opts?.interval ?? "monthly",
      spend_limit_include_oauth: opts?.includeOauth ?? 1,
    },
  ])
}

let logCounter = 0
function seedSpend(
  db: FakeD1,
  opts: { cost: number | null; provider?: string; createdAt?: string; apiKeyId?: string },
): void {
  db.seed("request_logs", [
    {
      id: `log_${logCounter++}`,
      user_id: "user_1",
      api_key_id: opts.apiKeyId ?? "key_1",
      provider: opts.provider ?? "claude-code",
      model: "claude-code/claude-opus-5",
      account_id: null,
      status_code: 200,
      latency_ms: 100,
      prompt_tokens: 100,
      completion_tokens: 10,
      cache_read_input_tokens: null,
      cache_creation_input_tokens: null,
      cost: opts.cost,
      error_code: null,
      created_at: opts.createdAt ?? new Date().toISOString(),
    },
  ])
}

function chatBody(): string {
  return JSON.stringify({
    model: "claude-code/claude-opus-5",
    messages: [{ role: "user", content: "hi" }],
  })
}

describe("spendWindowStart", () => {
  // 2026-08-04 is a Tuesday.
  const now = Date.parse("2026-08-04T15:30:00.000Z")

  it("daily → UTC midnight of today", () => {
    expect(spendWindowStart("daily", now)).toBe("2026-08-04T00:00:00.000Z")
  })

  it("weekly → most recent Monday 00:00 UTC", () => {
    expect(spendWindowStart("weekly", now)).toBe("2026-08-03T00:00:00.000Z")
    // A Monday is its own week start; Sunday reaches back six days.
    expect(spendWindowStart("weekly", Date.parse("2026-08-03T00:00:00.000Z"))).toBe(
      "2026-08-03T00:00:00.000Z",
    )
    expect(spendWindowStart("weekly", Date.parse("2026-08-09T23:59:59.000Z"))).toBe(
      "2026-08-03T00:00:00.000Z",
    )
  })

  it("monthly → 1st of the month 00:00 UTC", () => {
    expect(spendWindowStart("monthly", now)).toBe("2026-08-01T00:00:00.000Z")
  })

  it("total → epoch; unknown values fall back to monthly", () => {
    expect(spendWindowStart("total", now)).toBe("1970-01-01T00:00:00.000Z")
    expect(spendWindowStart("bogus", now)).toBe("2026-08-01T00:00:00.000Z")
  })
})

describe("keyWindowSpend", () => {
  const key = { id: "key_1", spend_limit_interval: "monthly", spend_limit_include_oauth: 1 }

  it("sums cost inside the window; NULL-cost rows contribute nothing", async () => {
    const db = new FakeD1()
    seedSpend(db, { cost: 1.25 })
    seedSpend(db, { cost: 0.75 })
    seedSpend(db, { cost: null })
    expect(await keyWindowSpend(buildEnv(db), key)).toBeCloseTo(2.0, 9)
  })

  it("excludes rows before the window start and rows from other keys", async () => {
    const db = new FakeD1()
    seedSpend(db, { cost: 5, createdAt: "2020-01-01T00:00:00.000Z" })
    seedSpend(db, { cost: 3, apiKeyId: "key_other" })
    seedSpend(db, { cost: 1 })
    expect(await keyWindowSpend(buildEnv(db), key)).toBeCloseTo(1, 9)
  })

  it("include_oauth=0 excludes builtin providers but keeps custom slugs", async () => {
    const db = new FakeD1()
    seedSpend(db, { cost: 10, provider: "claude-code" })
    seedSpend(db, { cost: 10, provider: "codex" })
    seedSpend(db, { cost: 10, provider: "grok" })
    seedSpend(db, { cost: 2.5, provider: "my-endpoint" })
    const spend = await keyWindowSpend(buildEnv(db), { ...key, spend_limit_include_oauth: 0 })
    expect(spend).toBeCloseTo(2.5, 9)
  })

  it("returns null on a D1 failure instead of throwing", async () => {
    const env = {
      DB: {
        prepare: () => {
          throw new Error("d1 down")
        },
      },
    } as unknown as Env
    expect(await keyWindowSpend(env, key)).toBeNull()
  })

  it("keyWindowSpendCached memoizes for the TTL", async () => {
    const db = new FakeD1()
    seedSpend(db, { cost: 1 })
    const env = buildEnv(db)
    expect(await keyWindowSpendCached(env, key)).toBeCloseTo(1, 9)
    seedSpend(db, { cost: 1 })
    // Second read within the TTL serves the memo, not the new row.
    expect(await keyWindowSpendCached(env, key)).toBeCloseTo(1, 9)
  })
})

describe("spend-limit enforcement (apiKeyAuth)", () => {
  it("429s a POST once window spend reaches the limit — OpenAI envelope", async () => {
    const db = new FakeD1()
    await seedApiKey(db, { spendLimit: 2 })
    seedSpend(db, { cost: 2.5 })
    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: { authorization: `Bearer ${API_KEY_PLAINTEXT}`, "content-type": "application/json" },
        body: chatBody(),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(429)
    const json = (await res.json()) as { error: { code: string; type: string } }
    expect(json.error.code).toBe("spend_limit_exceeded")
    expect(json.error.type).toBe("rate_limit_error")
  })

  it("429s on the Anthropic surface with the Anthropic envelope", async () => {
    const db = new FakeD1()
    await seedApiKey(db, { spendLimit: 1 })
    seedSpend(db, { cost: 1 }) // at the limit counts as reached
    const res = await app.request(
      "/anthropic/v1/messages",
      {
        method: "POST",
        headers: { "x-api-key": API_KEY_PLAINTEXT, "content-type": "application/json" },
        body: chatBody(),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(429)
    const json = (await res.json()) as { type: string; error: { type: string } }
    expect(json.type).toBe("error")
    expect(json.error.type).toBe("rate_limit_error")
  })

  it("logs the refusal as one spend_limit_exceeded row", async () => {
    const db = new FakeD1()
    await seedApiKey(db, { spendLimit: 1 })
    seedSpend(db, { cost: 5 })
    const before = db.rows("request_logs").length
    await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: { authorization: `Bearer ${API_KEY_PLAINTEXT}`, "content-type": "application/json" },
        body: chatBody(),
      },
      buildEnv(db),
      execCtx,
    )
    // waitUntil in the test exec context runs the promise; give it a turn.
    await new Promise((r) => setTimeout(r, 0))
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(before + 1)
    expect(rows[rows.length - 1]).toMatchObject({
      error_code: "spend_limit_exceeded",
      status_code: 429,
      cost: null,
    })
  })

  it("does not gate GET /models even when over the limit", async () => {
    const db = new FakeD1()
    await seedApiKey(db, { spendLimit: 1 })
    seedSpend(db, { cost: 5 })
    const res = await app.request(
      "/openai/v1/models",
      { method: "GET", headers: { authorization: `Bearer ${API_KEY_PLAINTEXT}` } },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)
  })

  it("under the limit passes through to dispatch", async () => {
    const db = new FakeD1()
    await seedApiKey(db, { spendLimit: 100 })
    seedSpend(db, { cost: 1 })
    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: { authorization: `Bearer ${API_KEY_PLAINTEXT}`, "content-type": "application/json" },
        body: chatBody(),
      },
      buildEnv(db),
      execCtx,
    )
    // Past the gate: fails later on "no upstream account", not 429.
    expect(res.status).toBe(400)
    const json = (await res.json()) as { error: { code: string } }
    expect(json.error.code).toBe("no_upstream_account")
  })

  it("no limit set → never checked, never 429", async () => {
    const db = new FakeD1()
    await seedApiKey(db, { spendLimit: null })
    seedSpend(db, { cost: 10_000 })
    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: { authorization: `Bearer ${API_KEY_PLAINTEXT}`, "content-type": "application/json" },
        body: chatBody(),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(400)
  })

  it("fails open when the window sum is unreadable (D1 failure)", async () => {
    const db = new FakeD1()
    await seedApiKey(db, { spendLimit: 1 })
    seedSpend(db, { cost: 5 })
    const env = buildEnv(db)
    const realPrepare = db.prepare.bind(db)
    ;(db as unknown as { prepare: (sql: string) => unknown }).prepare = (sql: string) => {
      if (/SUM\(cost\)/.test(sql)) throw new Error("d1 down")
      return realPrepare(sql)
    }
    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: { authorization: `Bearer ${API_KEY_PLAINTEXT}`, "content-type": "application/json" },
        body: chatBody(),
      },
      env,
      execCtx,
    )
    expect(res.status).toBe(400) // reached dispatch, not 429 and not 500
  })
})

describe("keys routes — limits API", () => {
  async function sessionEnv(db: FakeD1): Promise<{ env: Env; cookie: string }> {
    const env = {
      DB: db as unknown as D1Database,
      BENCH: fakeKV(),
      CACHE: fakeKV(),
      APP_URL: "https://app.example.com",
      SESSION_SECRET: "test-session-secret-not-real",
    } as unknown as Env
    db.seed("users", [
      {
        id: "user_1",
        google_sub: "sub-user_1",
        email: "user_1@example.com",
        name: "Test User",
        picture_url: null,
        created_at: "2026-01-01T00:00:00.000Z",
        updated_at: "2026-01-01T00:00:00.000Z",
      },
    ])
    const { createSession } = await import("../src/auth/session")
    const { cookie } = await createSession(env, "user_1")
    return { env, cookie: cookie.split(";")[0]! }
  }

  it("POST /api/keys accepts limit fields and echoes them (with the plaintext, once)", async () => {
    const db = new FakeD1()
    const { env, cookie } = await sessionEnv(db)
    const res = await app.request(
      "/api/keys",
      {
        method: "POST",
        headers: { cookie, "content-type": "application/json", origin: "https://app.example.com" },
        body: JSON.stringify({
          name: "ci key",
          spend_limit: 25,
          spend_limit_interval: "weekly",
          spend_limit_include_oauth: false,
        }),
      },
      env,
      execCtx,
    )
    expect(res.status).toBe(200)
    const json = (await res.json()) as Record<string, unknown>
    expect(json).toMatchObject({
      name: "ci key",
      spend_limit: 25,
      spend_limit_interval: "weekly",
      spend_limit_include_oauth: false,
    })
    expect(typeof json.key).toBe("string")
    expect(db.rows("api_keys")[0]).toMatchObject({
      spend_limit: 25,
      spend_limit_interval: "weekly",
      spend_limit_include_oauth: 0,
    })
  })

  it("POST rejects a non-positive or non-numeric limit and a bad interval", async () => {
    const db = new FakeD1()
    const { env, cookie } = await sessionEnv(db)
    for (const body of [
      { spend_limit: 0 },
      { spend_limit: -3 },
      { spend_limit: "ten" },
      { spend_limit: 5, spend_limit_interval: "hourly" },
      { spend_limit: 5, spend_limit_include_oauth: "yes" },
    ]) {
      const res = await app.request(
        "/api/keys",
        {
          method: "POST",
          headers: { cookie, "content-type": "application/json", origin: "https://app.example.com" },
          body: JSON.stringify(body),
        },
        env,
        execCtx,
      )
      expect(res.status).toBe(400)
    }
    expect(db.rows("api_keys")).toHaveLength(0)
  })

  it("GET /api/keys returns limit fields and the current window_spend", async () => {
    const db = new FakeD1()
    const { env, cookie } = await sessionEnv(db)
    await seedApiKey(db, { spendLimit: 50 })
    seedSpend(db, { cost: 3.2 })
    seedSpend(db, { cost: null })
    const res = await app.request("/api/keys", { headers: { cookie } }, env, execCtx)
    expect(res.status).toBe(200)
    const json = (await res.json()) as { keys: Array<Record<string, unknown>> }
    expect(json.keys).toHaveLength(1)
    expect(json.keys[0]).toMatchObject({
      spend_limit: 50,
      spend_limit_interval: "monthly",
      spend_limit_include_oauth: true,
    })
    expect(json.keys[0]!.window_spend).toBeCloseTo(3.2, 9)
    expect(json.keys[0]!.key_hash).toBeUndefined()
  })

  it("PATCH updates name and limits; spend_limit: null clears the limit", async () => {
    const db = new FakeD1()
    const { env, cookie } = await sessionEnv(db)
    await seedApiKey(db, { spendLimit: 50, interval: "monthly" })
    const res = await app.request(
      "/api/keys/key_1",
      {
        method: "PATCH",
        headers: { cookie, "content-type": "application/json", origin: "https://app.example.com" },
        body: JSON.stringify({ name: "renamed", spend_limit: null }),
      },
      env,
      execCtx,
    )
    expect(res.status).toBe(200)
    expect(db.rows("api_keys")[0]).toMatchObject({ name: "renamed", spend_limit: null })
  })

  it("PATCH 404s another user's key", async () => {
    const db = new FakeD1()
    const { env, cookie } = await sessionEnv(db)
    db.seed("api_keys", [
      {
        id: "key_other",
        user_id: "user_2",
        name: "not yours",
        key_prefix: "sk-kano-proxy-zzzzzz",
        key_hash: "other-hash",
        created_at: "2026-01-01T00:00:00.000Z",
        last_used_at: null,
        spend_limit: null,
        spend_limit_interval: "monthly",
        spend_limit_include_oauth: 1,
      },
    ])
    const res = await app.request(
      "/api/keys/key_other",
      {
        method: "PATCH",
        headers: { cookie, "content-type": "application/json", origin: "https://app.example.com" },
        body: JSON.stringify({ name: "hijack" }),
      },
      env,
      execCtx,
    )
    expect(res.status).toBe(404)
    expect(db.rows("api_keys")[0]!.name).toBe("not yours")
  })
})
