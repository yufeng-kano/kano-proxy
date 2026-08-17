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
  count_tokens_url: null,
  models_mode: "auto",
  manual_models_json: null,
  sort_order: 0,
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
    usage_snapshot_json: null,
    usage_fetched_at: null,
    usage_fetching_at: null,
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

  it("remaps a Tabby unsupported-effort 400 and retries once on the same account", async () => {
    const calls: Array<{ url: string; init?: RequestInit }> = []
    const retryResponse = new Response(JSON.stringify({ ok: true }), { status: 200 })
    globalThis.fetch = (async (url: string, init?: RequestInit) => {
      calls.push({ url, init })
      if (calls.length === 1) {
        return new Response(
          JSON.stringify({
            detail:
              "TemplateError: Unexpected reasoning effort high. Supported types are xhigh (default), medium, and low.",
          }),
          { status: 400, statusText: "Bad Request", headers: { "content-type": "application/json" } },
        )
      }
      return retryResponse
    }) as typeof fetch

    const adapter = createCustomOpenAIAdapter(row)
    const clientBody = {
      model: "my-endpoint/qwen",
      messages: [{ role: "user", content: "hi" }],
      temperature: 0.7,
      reasoning_effort: "high",
      response_format: { type: "json_object" },
    }
    const res = await adapter.chatCompletions({} as Env, account, {
      model: "my-endpoint/qwen",
      rawModel: "my-endpoint/qwen",
      upstreamModel: "qwen",
      messages: clientBody.messages,
      reasoning_effort: "high",
      rawBody: clientBody,
    })

    expect(calls).toHaveLength(2)
    expect(calls[0]!.url).toBe("https://upstream.example.com/v1/chat/completions")
    expect(calls[1]!.url).toBe(calls[0]!.url)
    const firstHeaders = calls[0]!.init?.headers as Record<string, string>
    const secondHeaders = calls[1]!.init?.headers as Record<string, string>
    expect(firstHeaders.authorization).toBe("Bearer sk-test-upstream-key")
    expect(secondHeaders.authorization).toBe(firstHeaders.authorization)
    const firstBody = JSON.parse(String(calls[0]!.init?.body))
    const secondBody = JSON.parse(String(calls[1]!.init?.body))
    expect(firstBody).toMatchObject({
      model: "qwen",
      messages: [{ role: "user", content: "hi" }],
      temperature: 0.7,
      reasoning_effort: "high",
      response_format: { type: "json_object" },
    })
    expect(secondBody).toEqual({ ...firstBody, reasoning_effort: "xhigh" })
    expect(res).toBe(retryResponse)
  })

  it("returns an unrecognized 400 unchanged after one fetch", async () => {
    const original = '{"error":"context length exceeded"}'
    globalThis.fetch = (async () =>
      new Response(original, {
        status: 400,
        statusText: "Bad Request",
        headers: { "content-type": "application/json", "x-upstream": "yes" },
      })) as typeof fetch

    const adapter = createCustomOpenAIAdapter(row)
    const res = await adapter.chatCompletions({} as Env, account, {
      model: "my-endpoint/gpt-4o",
      rawModel: "my-endpoint/gpt-4o",
      upstreamModel: "gpt-4o",
      messages: [],
      reasoning_effort: "high",
      rawBody: { model: "my-endpoint/gpt-4o", messages: [], reasoning_effort: "high" },
    })

    expect(res.status).toBe(400)
    expect(res.statusText).toBe("Bad Request")
    expect(res.headers.get("content-type")).toBe("application/json")
    expect(res.headers.get("x-upstream")).toBe("yes")
    expect(await res.text()).toBe(original)
  })

  it("returns a retry non-2xx instead of the original 400", async () => {
    let fetches = 0
    const retryBody = '{"error":"still rejected"}'
    globalThis.fetch = (async () => {
      fetches += 1
      if (fetches === 1) {
        return new Response(
          JSON.stringify({
            detail:
              "TemplateError: Unexpected reasoning effort high. Supported types are xhigh (default), medium, and low.",
          }),
          { status: 400 },
        )
      }
      return new Response(retryBody, { status: 422, statusText: "Unprocessable Entity" })
    }) as typeof fetch

    const adapter = createCustomOpenAIAdapter(row)
    const res = await adapter.chatCompletions({} as Env, account, {
      model: "my-endpoint/qwen",
      rawModel: "my-endpoint/qwen",
      upstreamModel: "qwen",
      messages: [],
      reasoning_effort: "high",
      rawBody: { model: "my-endpoint/qwen", messages: [], reasoning_effort: "high" },
    })

    expect(fetches).toBe(2)
    expect(res.status).toBe(422)
    expect(res.statusText).toBe("Unprocessable Entity")
    expect(await res.text()).toBe(retryBody)
  })

  it("does not retry when the parser does not recognize the 400", async () => {
    let fetches = 0
    globalThis.fetch = (async () => {
      fetches += 1
      return new Response('{"error":"nope"}', { status: 400 })
    }) as typeof fetch

    const adapter = createCustomOpenAIAdapter(row)
    const res = await adapter.chatCompletions({} as Env, account, {
      model: "my-endpoint/gpt-4o",
      rawModel: "my-endpoint/gpt-4o",
      upstreamModel: "gpt-4o",
      messages: [],
      reasoning_effort: "high",
      rawBody: { model: "my-endpoint/gpt-4o", messages: [], reasoning_effort: "high" },
    })

    expect(fetches).toBe(1)
    expect(res.status).toBe(400)
    expect(await res.text()).toBe('{"error":"nope"}')
  })

  it("forwards extras.signal on both POSTs", async () => {
    const signal = new AbortController().signal
    const signals: Array<AbortSignal | null | undefined> = []
    globalThis.fetch = (async (_url: string, init?: RequestInit) => {
      signals.push(init?.signal)
      if (signals.length === 1) {
        return new Response(
          JSON.stringify({
            detail:
              "TemplateError: Unexpected reasoning effort high. Supported types are xhigh (default), medium, and low.",
          }),
          { status: 400 },
        )
      }
      return new Response("{}", { status: 200 })
    }) as typeof fetch

    const adapter = createCustomOpenAIAdapter(row)
    await adapter.chatCompletions(
      {} as Env,
      account,
      {
        model: "my-endpoint/qwen",
        rawModel: "my-endpoint/qwen",
        upstreamModel: "qwen",
        messages: [],
        reasoning_effort: "high",
        rawBody: { model: "my-endpoint/qwen", messages: [], reasoning_effort: "high" },
      },
      { signal },
    )

    expect(signals).toHaveLength(2)
    expect(signals[0]).toBe(signal)
    expect(signals[1]).toBe(signal)
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

describe("createCustomOpenAIAdapter — countTokens", () => {
  const rowWithCountTokens: CustomProviderRow = {
    ...row,
    count_tokens_url: "https://count.example.com/anthropic/count_tokens",
  }

  it("has no countTokens() when count_tokens_url is unset", () => {
    const adapter = createCustomOpenAIAdapter(row)
    expect(adapter.countTokens).toBeUndefined()
  })

  it("has countTokens() when count_tokens_url is set", () => {
    const adapter = createCustomOpenAIAdapter(rowWithCountTokens)
    expect(adapter.countTokens).toBeInstanceOf(Function)
  })

  it("posts to the exact stored URL, verbatim, with both auth headers and the default anthropic-version", async () => {
    let capturedUrl: string | undefined
    let capturedInit: RequestInit | undefined
    globalThis.fetch = (async (url: string, init?: RequestInit) => {
      capturedUrl = url
      capturedInit = init
      return new Response(JSON.stringify({ input_tokens: 12 }), { status: 200 })
    }) as typeof fetch

    const adapter = createCustomOpenAIAdapter(rowWithCountTokens)
    const body = { model: "gpt-4o", messages: [{ role: "user", content: "hi" }] }
    const res = await adapter.countTokens!({} as Env, account, body, new Headers())

    expect(capturedUrl).toBe("https://count.example.com/anthropic/count_tokens")
    expect(capturedInit?.method).toBe("POST")
    const headers = capturedInit?.headers as Record<string, string>
    expect(headers.authorization).toBe("Bearer sk-test-upstream-key")
    expect(headers["x-api-key"]).toBe("sk-test-upstream-key")
    expect(headers["content-type"]).toBe("application/json")
    expect(headers["anthropic-version"]).toBe("2023-06-01")
    expect(headers["anthropic-beta"]).toBeUndefined()
    expect(JSON.parse(String(capturedInit?.body))).toEqual(body)
    expect(res.status).toBe(200)
  })

  it("forwards a client anthropic-version instead of the default", async () => {
    let capturedInit: RequestInit | undefined
    globalThis.fetch = (async (_url: string, init?: RequestInit) => {
      capturedInit = init
      return new Response("{}", { status: 200 })
    }) as typeof fetch

    const adapter = createCustomOpenAIAdapter(rowWithCountTokens)
    const headers = new Headers({ "anthropic-version": "2024-10-01" })
    await adapter.countTokens!({} as Env, account, {}, headers)

    expect((capturedInit?.headers as Record<string, string>)["anthropic-version"]).toBe("2024-10-01")
  })

  it("forwards anthropic-beta verbatim when the client sent one", async () => {
    let capturedInit: RequestInit | undefined
    globalThis.fetch = (async (_url: string, init?: RequestInit) => {
      capturedInit = init
      return new Response("{}", { status: 200 })
    }) as typeof fetch

    const adapter = createCustomOpenAIAdapter(rowWithCountTokens)
    const headers = new Headers({ "anthropic-beta": "some-beta-2025-01-01" })
    await adapter.countTokens!({} as Env, account, {}, headers)

    expect((capturedInit?.headers as Record<string, string>)["anthropic-beta"]).toBe(
      "some-beta-2025-01-01",
    )
  })

  it("returns the upstream response untouched", async () => {
    const upstream = new Response(JSON.stringify({ input_tokens: 42 }), { status: 200 })
    globalThis.fetch = (async () => upstream) as typeof fetch

    const adapter = createCustomOpenAIAdapter(rowWithCountTokens)
    const res = await adapter.countTokens!({} as Env, account, {}, new Headers())
    expect(res).toBe(upstream)
  })

  it("forwards extras.signal", async () => {
    const signal = new AbortController().signal
    let capturedInit: RequestInit | undefined
    globalThis.fetch = (async (_url: string, init?: RequestInit) => {
      capturedInit = init
      return new Response("{}", { status: 200 })
    }) as typeof fetch

    const adapter = createCustomOpenAIAdapter(rowWithCountTokens)
    await adapter.countTokens!({} as Env, account, {}, new Headers(), { signal })
    expect(capturedInit?.signal).toBe(signal)
  })
})
