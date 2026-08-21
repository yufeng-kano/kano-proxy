/**
 * End-to-end routing for `antigravity/*` on both LLM surfaces — the real Hono
 * handlers, apiKeyAuth, candidate resolution and dispatch down to a stubbed
 * `fetch`, not the adapter in isolation. Asserting on the adapter's return
 * value alone is what lets a routing-layer regression ship green.
 */
import { afterEach, describe, expect, it } from "vitest"
import { hashApiKey } from "../src/crypto/keys"
import { encryptJson } from "../src/crypto/token_crypto"
import type { Env } from "../src/env"
import { app } from "../src/index"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

const TOKEN_KEY = "test-token-encryption-key-not-secret"
const API_KEY_PLAINTEXT = "sk-kano-proxy-test-client-key-0001"
const DAILY = "https://daily-cloudcode-pa.googleapis.com"

const execCtx = {
  waitUntil: (p: Promise<unknown>) => {
    p.catch(() => {})
  },
  passThroughOnException: () => {},
} as unknown as ExecutionContext

function buildEnv(db: FakeD1): Env {
  return {
    DB: db as unknown as D1Database,
    BENCH: fakeKV(),
    CACHE: fakeKV(),
    APP_URL: "https://app.example.com",
    TOKEN_ENCRYPTION_KEY: TOKEN_KEY,
  } as unknown as Env
}

async function seed(db: FakeD1): Promise<void> {
  db.seed("api_keys", [
    {
      id: "key_1",
      user_id: "user_1",
      name: "test key",
      key_prefix: API_KEY_PLAINTEXT.slice(0, 20),
      key_hash: await hashApiKey(API_KEY_PLAINTEXT),
      created_at: "2026-01-01T00:00:00.000Z",
      last_used_at: null,
    },
  ])
  const encrypted = await encryptJson(TOKEN_KEY, {
    access_token: "at-1",
    refresh_token: "rt-1",
    expires_at: new Date(Date.now() + 3_600_000).toISOString(),
    extra: { project_id: "proj-42" },
  })
  db.seed("upstream_accounts", [
    {
      id: "acc_ag",
      user_id: "user_1",
      provider: "antigravity",
      external_account_id: null,
      label: "a@b.com",
      custom_label: null,
      priority: 1,
      encrypted_payload: encrypted,
      account_meta_json: null,
      usage_snapshot_json: null,
      usage_fetched_at: null,
      usage_fetching_at: null,
      bench_until: null,
      bench_reason: null,
      refreshing_at: null,
      edge_strikes: null,
      edge_strike_at: null,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
}

function authHeaders(): Record<string, string> {
  return { authorization: `Bearer ${API_KEY_PLAINTEXT}`, "content-type": "application/json" }
}

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

type Call = { url: string; init: RequestInit }

function stubUpstream(handler: (call: Call) => Response): Call[] {
  const calls: Call[] = []
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const call = { url: String(input), init: init ?? {} }
    calls.push(call)
    return handler(call)
  }) as typeof fetch
  return calls
}

function geminiOk(text: string): Response {
  return new Response(
    JSON.stringify({
      response: {
        candidates: [{ content: { role: "model", parts: [{ text }] }, finishReason: "STOP" }],
        usageMetadata: { promptTokenCount: 4, candidatesTokenCount: 2, totalTokenCount: 6 },
      },
    }),
    { status: 200, headers: { "content-type": "application/json" } },
  )
}

describe("antigravity routing — /openai/v1/chat/completions", () => {
  it("wraps the request in the CloudCode envelope and answers OpenAI-shaped", async () => {
    const db = new FakeD1()
    await seed(db)
    const calls = stubUpstream(() => geminiOk("hello"))

    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "antigravity/gemini-3-flash",
          messages: [
            { role: "system", content: "be terse" },
            { role: "user", content: "hi" },
          ],
        }),
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(200)
    expect(calls[0]!.url).toBe(`${DAILY}/v1internal:generateContent`)
    const sent = JSON.parse(calls[0]!.init.body as string) as {
      model: string
      project: string
      request: Record<string, unknown>
    }
    expect(sent.model).toBe("gemini-3-flash")
    expect(sent.project).toBe("proj-42")
    expect(sent.request.systemInstruction).toEqual({ role: "user", parts: [{ text: "be terse" }] })

    const json = (await res.json()) as Record<string, unknown>
    expect(json.model).toBe("antigravity/gemini-3-flash")
    expect(
      ((json.choices as Array<Record<string, unknown>>)[0].message as Record<string, unknown>)
        .content,
    ).toBe("hello")
  })

  it("logs one request_logs row as an OAuth-pool provider with real token counts", async () => {
    const db = new FakeD1()
    await seed(db)
    stubUpstream(() => geminiOk("hello"))

    await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "antigravity/gemini-3-flash",
          messages: [{ role: "user", content: "hi" }],
        }),
      },
      buildEnv(db),
      execCtx,
    )

    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      provider: "antigravity",
      model: "antigravity/gemini-3-flash",
      account_id: "acc_ag",
      status_code: 200,
      prompt_tokens: 4,
      completion_tokens: 2,
    })
  })

  it("benches the account until the reset the 429 body states", async () => {
    const db = new FakeD1()
    await seed(db)
    const before = Date.now()
    stubUpstream(
      () =>
        new Response(
          JSON.stringify({
            error: {
              status: "RESOURCE_EXHAUSTED",
              message: "out of quota",
              details: [
                { "@type": "type.googleapis.com/google.rpc.ErrorInfo", reason: "QUOTA_EXHAUSTED" },
                { "@type": "type.googleapis.com/google.rpc.RetryInfo", retryDelay: "1800s" },
              ],
            },
          }),
          { status: 429, headers: { "content-type": "application/json" } },
        ),
    )

    await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "antigravity/gemini-3-flash",
          messages: [{ role: "user", content: "hi" }],
        }),
      },
      buildEnv(db),
      execCtx,
    )

    const account = db.rows("upstream_accounts")[0]!
    const benchUntil = Date.parse(account.bench_until as string)
    // The routing module read the reset out of the adapter's hint header, not
    // the 300s default — 30 minutes, per the body's RetryInfo.
    expect(benchUntil).toBeGreaterThan(before + 29 * 60_000)
    expect(benchUntil).toBeLessThan(before + 31 * 60_000)
  })
})

describe("antigravity routing — /anthropic", () => {
  it("takes the converting-messages path, not the native passthrough", async () => {
    const db = new FakeD1()
    await seed(db)
    const calls = stubUpstream(() => geminiOk("hi there"))

    const res = await app.request(
      "/anthropic/v1/messages",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "antigravity/gemini-3-flash",
          system: "be terse",
          max_tokens: 64,
          messages: [{ role: "user", content: "hi" }],
        }),
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(200)
    // A Gemini endpoint, not an Anthropic one: the body was converted, not passed through.
    expect(calls[0]!.url).toBe(`${DAILY}/v1internal:generateContent`)
    const sent = JSON.parse(calls[0]!.init.body as string) as { request: Record<string, unknown> }
    expect(sent.request.contents).toEqual([{ role: "user", parts: [{ text: "hi" }] }])

    const json = (await res.json()) as Record<string, unknown>
    expect(json).toMatchObject({ type: "message", role: "assistant" })
    expect(json.model).toBe("antigravity/gemini-3-flash")
    expect(json.content).toEqual([{ type: "text", text: "hi there" }])
  })

  it("answers count_tokens from the real upstream count instead of a 400", async () => {
    const db = new FakeD1()
    await seed(db)
    const calls = stubUpstream(
      () => new Response(JSON.stringify({ totalTokens: 77 }), { status: 200 }),
    )

    const res = await app.request(
      "/anthropic/v1/messages/count_tokens",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "antigravity/gemini-3-flash",
          messages: [{ role: "user", content: "hi" }],
        }),
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(200)
    expect(calls[0]!.url).toBe(`${DAILY}/v1internal:countTokens`)
    expect(await res.json()).toEqual({ input_tokens: 77 })
  })
})
