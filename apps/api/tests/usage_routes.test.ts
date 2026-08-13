import { afterEach, beforeEach, describe, expect, it } from "vitest"
import {
  fillEstimatedCosts,
  filterToLiveProviders,
  summarizeUsageRows,
  usageRoutes,
} from "../src/routes/usage"
import { createSession } from "../src/auth/session"
import type { Env } from "../src/env"
import {
  _resetPricingForTests,
  getPriceTable,
  resolveModelPrice,
  trimLiteLLMTable,
} from "../src/pricing/litellm"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

type Row = {
  provider: string
  model: string
  status_code: number
  latency_ms: number
  prompt_tokens: number | null
  completion_tokens: number | null
  cache_read_input_tokens: number | null
  cache_creation_input_tokens: number | null
  cost: number | null
  created_at: string
}

// The summary route may fetch the LiteLLM table inline when KV holds none —
// never let a unit test reach the real network.
const originalFetch = globalThis.fetch
beforeEach(() => {
  _resetPricingForTests()
  globalThis.fetch = (async () => new Response("offline", { status: 500 })) as typeof fetch
})
afterEach(() => {
  globalThis.fetch = originalFetch
})

function row(overrides: Partial<Row>): Row {
  return {
    provider: "claude-code",
    model: "claude-code/claude-opus-5",
    status_code: 200,
    latency_ms: 100,
    prompt_tokens: null,
    completion_tokens: null,
    cache_read_input_tokens: null,
    cache_creation_input_tokens: null,
    cost: null,
    created_at: "2026-08-02T10:00:00.000Z",
    ...overrides,
  }
}

/** Loosely typed — this test asserts shape/values, not the response's own type. */
type SummaryJson = {
  days: number
  from: string
  totals: Record<string, unknown> & { requests: number; errors: number; avg_latency_ms: number | null }
  models: Array<Record<string, unknown>>
  series: Array<Record<string, unknown>>
}

describe("summarizeUsageRows", () => {
  it("computes requests/errors (status_code >= 400) over all rows", () => {
    const rows = [
      row({ status_code: 200 }),
      row({ status_code: 404 }),
      row({ status_code: 500 }),
    ]
    const out = summarizeUsageRows(rows, 7, "from") as SummaryJson
    expect(out.totals.requests).toBe(3)
    expect(out.totals.errors).toBe(2)
  })

  it("averages latency_ms across all rows, rounded, and null for zero rows", () => {
    const rows = [row({ latency_ms: 100 }), row({ latency_ms: 150 }), row({ latency_ms: 151 })]
    const out = summarizeUsageRows(rows, 7, "from") as SummaryJson
    expect(out.totals.avg_latency_ms).toBe(134) // 401/3 = 133.67 -> rounds to 134

    const empty = summarizeUsageRows([], 7, "from") as SummaryJson
    expect(empty.totals.avg_latency_ms).toBeNull()
    expect(empty.totals.requests).toBe(0)
  })

  it("sums prompt_tokens/completion_tokens over non-null rows only — a null row is excluded, not zero-summed", () => {
    const rows = [
      row({ prompt_tokens: 100, completion_tokens: 50 }),
      row({ prompt_tokens: 200, completion_tokens: null }), // completion unknown on this row alone
      row({ prompt_tokens: null, completion_tokens: null }), // count_tokens-shaped row: usage wholly unknown
    ]
    const out = summarizeUsageRows(rows, 7, "from") as SummaryJson
    expect(out.totals.prompt_tokens).toBe(300)
    expect(out.totals.completion_tokens).toBe(50)
    expect(out.totals.usage_known_requests).toBe(2)
  })

  it("cache_rate = SUM(cache_read)/SUM(prompt_tokens) only over rows where cache_read is known and prompt_tokens > 0", () => {
    const rows = [
      row({ prompt_tokens: 100, cache_read_input_tokens: 20 }), // counts toward the rate
      row({ prompt_tokens: 200, cache_read_input_tokens: 50 }), // counts toward the rate
      row({ prompt_tokens: 50, cache_read_input_tokens: null }), // cache unknown -> excluded from rate and its sum
      row({ prompt_tokens: 0, cache_read_input_tokens: 10 }), // prompt_tokens not > 0 -> excluded from the rate only
    ]
    const out = summarizeUsageRows(rows, 7, "from") as SummaryJson
    // cache_read_input_tokens total sums every non-null row unconditionally (20+10, the 0-prompt row included).
    expect(out.totals.cache_read_input_tokens).toBe(80)
    expect(out.totals.cache_rate).toBeCloseTo((20 + 50) / (100 + 200))
    expect(out.totals.cache_known_requests).toBe(3)
  })

  it("cache_rate is null when no row qualifies", () => {
    const rows = [row({ prompt_tokens: 100, cache_read_input_tokens: null })]
    const out = summarizeUsageRows(rows, 7, "from") as SummaryJson
    expect(out.totals.cache_rate).toBeNull()
  })

  it("groups models[] by (provider, model), sorted by prompt+completion tokens desc, without avg_latency_ms", () => {
    const rows = [
      row({
        provider: "claude-code",
        model: "claude-code/claude-opus-5",
        prompt_tokens: 10,
        completion_tokens: 5,
      }),
      row({ provider: "grok", model: "grok/grok-4.5", prompt_tokens: 100, completion_tokens: 50 }),
      row({
        provider: "claude-code",
        model: "claude-code/claude-opus-5",
        prompt_tokens: 20,
        completion_tokens: 5,
      }),
    ]
    const out = summarizeUsageRows(rows, 7, "from") as SummaryJson
    expect(out.models).toHaveLength(2)
    expect(out.models[0]).toMatchObject({
      provider: "grok",
      model: "grok/grok-4.5",
      requests: 1,
      prompt_tokens: 100,
      completion_tokens: 50,
    })
    expect(out.models[1]).toMatchObject({
      provider: "claude-code",
      model: "claude-code/claude-opus-5",
      requests: 2,
      prompt_tokens: 30,
      completion_tokens: 10,
    })
    expect(out.models[0]!.avg_latency_ms).toBeUndefined()
  })

  it("buckets series by hour when days=1, by day otherwise", () => {
    const rows = [
      row({ created_at: "2026-08-02T10:15:00.000Z", prompt_tokens: 10, completion_tokens: 1 }),
      row({ created_at: "2026-08-02T10:45:00.000Z", prompt_tokens: 20, completion_tokens: 2 }),
      row({ created_at: "2026-08-02T12:00:00.000Z", prompt_tokens: 30, completion_tokens: 3 }),
    ]
    const hourly = summarizeUsageRows(rows, 1, "from") as SummaryJson
    expect(hourly.series).toEqual([
      {
        bucket: "2026-08-02T10",
        provider: "claude-code",
        model: "claude-code/claude-opus-5",
        requests: 2,
        prompt_tokens: 30,
        completion_tokens: 3,
        cache_read_input_tokens: 0,
        cache_known_requests: 0,
        cost: null,
      },
      {
        bucket: "2026-08-02T12",
        provider: "claude-code",
        model: "claude-code/claude-opus-5",
        requests: 1,
        prompt_tokens: 30,
        completion_tokens: 3,
        cache_read_input_tokens: 0,
        cache_known_requests: 0,
        cost: null,
      },
    ])
    const daily = summarizeUsageRows(rows, 7, "from") as SummaryJson
    expect(daily.series).toEqual([
      {
        bucket: "2026-08-02",
        provider: "claude-code",
        model: "claude-code/claude-opus-5",
        requests: 3,
        prompt_tokens: 60,
        completion_tokens: 6,
        cache_read_input_tokens: 0,
        cache_known_requests: 0,
        cost: null,
      },
    ])
  })

  it("series is sparse (no zero-fill) and ascending", () => {
    const rows = [
      row({ created_at: "2026-08-03T00:00:00.000Z" }),
      row({ created_at: "2026-08-01T00:00:00.000Z" }),
    ]
    const out = summarizeUsageRows(rows, 7, "from") as SummaryJson
    expect(out.series.map((s) => s.bucket)).toEqual(["2026-08-01", "2026-08-03"])
  })

  it("splits a bucket into one point per (provider, model), ascending within the bucket", () => {
    const rows = [
      row({ model: "claude-code/claude-opus-5", prompt_tokens: 10, completion_tokens: 1 }),
      row({ model: "claude-code/claude-sonnet-5", prompt_tokens: 20, completion_tokens: 2 }),
      row({ model: "claude-code/claude-opus-5", prompt_tokens: 30, completion_tokens: 3 }),
      row({ provider: "grok", model: "grok/grok-4.5", prompt_tokens: 40, completion_tokens: 4 }),
    ]
    const out = summarizeUsageRows(rows, 7, "from") as SummaryJson
    // One bucket, three models -> three points; sorted by provider then model.
    expect(out.series.map((s) => [s.bucket, s.model, s.requests, s.prompt_tokens])).toEqual([
      ["2026-08-02", "claude-code/claude-opus-5", 2, 40],
      ["2026-08-02", "claude-code/claude-sonnet-5", 1, 20],
      ["2026-08-02", "grok/grok-4.5", 1, 40],
    ])
    // Bucket totals are the client-side sum over its model points.
    const bucketPrompt = out.series.reduce((sum, s) => sum + (s.prompt_tokens as number), 0)
    expect(bucketPrompt).toBe(100) // 10 + 30 + 20 + 40
  })

  it("counts cache_known_requests per series point so 0% cached is distinguishable from unreported", () => {
    const rows = [
      row({ prompt_tokens: 100, cache_read_input_tokens: 25 }),
      row({ prompt_tokens: 100, cache_read_input_tokens: 0 }), // reported, genuinely 0 cached
      row({ prompt_tokens: 100, cache_read_input_tokens: null }), // never reported
    ]
    const out = summarizeUsageRows(rows, 7, "from") as SummaryJson
    expect(out.series).toHaveLength(1)
    expect(out.series[0]!.requests).toBe(3)
    expect(out.series[0]!.cache_known_requests).toBe(2)
    expect(out.series[0]!.cache_read_input_tokens).toBe(25)
  })

  it("echoes days and from verbatim", () => {
    const out = summarizeUsageRows([], 30, "2026-07-03T00:00:00.000Z") as SummaryJson
    expect(out.days).toBe(30)
    expect(out.from).toBe("2026-07-03T00:00:00.000Z")
  })
})

const SESSION_SECRET = "test-session-secret-not-real"
const APP_URL = "https://app.example.com"

function buildEnv(db: FakeD1): Env {
  return {
    DB: db as unknown as D1Database,
    BENCH: fakeKV(),
    CACHE: fakeKV(),
    APP_URL,
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
function seedLog(db: FakeD1, overrides: Partial<Row> & { user_id: string; error_code?: string }): void {
  db.seed("request_logs", [
    {
      id: `log_${logCounter++}`,
      api_key_id: null,
      provider: "claude-code",
      model: "claude-code/claude-opus-5",
      account_id: null,
      status_code: 200,
      latency_ms: 100,
      prompt_tokens: null,
      completion_tokens: null,
      cache_read_input_tokens: null,
      cache_creation_input_tokens: null,
      error_code: null,
      created_at: "2026-08-02T00:00:00.000Z",
      ...overrides,
    },
  ])
}

function req(cookie?: string): RequestInit {
  return { method: "GET", headers: cookie ? { cookie } : {} }
}

describe("GET /api/usage/summary", () => {
  it("requires auth", async () => {
    const db = new FakeD1()
    const res = await usageRoutes.request("/summary", req(), buildEnv(db))
    expect(res.status).toBe(401)
  })

  it("rejects a days value outside {1,7,30}", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await usageRoutes.request("/summary?days=3", req(cookie), env)
    expect(res.status).toBe(400)
    expect(await res.json()).toEqual({ error: "invalid_days" })
  })

  it("rejects a non-numeric days value", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await usageRoutes.request("/summary?days=abc", req(cookie), env)
    expect(res.status).toBe(400)
  })

  it("defaults to days=7 when no query param is given", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await usageRoutes.request("/summary", req(cookie), env)
    expect(res.status).toBe(200)
    const json = (await res.json()) as SummaryJson
    expect(json.days).toBe(7)
  })

  it("accepts days=1 and days=30", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    for (const days of [1, 30]) {
      const res = await usageRoutes.request(`/summary?days=${days}`, req(cookie), env)
      expect(res.status).toBe(200)
      const json = (await res.json()) as SummaryJson
      expect(json.days).toBe(days)
    }
  })

  it("scopes rows to the signed-in user — another user's rows never leak", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    seedUser(db, "user_2")
    seedLog(db, { user_id: "user_1", prompt_tokens: 10, completion_tokens: 5 })
    seedLog(db, { user_id: "user_2", prompt_tokens: 999, completion_tokens: 999 })
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")

    const res = await usageRoutes.request("/summary?days=30", req(cookie), env)
    const json = (await res.json()) as SummaryJson
    expect(json.totals.requests).toBe(1)
    expect(json.totals.prompt_tokens).toBe(10)
  })

  it("excludes rows created before the `from` window (boundary)", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const now = Date.now()
    // Comfortably inside / outside a 7-day window on either side of the cut,
    // clear of clock-skew between this call and the route's own Date.now().
    const inWindow = new Date(now - 6 * 86_400_000).toISOString()
    const outOfWindow = new Date(now - 8 * 86_400_000).toISOString()
    seedLog(db, { user_id: "user_1", created_at: inWindow, prompt_tokens: 10, completion_tokens: 1 })
    seedLog(db, {
      user_id: "user_1",
      created_at: outOfWindow,
      prompt_tokens: 999,
      completion_tokens: 999,
    })

    const res = await usageRoutes.request("/summary?days=7", req(cookie), env)
    const json = (await res.json()) as SummaryJson
    expect(json.totals.requests).toBe(1)
    expect(json.totals.prompt_tokens).toBe(10)
  })

  it("end-to-end: mixed NULLs, errors, multiple providers/models, matches totals/models/series math", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")

    // Relative to the real wall clock, not a hardcoded date, so this stays
    // green regardless of when it runs (days=7 below is a window off
    // Date.now() in the route). Floored to UTC midnight first so the two
    // "same calendar day" rows and the "next calendar day" row can't drift
    // across a day boundary depending on what hour the test happens to run
    // at; day1 sits 3 days back (well inside 7) and day2 = day1 + exactly
    // 24h (still well inside 7, and always the next calendar date — a UTC
    // day is always 24h, no DST to fight).
    const today = new Date()
    const day1 = new Date(
      Date.UTC(today.getUTCFullYear(), today.getUTCMonth(), today.getUTCDate() - 3, 9, 0, 0),
    )
    const day1Later = new Date(day1.getTime() + 30 * 60_000) // same day, +30min
    const day2 = new Date(day1.getTime() + 24 * 60 * 60_000) // next calendar day
    const day1Bucket = day1.toISOString().slice(0, 10)
    const day2Bucket = day2.toISOString().slice(0, 10)

    seedLog(db, {
      user_id: "user_1",
      provider: "claude-code",
      model: "claude-code/claude-opus-5",
      status_code: 200,
      latency_ms: 100,
      prompt_tokens: 100,
      completion_tokens: 40,
      cache_read_input_tokens: 20,
      cache_creation_input_tokens: 0,
      created_at: day1.toISOString(),
    })
    seedLog(db, {
      user_id: "user_1",
      provider: "grok",
      model: "grok/grok-4.5",
      status_code: 500,
      latency_ms: 300,
      prompt_tokens: null,
      completion_tokens: null,
      cache_read_input_tokens: null,
      cache_creation_input_tokens: null,
      error_code: "upstream_error",
      created_at: day1Later.toISOString(),
    })
    seedLog(db, {
      user_id: "user_1",
      provider: "claude-code",
      model: "claude-code/claude-opus-5",
      status_code: 200,
      latency_ms: 200,
      // count_tokens-shaped row: never carries tokens.
      prompt_tokens: null,
      completion_tokens: null,
      cache_read_input_tokens: null,
      cache_creation_input_tokens: null,
      created_at: day2.toISOString(),
    })

    const res = await usageRoutes.request("/summary?days=7", req(cookie), env)
    expect(res.status).toBe(200)
    const json = (await res.json()) as SummaryJson
    expect(json.totals).toMatchObject({
      requests: 3,
      errors: 1,
      avg_latency_ms: 200, // (100+300+200)/3
      prompt_tokens: 100,
      completion_tokens: 40,
      cache_read_input_tokens: 20,
      cache_creation_input_tokens: 0,
      cache_rate: 0.2, // 20/100
      usage_known_requests: 1,
      cache_known_requests: 1,
    })
    expect(json.models).toHaveLength(2)
    const claude = json.models.find((m) => m.provider === "claude-code")!
    expect(claude).toMatchObject({ model: "claude-code/claude-opus-5", requests: 2, errors: 0 })
    const grok = json.models.find((m) => m.provider === "grok")!
    expect(grok).toMatchObject({ model: "grok/grok-4.5", requests: 1, errors: 1 })
    // day1 holds two models (claude-code + grok) -> two points, not one.
    expect(json.series).toEqual([
      {
        bucket: day1Bucket,
        provider: "claude-code",
        model: "claude-code/claude-opus-5",
        requests: 1,
        prompt_tokens: 100,
        completion_tokens: 40,
        cache_read_input_tokens: 20,
        cache_known_requests: 1,
        cost: null,
      },
      {
        bucket: day1Bucket,
        provider: "grok",
        model: "grok/grok-4.5",
        requests: 1,
        prompt_tokens: 0,
        completion_tokens: 0,
        cache_read_input_tokens: 0,
        cache_known_requests: 0,
        cost: null,
      },
      {
        bucket: day2Bucket,
        provider: "claude-code",
        model: "claude-code/claude-opus-5",
        requests: 1,
        prompt_tokens: 0,
        completion_tokens: 0,
        cache_read_input_tokens: 0,
        cache_known_requests: 0,
        cost: null,
      },
    ])
  })

  it("excludes rows from providers that no longer exist, keeps live custom slugs", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    db.seed("custom_providers", [
      {
        id: "cprov_1",
        user_id: "user_1",
        slug: "my-endpoint",
        name: "My endpoint",
        format: "openai",
        base_url: "https://api.example.com/v1",
        models_mode: "auto",
        manual_models_json: null,
        created_at: "2026-08-01T00:00:00.000Z",
        updated_at: "2026-08-01T00:00:00.000Z",
      },
    ])
    seedLog(db, { user_id: "user_1", provider: "claude-code", prompt_tokens: 10, completion_tokens: 1 })
    seedLog(db, { user_id: "user_1", provider: "my-endpoint", model: "my-endpoint/x", prompt_tokens: 20, completion_tokens: 2 })
    // A deleted endpoint's slug and an invalid-model 400's "unknown" prefix.
    seedLog(db, { user_id: "user_1", provider: "deleted-endpoint", model: "deleted-endpoint/x" })
    seedLog(db, { user_id: "user_1", provider: "unknown", model: "gibberish" })

    const res = await usageRoutes.request("/summary?days=30", req(cookie), env)
    const json = (await res.json()) as SummaryJson
    expect(json.totals.requests).toBe(2)
    expect(json.models.map((m) => m.provider).sort()).toEqual(["claude-code", "my-endpoint"])
  })

  it("computes cost totals from stored per-row costs", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    seedLog(db, { user_id: "user_1", prompt_tokens: 100, completion_tokens: 10, cost: 1.5 })
    seedLog(db, { user_id: "user_1", prompt_tokens: 100, completion_tokens: 10, cost: 0.5 })
    seedLog(db, { user_id: "user_1", prompt_tokens: 100, completion_tokens: 10 }) // unpriced

    const res = await usageRoutes.request("/summary?days=30", req(cookie), env)
    const json = (await res.json()) as SummaryJson
    expect(json.totals.cost).toBeCloseTo(2.0, 9)
    expect(json.totals.cost_known_requests).toBe(2)
  })

  it("refreshes a legacy snapshot from the admin summary and prices OpenRouter after catalog fetch", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    await env.CACHE.put(
      "pricing:litellm:v1",
      JSON.stringify({
        fetchedAt: Date.now(),
        table: {
          "openrouter/z-ai/glm-5.2": { input: 1, output: 1, cacheRead: null, cacheCreation: null },
        },
      }),
    )
    db.seed("custom_providers", [
      {
        id: "cprov_openrouter",
        user_id: "user_1",
        slug: "openrouter",
        name: "OpenRouter",
        format: "openai",
        base_url: "https://api.example.com/v1",
        models_mode: "auto",
        manual_models_json: null,
        created_at: "2026-08-01T00:00:00.000Z",
        updated_at: "2026-08-01T00:00:00.000Z",
      },
    ])
    seedLog(db, {
      user_id: "user_1",
      provider: "openrouter",
      model: "openrouter/z-ai/glm-5.2",
      prompt_tokens: 100,
      completion_tokens: 10,
    })

    const legacy = await getPriceTable(env)
    expect(resolveModelPrice(legacy!, "openrouter/z-ai/glm-5.2")).toBeNull()

    let calls = 0
    globalThis.fetch = (async (input: string | URL | Request) => {
      calls++
      if (String(input).includes("openrouter.ai")) {
        return Response.json({
          data: [{ id: "z-ai/glm-5.2", pricing: { prompt: "0.000001", completion: "0.000002" } }],
        })
      }
      return Response.json({
        "claude-opus-5": { input_cost_per_token: 0.00001, output_cost_per_token: 0.00005 },
      })
    }) as typeof fetch

    const res = await usageRoutes.request("/summary?days=30", req(cookie), env)
    const json = (await res.json()) as SummaryJson
    expect(calls).toBe(2)
    expect(json.totals.cost).toBeCloseTo(100 * 0.000001 + 10 * 0.000002, 12)
    expect(json.totals.cost_known_requests).toBe(1)
  })
})

describe("filterToLiveProviders / fillEstimatedCosts", () => {
  it("filterToLiveProviders keeps builtins and given slugs only", () => {
    const rows = [
      row({ provider: "claude-code" }),
      row({ provider: "codex" }),
      row({ provider: "grok" }),
      row({ provider: "live-slug" }),
      row({ provider: "dead-slug" }),
      row({ provider: "unknown" }),
    ]
    const out = filterToLiveProviders(rows, ["live-slug"])
    expect(out.map((r) => r.provider)).toEqual(["claude-code", "codex", "grok", "live-slug"])
  })

  it("fillEstimatedCosts prices NULL-cost rows at read time and never overwrites a stored cost", () => {
    const table = trimLiteLLMTable({
      "claude-opus-5": { input_cost_per_token: 0.00001, output_cost_per_token: 0.00005 },
    })
    const rows = [
      row({ prompt_tokens: 1000, completion_tokens: 100, cost: null }),
      row({ prompt_tokens: 1000, completion_tokens: 100, cost: 42 }),
      row({ model: "claude-code/unpriced-model", prompt_tokens: 1000, cost: null }),
    ]
    const out = fillEstimatedCosts(rows, table)
    expect(out[0]!.cost).toBeCloseTo(1000 * 0.00001 + 100 * 0.00005, 12)
    expect(out[1]!.cost).toBe(42)
    expect(out[2]!.cost).toBeNull()
    // No table at all → rows come back untouched.
    expect(fillEstimatedCosts(rows, null)).toBe(rows)
  })
})
