import { afterEach, beforeEach, describe, expect, it } from "vitest"
import type { Env } from "../src/env"
import { logRequest } from "../src/logging/request_log"
import {
  _resetPricingForTests,
  computeCost,
  ensureFreshPriceTable,
  estimateCost,
  getPriceTable,
  refreshPriceTable,
  resolveModelPrice,
  trimLiteLLMTable,
  trimOpenRouterTable,
  type PriceTable,
} from "../src/pricing/litellm"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

const originalFetch = globalThis.fetch

afterEach(() => {
  globalThis.fetch = originalFetch
})

beforeEach(() => {
  _resetPricingForTests()
})

function buildEnv(): Env {
  return { CACHE: fakeKV() } as unknown as Env
}

/** A plausible LiteLLM payload slice — rates are fixture values, not real prices. */
const OPENROUTER_JSON = {
  data: [
    {
      id: "z-ai/glm-5.2",
      pricing: {
        prompt: "0.00000076",
        completion: "0.00000242",
        input_cache_read: "0.00000014",
      },
    },
    {
      id: "conditional/model",
      pricing: { prompt: "0.000001", completion: "0.000002", overrides: [{}] },
    },
  ],
}

const LITELLM_JSON = {
  sample_spec: { input_cost_per_token: 0, output_cost_per_token: 0 },
  "claude-opus-5": {
    input_cost_per_token: 0.000015,
    output_cost_per_token: 0.000075,
    cache_read_input_token_cost: 0.0000015,
    cache_creation_input_token_cost: 0.00001875,
    litellm_provider: "anthropic",
    mode: "chat",
  },
  "gpt-4o-mini": {
    input_cost_per_token: 0.00000015,
    output_cost_per_token: 0.0000006,
    litellm_provider: "openai",
  },
  "xai/grok-4.5": {
    input_cost_per_token: 0.000003,
    output_cost_per_token: 0.000015,
  },
  "openrouter/openai/gpt-5.6-luna": {
    input_cost_per_token: 0.00000125,
    output_cost_per_token: 0.00001,
  },
  "no-rates-model": { litellm_provider: "openai", mode: "chat" },
}

function stubFetchOk(json: unknown = LITELLM_JSON): void {
  globalThis.fetch = (async () => Response.json(json)) as typeof fetch
}

function stubPricingFetch(
  litellm: unknown = LITELLM_JSON,
  openRouter: unknown = OPENROUTER_JSON,
): void {
  globalThis.fetch = (async (input: string | URL | Request) => {
    const url = String(input)
    return Response.json(url.includes("openrouter.ai") ? openRouter : litellm)
  }) as typeof fetch
}

describe("trimLiteLLMTable", () => {
  it("keeps only priced entries, lowercases keys, drops sample_spec", () => {
    const table = trimLiteLLMTable(LITELLM_JSON as Record<string, unknown>)
    expect(table["sample_spec"]).toBeUndefined()
    expect(table["no-rates-model"]).toBeUndefined()
    expect(table["claude-opus-5"]).toEqual({
      input: 0.000015,
      output: 0.000075,
      cacheRead: 0.0000015,
      cacheCreation: 0.00001875,
    })
    expect(table["gpt-4o-mini"]).toMatchObject({ cacheRead: null, cacheCreation: null })
  })

  it("ignores malformed entries", () => {
    const table = trimLiteLLMTable({
      good: { input_cost_per_token: 1e-6, output_cost_per_token: 2e-6 },
      bad1: "string",
      bad2: null,
      bad3: { input_cost_per_token: "not a number" },
    } as Record<string, unknown>)
    expect(Object.keys(table)).toEqual(["good"])
  })
})

describe("trimOpenRouterTable", () => {
  it("maps the public catalog default rates only under the complete OpenRouter id", () => {
    expect(trimOpenRouterTable(OPENROUTER_JSON)).toEqual({
      "openrouter/z-ai/glm-5.2": {
        input: 0.00000076,
        output: 0.00000242,
        cacheRead: 0.00000014,
        cacheCreation: null,
      },
    })
  })

  it("skips conditional catalog prices and malformed catalog responses", () => {
    expect(trimOpenRouterTable({ data: [{ id: "bad", pricing: { prompt: "nope" } }] })).toEqual({})
    expect(trimOpenRouterTable({ data: [] })).toEqual({})
  })
})

describe("resolveModelPrice", () => {
  const table = trimLiteLLMTable(LITELLM_JSON as Record<string, unknown>)

  it("matches the bare upstream id after stripping this proxy's provider prefix", () => {
    expect(resolveModelPrice(table, "claude-code/claude-opus-5")).toBeTruthy()
    expect(resolveModelPrice(table, "my-endpoint/gpt-4o-mini")).toBeTruthy()
  })

  it("strips a bracket variant suffix before matching", () => {
    expect(resolveModelPrice(table, "claude-code/claude-opus-5[1m]")).toBeTruthy()
    expect(resolveModelPrice(table, "claude-code/Claude-Opus-5[1M]")).toBeTruthy()
  })

  it("tries vendor-prefixed LiteLLM keys for non-OpenRouter upstream ids", () => {
    expect(resolveModelPrice(table, "grok/grok-4.5")).toEqual({
      input: 0.000003,
      output: 0.000015,
      cacheRead: null,
      cacheCreation: null,
    })
  })

  it("uses an exact OpenRouter catalog key and never falls back to another provider's rate", () => {
    const openRouterTable = trimOpenRouterTable(OPENROUTER_JSON)
    expect(resolveModelPrice(openRouterTable, "openrouter/z-ai/glm-5.2")).toEqual({
      input: 0.00000076,
      output: 0.00000242,
      cacheRead: 0.00000014,
      cacheCreation: null,
    })
    expect(resolveModelPrice(table, "openrouter/openai/gpt-5.6-luna")).toBeTruthy()
    expect(
      resolveModelPrice(
        { "z-ai/glm-5.2": { input: 1, output: 1, cacheRead: null, cacheCreation: null } },
        "openrouter/z-ai/glm-5.2",
      ),
    ).toBeNull()
  })

  it("progressively strips the upstream id's own path segments", () => {
    // A custom endpoint that namespaces its models: upstream id "openai/gpt-4o-mini".
    expect(resolveModelPrice(table, "byok/openai/gpt-4o-mini")).toBeTruthy()
  })

  it("returns null on no match — never a guessed rate", () => {
    expect(resolveModelPrice(table, "claude-code/some-unknown-model")).toBeNull()
    expect(resolveModelPrice(table, "")).toBeNull()
  })
})

describe("computeCost", () => {
  const price = {
    input: 0.00001,
    output: 0.00005,
    cacheRead: 0.000001,
    cacheCreation: 0.0000125,
  }

  it("splits prompt_tokens (a cache-inclusive total) into uncached + cached components", () => {
    const cost = computeCost(price, {
      promptTokens: 1000, // 700 uncached + 200 read + 100 creation
      completionTokens: 400,
      cacheReadInputTokens: 200,
      cacheCreationInputTokens: 100,
    })
    expect(cost).toBeCloseTo(700 * 0.00001 + 200 * 0.000001 + 100 * 0.0000125 + 400 * 0.00005, 12)
  })

  it("bills cached input at the plain input rate when the table has no cache rates", () => {
    const flat = { input: 0.00001, output: 0.00005, cacheRead: null, cacheCreation: null }
    const cost = computeCost(flat, {
      promptTokens: 1000,
      completionTokens: 0,
      cacheReadInputTokens: 400,
      cacheCreationInputTokens: 0,
    })
    expect(cost).toBeCloseTo(1000 * 0.00001, 12)
  })

  it("floors the uncached remainder at 0 against inconsistent upstream numbers", () => {
    const cost = computeCost(price, {
      promptTokens: 100,
      completionTokens: 0,
      cacheReadInputTokens: 150,
      cacheCreationInputTokens: 0,
    })
    expect(cost).toBeCloseTo(150 * 0.000001, 12)
  })

  it("treats a partially-null usage as zeros but an all-null usage as unknown", () => {
    expect(
      computeCost(price, {
        promptTokens: 100,
        completionTokens: null,
        cacheReadInputTokens: null,
        cacheCreationInputTokens: null,
      }),
    ).toBeCloseTo(100 * 0.00001, 12)
    expect(
      computeCost(price, {
        promptTokens: null,
        completionTokens: null,
        cacheReadInputTokens: null,
        cacheCreationInputTokens: null,
      }),
    ).toBeNull()
  })
})

describe("estimateCost", () => {
  it("returns null for an unpriced model", () => {
    const table: PriceTable = {}
    expect(
      estimateCost(table, "claude-code/whatever", {
        promptTokens: 100,
        completionTokens: 10,
        cacheReadInputTokens: 0,
        cacheCreationInputTokens: 0,
      }),
    ).toBeNull()
  })
})

describe("refresh / cache lifecycle", () => {
  it("getPriceTable is null before anything was ever fetched, and never fetches itself", async () => {
    const env = buildEnv()
    let fetched = false
    globalThis.fetch = (async () => {
      fetched = true
      return Response.json(LITELLM_JSON)
    }) as typeof fetch
    expect(await getPriceTable(env)).toBeNull()
    expect(fetched).toBe(false)
  })

  it("refreshPriceTable stores the trimmed table in KV; getPriceTable then serves it (memo cleared)", async () => {
    const env = buildEnv()
    stubFetchOk()
    const table = await refreshPriceTable(env)
    expect(table?.["claude-opus-5"]).toBeTruthy()

    _resetPricingForTests() // force the KV path
    const fromKv = await getPriceTable(env)
    expect(fromKv?.["claude-opus-5"]).toBeTruthy()
  })

  it("a failed refresh keeps the previous table (stale-serve)", async () => {
    const env = buildEnv()
    stubFetchOk()
    await refreshPriceTable(env)
    globalThis.fetch = (async () => new Response("boom", { status: 500 })) as typeof fetch
    const table = await refreshPriceTable(env)
    expect(table?.["claude-opus-5"]).toBeTruthy()
  })

  it("a refresh that trims to nothing keeps the previous table", async () => {
    const env = buildEnv()
    stubFetchOk()
    await refreshPriceTable(env)
    stubFetchOk({ sample_spec: {} })
    const table = await refreshPriceTable(env)
    expect(table?.["claude-opus-5"]).toBeTruthy()
  })

  it("a network error resolves to null rather than throwing when nothing was ever cached", async () => {
    const env = buildEnv()
    globalThis.fetch = (async () => {
      throw new Error("network down")
    }) as typeof fetch
    await expect(refreshPriceTable(env)).resolves.toBeNull()
  })

  it("ensureFreshPriceTable skips both source fetches while the stored tables are fresh", async () => {
    const env = buildEnv()
    let calls = 0
    globalThis.fetch = (async (input: string | URL | Request) => {
      calls++
      return Response.json(String(input).includes("openrouter.ai") ? OPENROUTER_JSON : LITELLM_JSON)
    }) as typeof fetch
    await ensureFreshPriceTable(env)
    await ensureFreshPriceTable(env)
    expect(calls).toBe(2)
  })

  it("stale-serves a source independently when the other source refresh fails", async () => {
    const env = buildEnv()
    stubPricingFetch()
    await refreshPriceTable(env)
    globalThis.fetch = (async (input: string | URL | Request) => {
      if (String(input).includes("openrouter.ai")) return new Response("down", { status: 500 })
      return Response.json(LITELLM_JSON)
    }) as typeof fetch

    const table = await refreshPriceTable(env)
    expect(resolveModelPrice(table!, "openrouter/z-ai/glm-5.2")).toEqual({
      input: 0.00000076,
      output: 0.00000242,
      cacheRead: 0.00000014,
      cacheCreation: null,
    })
  })

  it("keeps LiteLLM prices when OpenRouter is unavailable on the first refresh", async () => {
    const env = buildEnv()
    globalThis.fetch = (async (input: string | URL | Request) => {
      if (String(input).includes("openrouter.ai")) return new Response("down", { status: 500 })
      return Response.json(LITELLM_JSON)
    }) as typeof fetch

    const table = await refreshPriceTable(env)
    expect(table?.["claude-opus-5"]).toBeTruthy()
  })
})

describe("logRequest cost capture", () => {
  function envWithDb(db: FakeD1): Env {
    return { DB: db as unknown as D1Database, CACHE: fakeKV() } as unknown as Env
  }

  const baseEntry = {
    userId: "user_1",
    provider: "claude-code",
    model: "claude-code/claude-opus-5",
    statusCode: 200,
    latencyMs: 100,
  }

  it("writes the estimated cost when the table knows the model", async () => {
    const db = new FakeD1()
    const env = envWithDb(db)
    stubFetchOk()
    await refreshPriceTable(env)

    await logRequest(env, {
      ...baseEntry,
      promptTokens: 1000, // 800 uncached + 200 cache read
      completionTokens: 100,
      cacheReadInputTokens: 200,
      cacheCreationInputTokens: 0,
    })
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]!.cost).toBeCloseTo(
      800 * 0.000015 + 200 * 0.0000015 + 100 * 0.000075,
      12,
    )
  })

  it("writes NULL cost when no table is loaded, without failing the write", async () => {
    const db = new FakeD1()
    const env = envWithDb(db)
    await logRequest(env, { ...baseEntry, promptTokens: 1000, completionTokens: 100 })
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]!.cost).toBeNull()
  })

  it("writes NULL cost for an unpriced model and for all-NULL usage", async () => {
    const db = new FakeD1()
    const env = envWithDb(db)
    stubFetchOk()
    await refreshPriceTable(env)

    await logRequest(env, {
      ...baseEntry,
      model: "claude-code/never-heard-of-it",
      promptTokens: 1000,
      completionTokens: 100,
    })
    await logRequest(env, { ...baseEntry })
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(2)
    expect(rows[0]!.cost).toBeNull()
    expect(rows[1]!.cost).toBeNull()
  })
})
