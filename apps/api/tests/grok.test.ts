import { afterEach, describe, expect, it } from "vitest"
import { grokAdapter } from "../src/providers/grok"
import type { Env } from "../src/env"
import type { AcquiredAccount } from "../src/pool/acquire"

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
})
