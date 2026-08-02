import { afterEach, describe, expect, it } from "vitest"
import { grokAdapter } from "../src/providers/grok"
import type { Env } from "../src/env"
import type { AcquiredAccount } from "../src/pool/acquire"

function mockEnv(): Env {
  return {
    CACHE: {
      async get() {
        return null
      },
      async put() {},
      async delete() {},
    },
  } as unknown as Env
}

const account: AcquiredAccount = {
  row: {
    id: "acc_1",
    user_id: "user_1",
    provider: "grok",
    external_account_id: null,
    label: null,
    priority: 1,
    encrypted_payload: "",
    account_meta_json: null,
    created_at: "2026-01-01T00:00:00.000Z",
    updated_at: "2026-01-01T00:00:00.000Z",
  },
  credential: { access_token: "tok_test" },
}

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

async function capturedBody(
  req: Parameters<typeof grokAdapter.chatCompletions>[2],
): Promise<Record<string, unknown>> {
  let body: Record<string, unknown> | undefined
  globalThis.fetch = (async (_url: string, init?: RequestInit) => {
    body = JSON.parse(String(init?.body))
    return new Response(JSON.stringify({ choices: [] }), {
      status: 200,
      headers: { "content-type": "application/json" },
    })
  }) as typeof fetch
  await grokAdapter.chatCompletions({} as Env, account, req)
  return body!
}

describe("grokAdapter.chatCompletions — reasoning + sampling", () => {
  it("always sends include_reasoning: true", async () => {
    const body = await capturedBody({
      model: "grok/grok-4.5",
      rawModel: "grok/grok-4.5",
      upstreamModel: "grok-4.5",
      messages: [{ role: "user", content: "hi" }],
      rawBody: {},
    })
    expect(body.include_reasoning).toBe(true)
  })

  it("defaults temperature to 1 when the client sent none", async () => {
    const body = await capturedBody({
      model: "grok/grok-4.5",
      rawModel: "grok/grok-4.5",
      upstreamModel: "grok-4.5",
      messages: [{ role: "user", content: "hi" }],
      rawBody: {},
    })
    expect(body.temperature).toBe(1)
  })

  it("forwards a client-supplied temperature instead of the default", async () => {
    const body = await capturedBody({
      model: "grok/grok-4.5",
      rawModel: "grok/grok-4.5",
      upstreamModel: "grok-4.5",
      messages: [{ role: "user", content: "hi" }],
      temperature: 0.2,
      rawBody: {},
    })
    expect(body.temperature).toBe(0.2)
  })

  it("forwards top_p only when the client sent it", async () => {
    const withTopP = await capturedBody({
      model: "grok/grok-4.5",
      rawModel: "grok/grok-4.5",
      upstreamModel: "grok-4.5",
      messages: [{ role: "user", content: "hi" }],
      top_p: 0.9,
      rawBody: {},
    })
    expect(withTopP.top_p).toBe(0.9)

    const withoutTopP = await capturedBody({
      model: "grok/grok-4.5",
      rawModel: "grok/grok-4.5",
      upstreamModel: "grok-4.5",
      messages: [{ role: "user", content: "hi" }],
      rawBody: {},
    })
    expect("top_p" in withoutTopP).toBe(false)
  })

  it("temperature 0 is forwarded as-is, not treated as unset", async () => {
    const body = await capturedBody({
      model: "grok/grok-4.5",
      rawModel: "grok/grok-4.5",
      upstreamModel: "grok-4.5",
      messages: [{ role: "user", content: "hi" }],
      temperature: 0,
      rawBody: {},
    })
    expect(body.temperature).toBe(0)
  })

  it("OpenAI surface still hits api.x.ai chat completions with grok-shell identity", async () => {
    let url = ""
    let headers: Headers | undefined
    globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      url = String(input)
      headers = new Headers(init?.headers)
      return new Response(JSON.stringify({ choices: [] }), {
        status: 200,
        headers: { "content-type": "application/json" },
      })
    }) as typeof fetch
    await grokAdapter.chatCompletions(mockEnv(), account, {
      model: "grok/grok-4.5",
      rawModel: "grok/grok-4.5",
      upstreamModel: "grok-4.5",
      messages: [{ role: "user", content: "hi" }],
      rawBody: {},
    })
    expect(url).toBe("https://api.x.ai/v1/chat/completions")
    expect(headers!.get("x-grok-client-identifier")).toBe("grok-shell")
    expect(headers!.get("user-agent")).toContain("grok-shell/")
  })
})

describe("grokAdapter.messages — Anthropic → Responses", () => {
  it("posts to cli-chat-proxy /responses with workspace client headers", async () => {
    let url = ""
    let headers: Headers | undefined
    let body: Record<string, unknown> | undefined
    globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      url = String(input)
      headers = new Headers(init?.headers)
      body = JSON.parse(String(init?.body))
      const sse =
        'data: {"type":"response.output_text.delta","delta":"ok"}\n\n' +
        'data: {"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":1}}}\n\n'
      return new Response(sse, {
        status: 200,
        headers: { "content-type": "text/event-stream" },
      })
    }) as typeof fetch

    const reqHeaders = new Headers({
      "x-kano-api-key-id": "key_1",
      "x-kano-raw-model": "grok/grok-4.5",
      "x-grok-conv-id": "conv_1",
    })
    const res = await grokAdapter.messages!(
      mockEnv(),
      account,
      {
        model: "grok-4.5",
        max_tokens: 64,
        stream: false,
        thinking: { type: "adaptive" },
        output_config: { effort: "high" },
        messages: [{ role: "user", content: "hi" }],
      },
      reqHeaders,
    )
    expect(res.ok).toBe(true)
    expect(url).toBe("https://cli-chat-proxy.grok.com/v1/responses")
    expect(headers!.get("user-agent")).toBe("xai-grok-workspace/0.2.93")
    expect(headers!.get("x-grok-client-version")).toBe("0.2.93")
    expect(headers!.get("X-XAI-Token-Auth")).toBe("xai-grok-cli")
    expect(headers!.get("x-grok-conv-id")).toBe("conv_1")
    expect(body!.include).toEqual(["reasoning.encrypted_content"])
    expect(body!.reasoning).toEqual({ effort: "high" })
    const json = (await res.json()) as { content: Array<{ type: string }> }
    expect(json.content.some((b) => b.type === "text")).toBe(true)
  })

  it("omits include when thinking is disabled", async () => {
    let body: Record<string, unknown> | undefined
    globalThis.fetch = (async (_input: RequestInfo | URL, init?: RequestInit) => {
      body = JSON.parse(String(init?.body))
      const sse =
        'data: {"type":"response.output_text.delta","delta":"ok"}\n\n' +
        'data: {"type":"response.completed","response":{}}\n\n'
      return new Response(sse, {
        status: 200,
        headers: { "content-type": "text/event-stream" },
      })
    }) as typeof fetch

    await grokAdapter.messages!(
      mockEnv(),
      account,
      {
        model: "grok-4.5",
        stream: false,
        thinking: { type: "disabled" },
        messages: [{ role: "user", content: "hi" }],
      },
      new Headers({ "x-kano-raw-model": "grok/grok-4.5" }),
    )
    expect(body!.include).toBeUndefined()
    expect(body!.reasoning).toBeUndefined()
  })
})
