/**
 * Dispatch-level token capture wiring, exercised through the real Hono
 * routes (apiKeyAuth, resolveModel, dispatch) — not the sniffer/normalizer
 * math itself (covered by usage_capture.test.ts) or the converter usage
 * enrichment (covered by openai_anthropic.test.ts / codex_openai.test.ts).
 * This file checks that dispatch.ts actually wires those pieces together:
 * one row per request, tokens attached when knowable, deferred correctly
 * for streams.
 */
import { afterEach, describe, expect, it } from "vitest"
import { app } from "../src/index"
import { hashApiKey } from "../src/crypto/keys"
import { encryptJson } from "../src/crypto/token_crypto"
import type { Env } from "../src/env"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

const TOKEN_KEY = "test-token-encryption-key-not-secret"
const API_KEY_PLAINTEXT = "sk-kano-proxy-test-client-key-0001"

/** apiKeyAuth calls c.executionCtx.waitUntil — Hono throws without one supplied. */
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

async function seedApiKey(db: FakeD1, userId: string): Promise<void> {
  db.seed("api_keys", [
    {
      id: "key_1",
      user_id: userId,
      name: "test key",
      key_prefix: API_KEY_PLAINTEXT.slice(0, 20),
      key_hash: await hashApiKey(API_KEY_PLAINTEXT),
      created_at: "2026-01-01T00:00:00.000Z",
      last_used_at: null,
    },
  ])
}

/** Builtin providers only need `access_token` — no refresh_token means refreshIfNeeded is a no-op. */
async function seedAccount(db: FakeD1, opts: { userId: string; provider: string }): Promise<void> {
  const encrypted = await encryptJson(TOKEN_KEY, { access_token: "upstream-test-token" })
  db.seed("upstream_accounts", [
    {
      id: `acc_${opts.provider}`,
      user_id: opts.userId,
      provider: opts.provider,
      external_account_id: null,
      label: opts.provider,
      priority: 1,
      encrypted_payload: encrypted,
      account_meta_json: null,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
}

function authHeaders(): Record<string, string> {
  return { authorization: `Bearer ${API_KEY_PLAINTEXT}`, "content-type": "application/json" }
}

/**
 * A mocked upstream `Response` whose body arrives over several real macrotask
 * turns (via `setTimeout`) instead of all at once. A plain `new Response(text)`
 * resolves its whole body within the same microtask chain `await app.request`
 * already spans, so "not written until the stream ends" is unobservable —
 * everything downstream sees the upstream as already-finished. This makes the
 * still-in-flight state actually observable for deferred-write assertions.
 */
function trickleResponse(text: string, contentType: string): Response {
  const bytes = new TextEncoder().encode(text)
  let stopped = false
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      let i = 0
      const pushNext = () => {
        if (stopped) return
        if (i >= bytes.length) {
          controller.close()
          return
        }
        const end = Math.min(i + 24, bytes.length)
        controller.enqueue(bytes.subarray(i, end))
        i = end
        setTimeout(pushNext, 0)
      }
      setTimeout(pushNext, 0)
    },
    cancel() {
      stopped = true
    },
  })
  return new Response(stream, { status: 200, headers: { "content-type": contentType } })
}

async function drain(body: ReadableStream<Uint8Array> | null): Promise<string> {
  if (!body) return ""
  const reader = body.getReader()
  let out = ""
  for (;;) {
    const { done, value } = await reader.read()
    if (done) break
    out += new TextDecoder().decode(value)
  }
  return out
}

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

const ANTHROPIC_SSE = [
  "event: message_start",
  'data: {"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":10,"cache_read_input_tokens":2,"cache_creation_input_tokens":3}}}',
  "",
  "event: content_block_start",
  'data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}',
  "",
  "event: content_block_delta",
  'data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}',
  "",
  "event: content_block_stop",
  'data: {"type":"content_block_stop","index":0}',
  "",
  "event: message_delta",
  'data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}',
  "",
  "event: message_stop",
  'data: {"type":"message_stop"}',
  "",
].join("\n")

const OPENAI_SSE_WITH_USAGE = [
  'data: {"choices":[{"index":0,"delta":{"role":"assistant","content":"hi"}}]}',
  "",
  'data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}',
  "",
  'data: {"choices":[],"usage":{"prompt_tokens":50,"completion_tokens":10}}',
  "",
  "data: [DONE]",
  "",
].join("\n")

const OPENAI_SSE_NO_USAGE = [
  'data: {"choices":[{"index":0,"delta":{"role":"assistant","content":"hi"}}]}',
  "",
  'data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}',
  "",
  "data: [DONE]",
  "",
].join("\n")

describe("/openai/v1/chat/completions — non-stream capture", () => {
  it("grok (pure passthrough): logs one row with the upstream usage, cache details included", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "grok" })
    globalThis.fetch = (async () =>
      new Response(
        JSON.stringify({
          id: "x",
          choices: [{ message: { role: "assistant", content: "hi" }, finish_reason: "stop" }],
          usage: {
            prompt_tokens: 100,
            completion_tokens: 40,
            prompt_tokens_details: { cached_tokens: 20 },
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      )) as typeof fetch

    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({ model: "grok/grok-4.5", messages: [{ role: "user", content: "hi" }] }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      provider: "grok",
      model: "grok/grok-4.5",
      prompt_tokens: 100,
      completion_tokens: 40,
      cache_read_input_tokens: 20,
      cache_creation_input_tokens: null,
    })
  })

  it("claude-code (converted from Anthropic): tokens come from the converter's enriched usage", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "claude-code" })
    globalThis.fetch = (async () =>
      new Response(
        JSON.stringify({
          id: "msg_1",
          content: [{ type: "text", text: "hi" }],
          stop_reason: "end_turn",
          usage: {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_input_tokens: 2,
            cache_creation_input_tokens: 3,
          },
        }),
        { status: 200 },
      )) as typeof fetch

    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "claude-code/claude-opus-5",
          messages: [{ role: "user", content: "hi" }],
        }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    // total = input(10) + cache_read(2) + cache_creation(3)
    expect(rows[0]).toMatchObject({
      provider: "claude-code",
      prompt_tokens: 15,
      completion_tokens: 5,
      cache_read_input_tokens: 2,
      cache_creation_input_tokens: 3,
    })
  })
})

describe("/openai/v1/chat/completions — streaming capture", () => {
  it("writes the row only once the stream ends (deferred), with the sniffed usage", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "grok" })
    globalThis.fetch = (async () =>
      trickleResponse(OPENAI_SSE_WITH_USAGE, "text/event-stream")) as typeof fetch

    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "grok/grok-4.5",
          stream: true,
          messages: [{ role: "user", content: "hi" }],
        }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)
    // Nothing written yet — the row is deferred to stream close.
    expect(db.rows("request_logs")).toHaveLength(0)

    const body = await drain(res.body)
    expect(body).toContain("hi")

    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      provider: "grok",
      prompt_tokens: 50,
      completion_tokens: 10,
      cache_read_input_tokens: null,
    })
  })

  it("grok best-effort: no usage chunk from upstream still writes exactly one row, with NULL tokens", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "grok" })
    globalThis.fetch = (async () =>
      new Response(OPENAI_SSE_NO_USAGE, {
        status: 200,
        headers: { "content-type": "text/event-stream" },
      })) as typeof fetch

    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "grok/grok-4.5",
          stream: true,
          messages: [{ role: "user", content: "hi" }],
        }),
      },
      buildEnv(db),
      execCtx,
    )
    await drain(res.body)
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      prompt_tokens: null,
      completion_tokens: null,
      cache_read_input_tokens: null,
      cache_creation_input_tokens: null,
    })
  })

  it("client cancel mid-stream still writes exactly one row (finishes with whatever was seen)", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "grok" })
    // The usage chunk rides near the end of the SSE body — a trickling
    // upstream body plus reading only the first piece guarantees the sniffer
    // genuinely never sees it before cancel fires.
    globalThis.fetch = (async () => trickleResponse(OPENAI_SSE_WITH_USAGE, "text/event-stream")) as typeof fetch

    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "grok/grok-4.5",
          stream: true,
          messages: [{ role: "user", content: "hi" }],
        }),
      },
      buildEnv(db),
      execCtx,
    )
    const reader = res.body!.getReader()
    await reader.read() // first piece only — well before the usage line arrives
    expect(db.rows("request_logs")).toHaveLength(0)

    await expect(reader.cancel()).resolves.toBeUndefined()

    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    // Cancelled before the usage-bearing line ever arrived: NULL, not a
    // failed request and not a fabricated count.
    expect(rows[0]).toMatchObject({ prompt_tokens: null, completion_tokens: null })
  })

  it("claude-code streaming via /openai/v1: usage rides the converter's final chunk", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "claude-code" })
    globalThis.fetch = (async () =>
      new Response(ANTHROPIC_SSE, {
        status: 200,
        headers: { "content-type": "text/event-stream" },
      })) as typeof fetch

    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "claude-code/claude-opus-5",
          stream: true,
          messages: [{ role: "user", content: "hi" }],
        }),
      },
      buildEnv(db),
      execCtx,
    )
    await drain(res.body)
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      prompt_tokens: 15,
      completion_tokens: 7,
      cache_read_input_tokens: 2,
      cache_creation_input_tokens: 3,
    })
  })
})

describe("/anthropic/v1/messages — native claude-code capture", () => {
  it("non-stream: fromAnthropicUsage sums input + cache into prompt_tokens", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "claude-code" })
    globalThis.fetch = (async () =>
      new Response(
        JSON.stringify({
          id: "msg_1",
          content: [{ type: "text", text: "hi" }],
          stop_reason: "end_turn",
          usage: {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_input_tokens: 2,
            cache_creation_input_tokens: 3,
          },
        }),
        { status: 200 },
      )) as typeof fetch

    const res = await app.request(
      "/anthropic/v1/messages",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "claude-code/claude-opus-5",
          max_tokens: 100,
          messages: [{ role: "user", content: "hi" }],
        }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      prompt_tokens: 15,
      completion_tokens: 5,
      cache_read_input_tokens: 2,
      cache_creation_input_tokens: 3,
    })
  })

  it("streaming: the Anthropic sniffer captures message_start + message_delta, deferred to stream close", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "claude-code" })
    globalThis.fetch = (async () => trickleResponse(ANTHROPIC_SSE, "text/event-stream")) as typeof fetch

    const res = await app.request(
      "/anthropic/v1/messages",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "claude-code/claude-opus-5",
          stream: true,
          max_tokens: 100,
          messages: [{ role: "user", content: "hi" }],
        }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(db.rows("request_logs")).toHaveLength(0)
    await drain(res.body)
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      prompt_tokens: 15,
      completion_tokens: 7,
      cache_read_input_tokens: 2,
      cache_creation_input_tokens: 3,
    })
  })

  it("count_tokens never carries tokens, even though the upstream response has a token count", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "claude-code" })
    globalThis.fetch = (async () =>
      new Response(JSON.stringify({ input_tokens: 42 }), { status: 200 })) as typeof fetch

    const res = await app.request(
      "/anthropic/v1/messages/count_tokens",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "claude-code/claude-opus-5",
          messages: [{ role: "user", content: "hi" }],
        }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      prompt_tokens: null,
      completion_tokens: null,
      cache_read_input_tokens: null,
      cache_creation_input_tokens: null,
    })
  })
})

/** Upstream Responses SSE for `/anthropic` → grok (cli-chat-proxy). */
const GROK_RESPONSES_SSE_WITH_USAGE = [
  'data: {"type":"response.output_text.delta","delta":"hi"}',
  "",
  'data: {"type":"response.completed","response":{"usage":{"input_tokens":50,"output_tokens":10}}}',
  "",
].join("\n")

describe("/anthropic/v1/messages — grok Responses / codex conversion: exactly one row", () => {
  it("grok non-stream: exactly one request_logs row from converted Anthropic usage", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "grok" })
    // grok Anthropic path always requests upstream stream:true and collects.
    globalThis.fetch = (async () =>
      new Response(GROK_RESPONSES_SSE_WITH_USAGE, {
        status: 200,
        headers: { "content-type": "text/event-stream" },
      })) as typeof fetch

    const res = await app.request(
      "/anthropic/v1/messages",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "grok/grok-4.5",
          max_tokens: 100,
          messages: [{ role: "user", content: "hi" }],
        }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)
    const json = (await res.json()) as Record<string, unknown>
    expect(json.type).toBe("message")
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ provider: "grok", prompt_tokens: 50, completion_tokens: 10 })
  })

  it("grok streaming: exactly one request_logs row after the client-facing Anthropic stream fully drains", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "grok" })
    globalThis.fetch = (async () =>
      trickleResponse(GROK_RESPONSES_SSE_WITH_USAGE, "text/event-stream")) as typeof fetch

    const res = await app.request(
      "/anthropic/v1/messages",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "grok/grok-4.5",
          stream: true,
          max_tokens: 100,
          messages: [{ role: "user", content: "hi" }],
        }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)
    // Deferred: nothing yet, same invariant as the direct /openai/v1 stream path.
    expect(db.rows("request_logs")).toHaveLength(0)

    const body = await drain(res.body)
    expect(body).toContain("event: message_start")
    expect(body).toContain("event: message_stop")

    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ provider: "grok", prompt_tokens: 50, completion_tokens: 10 })
  })
})

describe("invalid_model — pre-dispatch logging (authenticated only)", () => {
  it("openai surface: an unresolvable model logs one row with error_code invalid_model", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")

    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "not-a-real-provider/some-model",
          messages: [{ role: "user", content: "hi" }],
        }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(400)
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      provider: "not-a-real-provider",
      model: "not-a-real-provider/some-model",
      status_code: 400,
      error_code: "invalid_model",
    })
  })

  it("anthropic surface (/v1/messages): same logging", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")

    const res = await app.request(
      "/anthropic/v1/messages",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "bogus/model",
          max_tokens: 10,
          messages: [{ role: "user", content: "hi" }],
        }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(400)
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      provider: "bogus",
      model: "bogus/model",
      status_code: 400,
      error_code: "invalid_model",
    })
  })

  it("anthropic surface (/v1/messages/count_tokens): same logging", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")

    const res = await app.request(
      "/anthropic/v1/messages/count_tokens",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({ model: "bogus/model", messages: [{ role: "user", content: "hi" }] }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(400)
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ provider: "bogus", error_code: "invalid_model" })
  })

  it("a model with no '/' logs provider as unknown", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")

    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({ model: "", messages: [] }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(400)
    expect(db.rows("request_logs")[0]).toMatchObject({ provider: "unknown", model: "" })
  })

  it("truncates a long raw model string to 200 chars in the logged row", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    const longModel = "bogus-provider/" + "x".repeat(250)

    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({ model: longModel, messages: [] }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(400)
    const row = db.rows("request_logs")[0]!
    expect(row.provider).toBe("bogus-provider")
    expect((row.model as string).length).toBe(200)
    expect(row.model).toBe(longModel.slice(0, 200))
  })

  it("an unauthenticated request is never logged", async () => {
    const db = new FakeD1()
    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ model: "not-a-real-provider/some-model", messages: [] }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(401)
    expect(db.rows("request_logs")).toHaveLength(0)
  })
})

describe("loop guard — pre-dispatch (conversion path only, grok/codex/custom-openai)", () => {
  function anthropicUnit(id: string): unknown[] {
    return [
      { role: "assistant", content: [{ type: "tool_use", id, name: "Read", input: { file_path: "/a" } }] },
      { role: "user", content: [{ type: "tool_result", tool_use_id: id, content: "ok" }] },
    ]
  }
  function anthropicLoopMessages(n = 8): unknown[] {
    const out: unknown[] = []
    for (let i = 0; i < n; i++) out.push(...anthropicUnit(`toolu_${i}`))
    return out
  }
  function openaiUnit(id: string): unknown[] {
    return [
      {
        role: "assistant",
        content: null,
        tool_calls: [{ id, type: "function", function: { name: "Read", arguments: '{"file_path":"/a"}' } }],
      },
      { role: "tool", tool_call_id: id, content: "ok" },
    ]
  }
  function openaiLoopMessages(n = 8): unknown[] {
    const out: unknown[] = []
    for (let i = 0; i < n; i++) out.push(...openaiUnit(`call_${i}`))
    return out
  }

  it("anthropic surface (grok conversion path): trips, no upstream call, logs loop_detected", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    let fetchCalled = false
    globalThis.fetch = (async () => {
      fetchCalled = true
      return new Response("{}", { status: 200 })
    }) as typeof fetch

    const res = await app.request(
      "/anthropic/v1/messages",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "grok/grok-4.5",
          max_tokens: 10,
          messages: anthropicLoopMessages(),
        }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(400)
    const json = (await res.json()) as { error: { message: string } }
    expect(json.error.message).toContain("Read repeated 8 times")
    expect(fetchCalled).toBe(false)
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ provider: "grok", status_code: 400, error_code: "loop_detected" })
  })

  it("openai surface (grok conversion path): trips, no upstream call, logs loop_detected", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    let fetchCalled = false
    globalThis.fetch = (async () => {
      fetchCalled = true
      return new Response("{}", { status: 200 })
    }) as typeof fetch

    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({ model: "grok/grok-4.5", messages: openaiLoopMessages() }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(400)
    const json = (await res.json()) as { error: { code: string } }
    expect(json.error.code).toBe("loop_detected")
    expect(fetchCalled).toBe(false)
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ provider: "grok", status_code: 400, error_code: "loop_detected" })
  })

  it("claude-code (native passthrough) is exempt — identical history dispatches normally", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "claude-code" })
    globalThis.fetch = (async () =>
      new Response(
        JSON.stringify({
          id: "msg_1",
          content: [{ type: "text", text: "hi" }],
          stop_reason: "end_turn",
        }),
        { status: 200 },
      )) as typeof fetch

    const res = await app.request(
      "/anthropic/v1/messages",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "claude-code/claude-opus-5",
          max_tokens: 10,
          messages: anthropicLoopMessages(),
        }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]!.error_code).toBeNull()
  })

  it("7 identical rounds does not trip — dispatch proceeds normally", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "grok" })
    globalThis.fetch = (async () =>
      new Response(
        JSON.stringify({
          choices: [{ message: { role: "assistant", content: "hi" }, finish_reason: "stop" }],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      )) as typeof fetch

    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({ model: "grok/grok-4.5", messages: openaiLoopMessages(7) }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]!.error_code).toBeNull()
  })
})

describe("no_upstream_account — pre-dispatch logging parity (anthropic native passthrough)", () => {
  it("anthropic surface: no claude-code account at all logs error_code no_upstream_account", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    // No seedAccount call — the user has no claude-code account.

    const res = await app.request(
      "/anthropic/v1/messages",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "claude-code/claude-opus-5",
          max_tokens: 10,
          messages: [{ role: "user", content: "hi" }],
        }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(400)
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      provider: "claude-code",
      status_code: 400,
      error_code: "no_upstream_account",
    })
  })

  it("openai surface: pre-existing behavior stays intact (no regression)", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")

    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({ model: "grok/grok-4.5", messages: [{ role: "user", content: "hi" }] }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(400)
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ provider: "grok", status_code: 400, error_code: "no_upstream_account" })
  })
})
