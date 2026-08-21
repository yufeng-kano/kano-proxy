import { afterEach, describe, expect, it } from "vitest"
import { antigravityAdapter, buildAntigravityEnvelope } from "../src/providers/antigravity"
import { ANTIGRAVITY_QUOTA_BENCH_MS } from "../src/providers/antigravity_limits"
import type { ChatCompletionRequest } from "../src/providers/types"
import type { AcquiredAccount } from "../src/pool/acquire"
import type { Env } from "../src/env"
import { RATELIMIT_RESET_HINT_HEADER } from "../src/routing/feedback"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

const DAILY = "https://daily-cloudcode-pa.googleapis.com"
const PROD = "https://cloudcode-pa.googleapis.com"

function buildEnv(): Env {
  return {
    DB: new FakeD1() as unknown as D1Database,
    BENCH: fakeKV(),
    CACHE: fakeKV(),
    APP_URL: "https://app.example.com",
    TOKEN_ENCRYPTION_KEY: "test-token-encryption-key-not-secret",
  } as unknown as Env
}

/** An account whose token is fresh and whose project is already resolved, so
 *  neither the refresh nor the bootstrap path fires during the test. */
function account(): AcquiredAccount {
  return {
    row: {
      id: "acc_1",
      user_id: "user_1",
      provider: "antigravity",
      encrypted_payload: "unused",
      refreshing_at: null,
    } as unknown as AcquiredAccount["row"],
    credential: {
      access_token: "at-1",
      refresh_token: "rt-1",
      expires_at: new Date(Date.now() + 3_600_000).toISOString(),
      extra: { project_id: "proj-42" },
    },
  }
}

function chatRequest(patch: Partial<ChatCompletionRequest> = {}): ChatCompletionRequest {
  return {
    model: "antigravity/gemini-3-flash",
    rawModel: "antigravity/gemini-3-flash",
    upstreamModel: "gemini-3-flash",
    messages: [{ role: "user", content: "hi" }],
    rawBody: {},
    ...patch,
  }
}

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

type Call = { url: string; init: RequestInit }

function stubFetch(handler: (call: Call) => Response | Promise<Response>): Call[] {
  const calls: Call[] = []
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const call = { url: String(input), init: init ?? {} }
    calls.push(call)
    return handler(call)
  }) as typeof fetch
  return calls
}

function okGenerate(text = "hello"): Response {
  return new Response(
    JSON.stringify({
      response: {
        candidates: [{ content: { role: "model", parts: [{ text }] }, finishReason: "STOP" }],
        usageMetadata: { promptTokenCount: 3, candidatesTokenCount: 1, totalTokenCount: 4 },
      },
    }),
    { status: 200, headers: { "content-type": "application/json" } },
  )
}

describe("buildAntigravityEnvelope", () => {
  it("puts model, project and the fixed userAgent outside the Gemini request", async () => {
    const envelope = await buildAntigravityEnvelope({
      model: "gemini-3-flash",
      projectId: "proj-42",
      request: { contents: [{ role: "user", parts: [{ text: "hi" }] }] },
    })
    expect(envelope).toMatchObject({
      model: "gemini-3-flash",
      project: "proj-42",
      userAgent: "antigravity",
      requestType: "agent",
      requestId: expect.stringMatching(/^agent-/),
    })
    expect((envelope.request as Record<string, unknown>).contents).toEqual([
      { role: "user", parts: [{ text: "hi" }] },
    ])
  })

  it("derives a session id that is stable for the same opening user text", async () => {
    const build = () =>
      buildAntigravityEnvelope({
        model: "gemini-3-flash",
        projectId: "p",
        request: { contents: [{ role: "user", parts: [{ text: "same opener" }] }] },
      })
    const [a, b] = await Promise.all([build(), build()])
    const idOf = (e: Record<string, unknown>) =>
      (e.request as Record<string, unknown>).sessionId as string
    expect(idOf(a)).toBe(idOf(b))
    expect(idOf(a)).toMatch(/^-\d+$/)

    const other = await buildAntigravityEnvelope({
      model: "gemini-3-flash",
      projectId: "p",
      request: { contents: [{ role: "user", parts: [{ text: "different opener" }] }] },
    })
    expect(idOf(other)).not.toBe(idOf(a))
  })

  it("prefers an explicit session id over the derived one", async () => {
    const envelope = await buildAntigravityEnvelope({
      model: "gemini-3-flash",
      projectId: "p",
      request: { contents: [] },
      sessionId: "conv-7",
    })
    expect((envelope.request as Record<string, unknown>).sessionId).toBe("conv-7")
  })

  it("omits project entirely when none is known", async () => {
    const envelope = await buildAntigravityEnvelope({
      model: "gemini-3-flash",
      projectId: "",
      request: { contents: [] },
    })
    expect(envelope).not.toHaveProperty("project")
  })

  it("drops maxOutputTokens for a Gemini model that also sends tools", async () => {
    const envelope = await buildAntigravityEnvelope({
      model: "gemini-3-flash",
      projectId: "p",
      request: {
        contents: [],
        tools: [{ functionDeclarations: [] }],
        generationConfig: { maxOutputTokens: 128, temperature: 0.5 },
      },
    })
    expect((envelope.request as { generationConfig: Record<string, unknown> }).generationConfig)
      .toEqual({ temperature: 0.5 })
  })

  it("keeps maxOutputTokens when there is no schema to conflict with", async () => {
    const envelope = await buildAntigravityEnvelope({
      model: "gemini-3-flash",
      projectId: "p",
      request: { contents: [], generationConfig: { maxOutputTokens: 128 } },
    })
    expect((envelope.request as { generationConfig: Record<string, unknown> }).generationConfig)
      .toEqual({ maxOutputTokens: 128 })
  })

  it("forces VALIDATED function calling for a Claude model behind Antigravity", async () => {
    const envelope = await buildAntigravityEnvelope({
      model: "claude-sonnet-4-6",
      projectId: "p",
      request: { contents: [], tools: [{ functionDeclarations: [] }] },
    })
    expect((envelope.request as Record<string, unknown>).toolConfig).toEqual({
      functionCallingConfig: { mode: "VALIDATED" },
    })
  })

  it("preserves an explicit NONE / ANY tool choice for a Claude model instead of forcing VALIDATED", async () => {
    // Overwriting an explicit mode could produce a tool call the client
    // prohibited (NONE) or text where a call was required (ANY).
    const none = await buildAntigravityEnvelope({
      model: "claude-sonnet-4-6",
      projectId: "p",
      request: {
        contents: [],
        tools: [{ functionDeclarations: [] }],
        toolConfig: { functionCallingConfig: { mode: "NONE" } },
      },
    })
    expect((none.request as Record<string, unknown>).toolConfig).toEqual({
      functionCallingConfig: { mode: "NONE" },
    })

    const forced = await buildAntigravityEnvelope({
      model: "claude-sonnet-4-6",
      projectId: "p",
      request: {
        contents: [],
        tools: [{ functionDeclarations: [] }],
        toolConfig: { functionCallingConfig: { mode: "ANY", allowedFunctionNames: ["search"] } },
      },
    })
    expect((forced.request as Record<string, unknown>).toolConfig).toEqual({
      functionCallingConfig: { mode: "ANY", allowedFunctionNames: ["search"] },
    })
  })

  it("upgrades the default AUTO tool choice to VALIDATED for a Claude model", async () => {
    const envelope = await buildAntigravityEnvelope({
      model: "claude-sonnet-4-6",
      projectId: "p",
      request: {
        contents: [],
        tools: [{ functionDeclarations: [] }],
        toolConfig: { functionCallingConfig: { mode: "AUTO" } },
      },
    })
    expect((envelope.request as Record<string, unknown>).toolConfig).toEqual({
      functionCallingConfig: { mode: "VALIDATED" },
    })
  })

  it("does not force VALIDATED for a tool-less Claude request with only a response schema", async () => {
    // Structured output without tools must not carry an unattached
    // function-calling config — the backend rejects that shape.
    const envelope = await buildAntigravityEnvelope({
      model: "claude-sonnet-4-6",
      projectId: "p",
      request: { contents: [], generationConfig: { responseSchema: { type: "object" } } },
    })
    expect(envelope.request as Record<string, unknown>).not.toHaveProperty("toolConfig")
  })

  it("uses the image request type for an image model", async () => {
    const envelope = await buildAntigravityEnvelope({
      model: "gemini-3.1-flash-image",
      projectId: "p",
      request: { contents: [] },
    })
    expect(envelope.requestType).toBe("image_gen")
  })
})

describe("antigravityAdapter.chatCompletions", () => {
  it("posts the envelope to the daily base URL with the Antigravity identity", async () => {
    const calls = stubFetch(() => okGenerate())
    const res = await antigravityAdapter.chatCompletions(buildEnv(), account(), chatRequest())

    expect(res.status).toBe(200)
    expect(calls).toHaveLength(1)
    expect(calls[0]!.url).toBe(`${DAILY}/v1internal:generateContent`)
    const headers = calls[0]!.init.headers as Record<string, string>
    expect(headers.authorization).toBe("Bearer at-1")
    expect(headers["user-agent"]).toMatch(/^antigravity\/hub\/\d+\.\d+\.\d+ darwin\/arm64$/)
    const body = JSON.parse(calls[0]!.init.body as string) as Record<string, unknown>
    expect(body.model).toBe("gemini-3-flash")
    expect(body.project).toBe("proj-42")

    const json = (await res.json()) as Record<string, unknown>
    // The client-visible model is echoed, not the bare upstream id.
    expect(json.model).toBe("antigravity/gemini-3-flash")
  })

  it("honours an ANTIGRAVITY_CLIENT_VERSION override in the User-Agent", async () => {
    const calls = stubFetch(() => okGenerate())
    const env = buildEnv()
    env.ANTIGRAVITY_CLIENT_VERSION = "9.9.9"
    await antigravityAdapter.chatCompletions(env, account(), chatRequest())
    expect((calls[0]!.init.headers as Record<string, string>)["user-agent"]).toBe(
      "antigravity/hub/9.9.9 darwin/arm64",
    )
  })

  it("uses the streaming path with alt=sse when the client asked to stream", async () => {
    const calls = stubFetch(
      () =>
        new Response('data: {"response":{"candidates":[{"finishReason":"STOP"}]}}\n\n', {
          status: 200,
          headers: { "content-type": "text/event-stream" },
        }),
    )
    const res = await antigravityAdapter.chatCompletions(
      buildEnv(),
      account(),
      chatRequest({ stream: true }),
    )
    expect(calls[0]!.url).toBe(`${DAILY}/v1internal:streamGenerateContent?alt=sse`)
    expect(res.headers.get("content-type")).toContain("text/event-stream")
  })

  it("falls back to the second base URL on a transport error", async () => {
    const calls = stubFetch((call) => {
      if (call.url.startsWith(DAILY)) throw new TypeError("network down")
      return okGenerate()
    })
    const res = await antigravityAdapter.chatCompletions(buildEnv(), account(), chatRequest())
    expect(res.status).toBe(200)
    expect(calls.map((c) => new URL(c.url).origin)).toEqual([DAILY, PROD])
  })

  it("falls back to the second base URL on a 429, then returns the last failure", async () => {
    const calls = stubFetch(
      () =>
        new Response(
          JSON.stringify({
            error: {
              status: "RESOURCE_EXHAUSTED",
              details: [
                { "@type": "type.googleapis.com/google.rpc.ErrorInfo", reason: "QUOTA_EXHAUSTED" },
              ],
            },
          }),
          { status: 429 },
        ),
    )
    const before = Date.now()
    const res = await antigravityAdapter.chatCompletions(buildEnv(), account(), chatRequest())

    expect(calls).toHaveLength(2)
    expect(res.status).toBe(429)
    // Quota exhaustion with no upstream reset benches for the documented hour.
    const hint = Number(res.headers.get(RATELIMIT_RESET_HINT_HEADER))
    expect(hint).toBeGreaterThanOrEqual(before + ANTIGRAVITY_QUOTA_BENCH_MS)
  })

  it("attaches the upstream retry delay as the reset hint on a transient throttle", async () => {
    const before = Date.now()
    stubFetch(
      () =>
        new Response(
          JSON.stringify({
            error: {
              status: "RESOURCE_EXHAUSTED",
              details: [
                {
                  "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                  reason: "RATE_LIMIT_EXCEEDED",
                },
                { "@type": "type.googleapis.com/google.rpc.RetryInfo", retryDelay: "30s" },
              ],
            },
          }),
          { status: 429 },
        ),
    )
    const res = await antigravityAdapter.chatCompletions(buildEnv(), account(), chatRequest())
    const hint = Number(res.headers.get(RATELIMIT_RESET_HINT_HEADER))
    expect(hint).toBeGreaterThanOrEqual(before + 30_000)
    expect(hint).toBeLessThan(before + 60_000)
  })

  it("returns the earlier 429 when the fallback base URL then fails at the transport layer", async () => {
    const calls = stubFetch((call) => {
      if (call.url.startsWith(DAILY)) {
        return new Response(
          JSON.stringify({
            error: {
              status: "RESOURCE_EXHAUSTED",
              details: [
                { "@type": "type.googleapis.com/google.rpc.ErrorInfo", reason: "QUOTA_EXHAUSTED" },
              ],
            },
          }),
          { status: 429 },
        )
      }
      throw new TypeError("network down")
    })
    const before = Date.now()
    // The saved HTTP answer must survive the later transport failure — a
    // thrown error would collapse into a generic 502 and skip the bench
    // classification plus candidate failover entirely.
    const res = await antigravityAdapter.chatCompletions(buildEnv(), account(), chatRequest())
    expect(calls).toHaveLength(2)
    expect(res.status).toBe(429)
    const hint = Number(res.headers.get(RATELIMIT_RESET_HINT_HEADER))
    expect(hint).toBeGreaterThanOrEqual(before + ANTIGRAVITY_QUOTA_BENCH_MS)
  })

  it("does not bench the account for a terminal no-capacity 429 — the hint is already expired", async () => {
    stubFetch(
      () =>
        new Response(
          JSON.stringify({ error: { status: "RESOURCE_EXHAUSTED", message: "no capacity" } }),
          { status: 429 },
        ),
    )
    const res = await antigravityAdapter.chatCompletions(buildEnv(), account(), chatRequest())
    expect(res.status).toBe(429)
    // Fleet-side condition, not this credential's: the hint must not put the
    // account on the 300s default bench, only let the failover walk continue.
    const hint = Number(res.headers.get(RATELIMIT_RESET_HINT_HEADER))
    expect(hint).toBeLessThanOrEqual(Date.now())
  })

  it("does not fall back or hint on a non-429 failure — the body passes through", async () => {
    const calls = stubFetch(
      () => new Response(JSON.stringify({ error: { message: "bad model" } }), { status: 404 }),
    )
    const res = await antigravityAdapter.chatCompletions(buildEnv(), account(), chatRequest())
    expect(calls).toHaveLength(1)
    expect(res.status).toBe(404)
    expect(res.headers.get(RATELIMIT_RESET_HINT_HEADER)).toBeNull()
    expect(await res.json()).toEqual({ error: { message: "bad model" } })
  })
})

describe("antigravityAdapter.messages", () => {
  it("converts an Anthropic body and echoes the client-visible model back", async () => {
    const calls = stubFetch(() => okGenerate("hi there"))
    const headers = new Headers({ "x-kano-raw-model": "antigravity/gemini-3-flash" })
    const res = await antigravityAdapter.messages!(
      buildEnv(),
      account(),
      { model: "gemini-3-flash", messages: [{ role: "user", content: "hi" }], max_tokens: 64 },
      headers,
    )

    const body = JSON.parse(calls[0]!.init.body as string) as {
      request: { contents: unknown[]; generationConfig: Record<string, unknown> }
    }
    expect(body.request.contents).toEqual([{ role: "user", parts: [{ text: "hi" }] }])
    expect(body.request.generationConfig).toMatchObject({ maxOutputTokens: 64 })

    const json = (await res.json()) as Record<string, unknown>
    expect(json).toMatchObject({ type: "message", model: "antigravity/gemini-3-flash" })
    expect(json.content).toEqual([{ type: "text", text: "hi there" }])
  })

  it("rejects a garbage reasoning_effort with a 400 before calling upstream", async () => {
    const calls = stubFetch(() => okGenerate())
    const res = await antigravityAdapter.messages!(
      buildEnv(),
      account(),
      { model: "gemini-3-flash", messages: [], reasoning_effort: "turbo" },
      new Headers(),
    )
    expect(res.status).toBe(400)
    expect(calls).toHaveLength(0)
  })

  it("sends a namespaced upstream id unchanged instead of re-splitting it", async () => {
    const calls = stubFetch(() => okGenerate())
    // The route already stripped the `antigravity/` provider prefix — a
    // second split would eat the first segment of a namespaced upstream id.
    await antigravityAdapter.messages!(
      buildEnv(),
      account(),
      { model: "org/model-x", messages: [{ role: "user", content: "hi" }], max_tokens: 8 },
      new Headers(),
    )
    const body = JSON.parse(calls[0]!.init.body as string) as Record<string, unknown>
    expect(body.model).toBe("org/model-x")
  })
})

describe("antigravityAdapter.countTokens", () => {
  it("posts the envelope without model or project and answers Anthropic-shaped", async () => {
    const calls = stubFetch(
      () => new Response(JSON.stringify({ totalTokens: 1234 }), { status: 200 }),
    )
    const res = await antigravityAdapter.countTokens!(
      buildEnv(),
      account(),
      { model: "gemini-3-flash", messages: [{ role: "user", content: "hi" }] },
      new Headers(),
    )

    expect(calls[0]!.url).toBe(`${DAILY}/v1internal:countTokens`)
    const body = JSON.parse(calls[0]!.init.body as string) as Record<string, unknown>
    expect(body).not.toHaveProperty("model")
    expect(body).not.toHaveProperty("project")
    expect(body.request).toBeDefined()
    expect(await res.json()).toEqual({ input_tokens: 1234 })
  })

  it("rejects an invalid reasoning_effort with a 400 before calling upstream", async () => {
    const calls = stubFetch(() => new Response(JSON.stringify({ totalTokens: 1 }), { status: 200 }))
    const res = await antigravityAdapter.countTokens!(
      buildEnv(),
      account(),
      { model: "gemini-3-flash", messages: [], reasoning_effort: "turbo" },
      new Headers(),
    )
    // Same contract as `messages`: a malformed client field is the client's
    // 400, never a 502 for an upstream call that was never made.
    expect(res.status).toBe(400)
    expect(calls).toHaveLength(0)
  })

  it("rejects a 200 without a usable totalTokens instead of fabricating zero", async () => {
    stubFetch(() => new Response(JSON.stringify({ totalTokens: "many" }), { status: 200 }))
    const res = await antigravityAdapter.countTokens!(
      buildEnv(),
      account(),
      { model: "gemini-3-flash", messages: [{ role: "user", content: "hi" }] },
      new Headers(),
    )
    // Clients budget context on this number — a malformed upstream payload
    // must be a detectable error, never a plausible `input_tokens: 0`.
    expect(res.status).toBe(502)
    expect(await res.json()).toMatchObject({ type: "error" })
  })
})

describe("antigravityAdapter.listModels", () => {
  it("reads the live fetchAvailableModels map, ids verbatim", async () => {
    const calls = stubFetch(
      () =>
        new Response(
          JSON.stringify({
            models: {
              "gemini-3.6-flash-high": { displayName: "Gemini 3.6 Flash" },
              "gemini-pro-agent": {},
            },
          }),
          { status: 200 },
        ),
    )
    const result = await antigravityAdapter.listModels!(buildEnv(), account())

    expect(calls[0]!.url).toBe(`${DAILY}/v1internal:fetchAvailableModels`)
    expect(JSON.parse(calls[0]!.init.body as string)).toEqual({ project: "proj-42" })
    expect(result).toEqual({
      models: [
        // The upstream id is what callers must send; the drifting UI name is
        // only a label (docs/providers.md § Antigravity).
        { id: "gemini-3.6-flash-high", display_name: "Gemini 3.6 Flash" },
        { id: "gemini-pro-agent", display_name: null },
      ],
      error: null,
    })
  })

  it("returns an empty list with the error rather than inventing a catalog", async () => {
    stubFetch(() => new Response("nope", { status: 500 }))
    expect(await antigravityAdapter.listModels!(buildEnv(), account())).toEqual({
      models: [],
      error: "models fetch failed",
    })
  })

  it("treats a well-formed empty models map as a real answer, not a failure", async () => {
    const calls = stubFetch(() => new Response(JSON.stringify({ models: {} }), { status: 200 }))
    // An account with no currently available models must not probe the
    // fallback host or cache "models fetch failed" for an hour.
    expect(await antigravityAdapter.listModels!(buildEnv(), account())).toEqual({
      models: [],
      error: null,
    })
    expect(calls).toHaveLength(1)
  })

  it("retries the project bootstrap for an account that has none stored", async () => {
    const calls = stubFetch((call) => {
      if (call.url.includes("loadCodeAssist")) {
        return new Response(
          JSON.stringify({ cloudaicompanionProject: "proj-new", currentTier: { id: "free-tier" } }),
          { status: 200 },
        )
      }
      return new Response(
        JSON.stringify({ models: { "gemini-3-flash": {} } }),
        { status: 200 },
      )
    })
    const acc = account()
    delete acc.credential.extra
    const result = await antigravityAdapter.listModels!(buildEnv(), acc)
    // The catalog is how a user discovers a callable model, so a login-time
    // bootstrap failure must not leave it permanently on "models fetch failed".
    expect(calls[0]!.url).toContain("loadCodeAssist")
    const modelsCall = calls.find((c) => c.url.includes("fetchAvailableModels"))!
    expect(JSON.parse(modelsCall.init.body as string)).toEqual({ project: "proj-new" })
    expect(result).toEqual({ models: [{ id: "gemini-3-flash", display_name: null }], error: null })
  })
})

describe("antigravityAdapter.fetchUsage", () => {
  it("reports the tier and credit balance with no fabricated usage window", async () => {
    stubFetch(
      () =>
        new Response(
          JSON.stringify({
            cloudaicompanionProject: "proj-42",
            currentTier: { id: "standard-tier" },
            paidTier: {
              id: "ai-pro",
              availableCredits: [
                {
                  creditType: "GOOGLE_ONE_AI",
                  creditAmount: "1500",
                  minimumCreditAmountForUsage: "10",
                },
              ],
            },
          }),
          { status: 200 },
        ),
    )
    const usage = await antigravityAdapter.fetchUsage!(buildEnv(), account())
    // Antigravity publishes no percentage quota, so no window is invented.
    expect(usage.windows).toEqual([])
    expect(usage.account).toMatchObject({
      plan_type: "ai-pro",
      project_id: "proj-42",
      credits_remaining: 1500,
      credits_minimum: 10,
    })
    expect(usage.error).toBeUndefined()
  })

  it("marks the snapshot stale on failure rather than blanking it", async () => {
    stubFetch(() => new Response("boom", { status: 503 }))
    const usage = await antigravityAdapter.fetchUsage!(buildEnv(), account())
    expect(usage.stale).toBe(true)
    expect(usage.error).toMatch(/loadCodeAssist 503/)
  })
})
