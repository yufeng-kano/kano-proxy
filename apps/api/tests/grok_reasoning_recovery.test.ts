import { afterEach, describe, expect, it } from "vitest"
import { grokAdapter } from "../src/providers/grok"
import {
  affinityPresent,
  isGrokOpaqueDecodeFailure,
  stripPromptCacheKey,
  stripResponsesOpaqueState,
} from "../src/providers/grok_reasoning_recovery"
import type { Env } from "../src/env"
import type { AcquiredAccount } from "../src/pool/acquire"

describe("isGrokOpaqueDecodeFailure", () => {
  it("matches compaction and encrypted_content decode errors", () => {
    expect(
      isGrokOpaqueDecodeFailure(
        JSON.stringify({
          code: "invalid-argument",
          error:
            "Could not decode the compaction blob. Ensure it is unmodified from the compact response.",
        }),
      ),
    ).toBe(true)
    expect(
      isGrokOpaqueDecodeFailure(
        "Could not decrypt the provided encrypted_content",
      ),
    ).toBe(true)
    expect(isGrokOpaqueDecodeFailure('{"error":"model not found"}')).toBe(
      false,
    )
  })
})

describe("stripResponsesOpaqueState", () => {
  it("removes reasoning.encrypted_content and drops empty reasoning items", () => {
    const { body, changed } = stripResponsesOpaqueState({
      model: "grok-4.5",
      input: [
        {
          type: "reasoning",
          summary: [],
          content: null,
          encrypted_content: "abc",
        },
        {
          type: "message",
          role: "user",
          content: [{ type: "input_text", text: "hi" }],
        },
      ],
    })
    expect(changed).toBe(true)
    expect(body.input).toEqual([
      {
        type: "message",
        role: "user",
        content: [{ type: "input_text", text: "hi" }],
      },
    ])
  })

  it("keeps reasoning items that still have readable summary text", () => {
    const { body, changed } = stripResponsesOpaqueState({
      input: [
        {
          type: "reasoning",
          summary: [{ type: "summary_text", text: "plan" }],
          encrypted_content: "abc",
        },
      ],
    })
    expect(changed).toBe(true)
    expect(body.input).toEqual([
      {
        type: "reasoning",
        summary: [{ type: "summary_text", text: "plan" }],
      },
    ])
  })

  it("drops compaction items entirely", () => {
    const { body, changed } = stripResponsesOpaqueState({
      input: [
        { type: "compaction", encrypted_content: "opaque" },
        {
          type: "message",
          role: "user",
          content: [{ type: "input_text", text: "after" }],
        },
      ],
    })
    expect(changed).toBe(true)
    expect(body.input).toHaveLength(1)
    expect((body.input as Array<{ type: string }>)[0]!.type).toBe("message")
  })
})

describe("stripPromptCacheKey / affinityPresent", () => {
  it("removes prompt_cache_key when present", () => {
    expect(stripPromptCacheKey({ prompt_cache_key: "s", model: "m" })).toEqual(
      { model: "m" },
    )
    expect(stripPromptCacheKey({ model: "m" })).toEqual({ model: "m" })
  })

  it("detects any sticky affinity header", () => {
    expect(affinityPresent({ convId: "c" })).toBe(true)
    expect(affinityPresent({ sessionId: "s" })).toBe(true)
    expect(affinityPresent({})).toBe(false)
  })
})

function mockEnv(deleted: string[] = []): Env {
  return {
    CACHE: {
      async get() {
        return null
      },
      async put() {},
      async delete(key: string) {
        deleted.push(key)
      },
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
  credential: { access_token: "tok_test" },
}

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

function okSse(): Response {
  const sse =
    'data: {"type":"response.output_text.delta","delta":"recovered"}\n\n' +
    'data: {"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":1}}}\n\n'
  return new Response(sse, {
    status: 200,
    headers: { "content-type": "text/event-stream" },
  })
}

/** High-entropy unpadded base64 that passes the Grok transport check. */
function fakeGrokCipher(): string {
  const bytes = new Uint8Array(64)
  for (let i = 0; i < bytes.length; i++) bytes[i] = (i * 37 + 11) % 256
  let bin = ""
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]!)
  return btoa(bin).replace(/=+$/, "")
}

describe("grokAdapter.messages — opaque decode recovery", () => {
  it("strips encrypted_content and retries after compaction-blob 400", async () => {
    const bodies: Array<Record<string, unknown>> = []
    let calls = 0
    globalThis.fetch = (async (_input: RequestInfo | URL, init?: RequestInit) => {
      calls++
      const body = JSON.parse(String(init?.body)) as Record<string, unknown>
      bodies.push(body)
      if (calls === 1) {
        return new Response(
          JSON.stringify({
            code: "invalid-argument",
            error:
              "Could not decode the compaction blob. Ensure it is unmodified from the compact response.",
          }),
          { status: 400, headers: { "content-type": "application/json" } },
        )
      }
      return okSse()
    }) as typeof fetch

    const sig = fakeGrokCipher()
    const res = await grokAdapter.messages!(
      mockEnv(),
      account,
      {
        model: "grok-4.5",
        stream: false,
        thinking: { type: "adaptive" },
        messages: [
          { role: "user", content: "hi" },
          {
            role: "assistant",
            content: [
              { type: "thinking", thinking: "plan", signature: sig },
              { type: "text", text: "hello" },
            ],
          },
          { role: "user", content: "continue" },
        ],
      },
      new Headers({
        "x-kano-api-key-id": "key_1",
        "x-kano-raw-model": "grok/grok-4.5",
        "x-grok-conv-id": "conv_bad",
      }),
    )

    expect(res.ok).toBe(true)
    expect(calls).toBe(2)
    const firstInput = bodies[0]!.input as Array<Record<string, unknown>>
    expect(
      firstInput.some(
        (i) => i.type === "reasoning" && typeof i.encrypted_content === "string",
      ),
    ).toBe(true)
    const secondInput = bodies[1]!.input as Array<Record<string, unknown>>
    expect(
      secondInput.some(
        (i) => i.type === "reasoning" && typeof i.encrypted_content === "string",
      ),
    ).toBe(false)
    const json = (await res.json()) as { content: Array<{ type: string; text?: string }> }
    expect(json.content.some((b) => b.text === "recovered")).toBe(true)
  })

  it("returns the original 400 when recovery retries also fail", async () => {
    let calls = 0
    globalThis.fetch = (async () => {
      calls++
      return new Response(
        JSON.stringify({
          code: "invalid-argument",
          error:
            "Could not decode the compaction blob. Ensure it is unmodified from the compact response.",
        }),
        { status: 400, headers: { "content-type": "application/json" } },
      )
    }) as typeof fetch

    const sig = fakeGrokCipher()
    const res = await grokAdapter.messages!(
      mockEnv(),
      account,
      {
        model: "grok-4.5",
        stream: false,
        thinking: { type: "adaptive" },
        messages: [
          { role: "user", content: "hi" },
          {
            role: "assistant",
            content: [
              { type: "thinking", thinking: "", signature: sig },
              { type: "text", text: "hello" },
            ],
          },
          { role: "user", content: "continue" },
        ],
      },
      new Headers({
        "x-kano-api-key-id": "key_1",
        "x-kano-raw-model": "grok/grok-4.5",
        "x-grok-conv-id": "conv_bad",
      }),
    )

    expect(res.status).toBe(400)
    const text = await res.text()
    expect(text).toContain("Could not decode the compaction blob")
    // original + strip retry + affinity-reset retry
    expect(calls).toBe(3)
  })
})
