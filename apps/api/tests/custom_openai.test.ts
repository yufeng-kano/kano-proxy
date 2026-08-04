import { afterEach, describe, expect, it } from "vitest"
import { createCustomOpenAIAdapter } from "../src/providers/custom_openai"
import type { CustomProviderRow } from "../src/db/custom_providers"
import type { Env } from "../src/env"
import type { AcquiredAccount } from "../src/pool/acquire"

const row: CustomProviderRow = {
  id: "cprov_1",
  user_id: "user_1",
  slug: "my-endpoint",
  name: "My Endpoint",
  format: "openai",
  base_url: "https://upstream.example.com/v1",
  models_mode: "auto",
  manual_models_json: null,
  created_at: "2026-01-01T00:00:00.000Z",
  updated_at: "2026-01-01T00:00:00.000Z",
}

const account: AcquiredAccount = {
  row: {
    id: "acc_1",
    user_id: "user_1",
    provider: "my-endpoint",
    external_account_id: null,
    label: null,
    custom_label: null,
    priority: 1,
    encrypted_payload: "",
    account_meta_json: null,
    created_at: "2026-01-01T00:00:00.000Z",
    updated_at: "2026-01-01T00:00:00.000Z",
  },
  credential: { access_token: "sk-test-upstream-key" },
}

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

describe("createCustomOpenAIAdapter", () => {
  it("has no messages()/countTokens() — the /anthropic surface reaches it via conversion", () => {
    const adapter = createCustomOpenAIAdapter(row)
    expect(adapter.messages).toBeUndefined()
    expect(adapter.countTokens).toBeUndefined()
    expect(adapter.fetchUsage).toBeUndefined()
    expect(adapter.refreshIfNeeded).toBeUndefined()
  })

  it("posts to {base}/chat/completions with a Bearer header", async () => {
    let capturedUrl: string | undefined
    let capturedInit: RequestInit | undefined
    globalThis.fetch = (async (url: string, init?: RequestInit) => {
      capturedUrl = url
      capturedInit = init
      return new Response(JSON.stringify({ ok: true }), { status: 200 })
    }) as typeof fetch

    const adapter = createCustomOpenAIAdapter(row)
    await adapter.chatCompletions({} as Env, account, {
      model: "my-endpoint/gpt-4o",
      rawModel: "my-endpoint/gpt-4o",
      upstreamModel: "gpt-4o",
      messages: [{ role: "user", content: "hi" }],
      rawBody: { model: "my-endpoint/gpt-4o", messages: [{ role: "user", content: "hi" }] },
    })

    expect(capturedUrl).toBe("https://upstream.example.com/v1/chat/completions")
    expect(capturedInit?.method).toBe("POST")
    const headers = capturedInit?.headers as Record<string, string>
    expect(headers.authorization).toBe("Bearer sk-test-upstream-key")
    expect(headers["content-type"]).toBe("application/json")
  })

  it("rewrites model to the bare upstream id", async () => {
    let body: Record<string, unknown> | undefined
    globalThis.fetch = (async (_url: string, init?: RequestInit) => {
      body = JSON.parse(String(init?.body))
      return new Response("{}", { status: 200 })
    }) as typeof fetch

    const adapter = createCustomOpenAIAdapter(row)
    await adapter.chatCompletions({} as Env, account, {
      model: "my-endpoint/gpt-4o",
      rawModel: "my-endpoint/gpt-4o",
      upstreamModel: "gpt-4o",
      messages: [],
      rawBody: { model: "my-endpoint/gpt-4o", messages: [] },
    })

    expect(body?.model).toBe("gpt-4o")
  })

  it("forwards the client body verbatim, including temperature and reasoning_effort", async () => {
    let body: Record<string, unknown> | undefined
    globalThis.fetch = (async (_url: string, init?: RequestInit) => {
      body = JSON.parse(String(init?.body))
      return new Response("{}", { status: 200 })
    }) as typeof fetch

    const adapter = createCustomOpenAIAdapter(row)
    const clientBody = {
      model: "my-endpoint/gpt-4o",
      messages: [{ role: "user", content: "hi" }],
      temperature: 0.7,
      reasoning_effort: "high",
      response_format: { type: "json_object" },
      top_p: 0.9,
      seed: 42,
    }
    await adapter.chatCompletions({} as Env, account, {
      model: "my-endpoint/gpt-4o",
      rawModel: "my-endpoint/gpt-4o",
      upstreamModel: "gpt-4o",
      messages: clientBody.messages,
      reasoning_effort: "high",
      rawBody: clientBody,
    })

    expect(body).toMatchObject({
      messages: [{ role: "user", content: "hi" }],
      temperature: 0.7,
      reasoning_effort: "high",
      response_format: { type: "json_object" },
      top_p: 0.9,
      seed: 42,
      model: "gpt-4o",
    })
  })

  it("pipes an SSE response through untouched (no buffering)", async () => {
    const upstreamStream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode('data: {"delta":"hi"}\n\n'))
        controller.close()
      },
    })
    const upstreamResponse = new Response(upstreamStream, {
      status: 200,
      headers: { "content-type": "text/event-stream" },
    })
    globalThis.fetch = (async () => upstreamResponse) as typeof fetch

    const adapter = createCustomOpenAIAdapter(row)
    const res = await adapter.chatCompletions({} as Env, account, {
      model: "my-endpoint/gpt-4o",
      rawModel: "my-endpoint/gpt-4o",
      upstreamModel: "gpt-4o",
      messages: [],
      stream: true,
      rawBody: { model: "my-endpoint/gpt-4o", messages: [], stream: true },
    })

    // The adapter must return the exact same Response/stream — no re-encoding.
    expect(res).toBe(upstreamResponse)
    expect(res.body).toBe(upstreamStream)
    const text = await res.text()
    expect(text).toContain('"delta":"hi"')
  })

  it("listModels GETs {base}/models with a Bearer header", async () => {
    let capturedUrl: string | undefined
    let capturedInit: RequestInit | undefined
    globalThis.fetch = (async (url: string, init?: RequestInit) => {
      capturedUrl = url
      capturedInit = init
      return new Response(JSON.stringify({ data: [{ id: "gpt-4o" }, { id: "gpt-4o-mini" }] }), {
        status: 200,
      })
    }) as typeof fetch

    const adapter = createCustomOpenAIAdapter(row)
    const result = await adapter.listModels!({} as Env, account)

    expect(capturedUrl).toBe("https://upstream.example.com/v1/models")
    const headers = capturedInit?.headers as Record<string, string>
    expect(headers.authorization).toBe("Bearer sk-test-upstream-key")
    expect(result.models).toEqual([
      { id: "gpt-4o", display_name: null },
      { id: "gpt-4o-mini", display_name: null },
    ])
    expect(result.error).toBeNull()
  })

  it("listModels reports an error string on non-2xx instead of throwing", async () => {
    globalThis.fetch = (async () => new Response("nope", { status: 401 })) as typeof fetch
    const adapter = createCustomOpenAIAdapter(row)
    const result = await adapter.listModels!({} as Env, account)
    expect(result.models).toEqual([])
    expect(result.error).toBe("models 401")
  })

  it("listModels catches a network failure instead of throwing", async () => {
    globalThis.fetch = (async () => {
      throw new Error("network down")
    }) as typeof fetch
    const adapter = createCustomOpenAIAdapter(row)
    const result = await adapter.listModels!({} as Env, account)
    expect(result.models).toEqual([])
    expect(result.error).toBe("network down")
  })
})
