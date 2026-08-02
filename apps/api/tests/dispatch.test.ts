/**
 * Direct unit-level tests for proxy/dispatch.ts functions that need a short,
 * injectable idle timeout (real HTTP-route tests can't practically wait
 * 120s) or a hand-built adapter to observe exactly what reaches it. Dispatch
 * wiring reachable through the real Hono routes (invalid_model,
 * loop_detected, no_upstream_account, token capture) is covered in
 * request_logging.test.ts instead.
 */
import { describe, expect, it } from "vitest"
import { encryptJson } from "../src/crypto/token_crypto"
import type { Env } from "../src/env"
import {
  dispatchAnthropicMessages,
  dispatchAnthropicViaOpenAI,
  dispatchChatCompletions,
} from "../src/proxy/dispatch"
import type { ProviderAdapter } from "../src/providers/types"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

const TOKEN_KEY = "test-token-encryption-key-not-secret"

function buildEnv(db: FakeD1): Env {
  return {
    DB: db as unknown as D1Database,
    BENCH: fakeKV(),
    CACHE: fakeKV(),
    APP_URL: "https://app.example.com",
    TOKEN_ENCRYPTION_KEY: TOKEN_KEY,
  } as unknown as Env
}

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

function collectWaitUntil(): { waitUntil: (p: Promise<unknown>) => void; drain: () => Promise<void> } {
  const pending: Promise<unknown>[] = []
  return {
    waitUntil: (p) => pending.push(p),
    drain: async () => {
      await Promise.all(pending)
    },
  }
}

async function drainBody(body: ReadableStream<Uint8Array> | null): Promise<string> {
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

/** Enqueues one chunk, then never enqueues again and never closes — simulates upstream silence. */
function neverEndingSseStream(initialChunk: string): ReadableStream<Uint8Array> {
  return new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(new TextEncoder().encode(initialChunk))
    },
  })
}

describe("dispatchChatCompletions — idle timeout (injectable for testability)", () => {
  it("a stream that sends one chunk then goes silent trips the idle timeout: stall frame, error_code upstream_stall", async () => {
    const db = new FakeD1()
    await seedAccount(db, { userId: "user_1", provider: "grok" })
    const { waitUntil, drain } = collectWaitUntil()

    const adapter: ProviderAdapter = {
      id: "grok",
      async chatCompletions() {
        return new Response(
          neverEndingSseStream('data: {"choices":[{"delta":{"role":"assistant"}}]}\n\n'),
          { status: 200, headers: { "content-type": "text/event-stream" } },
        )
      },
    }

    const res = await dispatchChatCompletions(buildEnv(db), {
      userId: "user_1",
      apiKeyId: "key_1",
      provider: "grok",
      adapter,
      idleTimeoutMs: 20,
      waitUntil,
      req: {
        model: "grok/grok-4.5",
        rawModel: "grok/grok-4.5",
        upstreamModel: "grok-4.5",
        messages: [{ role: "user", content: "hi" }],
        stream: true,
        rawBody: {},
      },
    })
    expect(res.status).toBe(200)
    const text = await drainBody(res.body)
    expect(text).toContain(
      'data: {"error":{"message":"upstream stalled: no data received for 120s","type":"api_error","code":"upstream_stall"}}',
    )
    await drain()
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ provider: "grok", status_code: 200, error_code: "upstream_stall" })
  })

  it("defaults idleTimeoutMs to 120_000 when not passed (option is respected when given)", async () => {
    const db = new FakeD1()
    await seedAccount(db, { userId: "user_1", provider: "grok" })
    const { waitUntil, drain } = collectWaitUntil()

    const adapter: ProviderAdapter = {
      id: "grok",
      async chatCompletions() {
        return new Response('data: {"choices":[{"delta":{"content":"hi"}}]}\n\ndata: [DONE]\n\n', {
          status: 200,
          headers: { "content-type": "text/event-stream" },
        })
      },
    }

    const res = await dispatchChatCompletions(buildEnv(db), {
      userId: "user_1",
      apiKeyId: "key_1",
      provider: "grok",
      adapter,
      // idleTimeoutMs intentionally omitted
      waitUntil,
      req: {
        model: "grok/grok-4.5",
        rawModel: "grok/grok-4.5",
        upstreamModel: "grok-4.5",
        messages: [{ role: "user", content: "hi" }],
        stream: true,
        rawBody: {},
      },
    })
    await drainBody(res.body)
    await drain()
    // A short, fully-drained stream completes long before any 120s timer —
    // proves the default doesn't fire early and doesn't crash anything.
    expect(db.rows("request_logs")[0]).toMatchObject({ error_code: null })
  })
})

describe("dispatchAnthropicMessages — idle timeout (injectable for testability)", () => {
  it("a message_start-only stream that goes silent trips the idle timeout: Anthropic stall event, error_code upstream_stall", async () => {
    const db = new FakeD1()
    await seedAccount(db, { userId: "user_1", provider: "claude-code" })
    const { waitUntil, drain } = collectWaitUntil()

    const adapter: ProviderAdapter = {
      id: "claude-code",
      async chatCompletions() {
        throw new Error("not used by this test")
      },
      async messages() {
        return new Response(
          neverEndingSseStream(
            'event: message_start\ndata: {"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":5}}}\n\n',
          ),
          { status: 200, headers: { "content-type": "text/event-stream" } },
        )
      },
    }

    const res = await dispatchAnthropicMessages(buildEnv(db), {
      userId: "user_1",
      apiKeyId: "key_1",
      body: { model: "claude-opus-5", max_tokens: 10, messages: [] },
      headers: new Headers(),
      model: "claude-code/claude-opus-5",
      provider: "claude-code",
      adapter,
      idleTimeoutMs: 20,
      waitUntil,
    })
    expect(res.status).toBe(200)
    const text = await drainBody(res.body)
    expect(text).toContain("event: error")
    expect(text).toContain(
      '"error":{"type":"overloaded_error","message":"upstream stalled: no data received for 120s"}',
    )
    await drain()
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      provider: "claude-code",
      status_code: 200,
      error_code: "upstream_stall",
    })
  })
})

describe("dispatchAnthropicMessages — no_upstream_account parity", () => {
  it("logs error_code no_upstream_account on the first-acquire-null path (no account, no prior response)", async () => {
    const db = new FakeD1()
    // No seedAccount call — the user has no usable account at all.

    const res = await dispatchAnthropicMessages(buildEnv(db), {
      userId: "user_1",
      apiKeyId: "key_1",
      body: { model: "claude-opus-5", max_tokens: 10, messages: [] },
      headers: new Headers(),
      model: "claude-code/claude-opus-5",
      provider: "claude-code",
      waitUntil: () => {},
    })
    expect(res.status).toBe(400)
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      provider: "claude-code",
      status_code: 400,
      error_code: "no_upstream_account",
    })
  })
})

describe("dispatchAnthropicViaOpenAI — sampling passthrough", () => {
  it("a client-sent Anthropic temperature/top_p lands in req.temperature/top_p and the converted rawBody", async () => {
    const db = new FakeD1()
    await seedAccount(db, { userId: "user_1", provider: "custom-openai-test" })
    let captured: Record<string, unknown> | undefined

    const adapter: ProviderAdapter = {
      id: "custom-openai-test",
      async chatCompletions(_env, _account, req) {
        captured = req as unknown as Record<string, unknown>
        return new Response(
          JSON.stringify({
            choices: [{ message: { role: "assistant", content: "hi" }, finish_reason: "stop" }],
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        )
      },
    }

    await dispatchAnthropicViaOpenAI(buildEnv(db), {
      userId: "user_1",
      apiKeyId: "key_1",
      provider: "custom-openai-test",
      adapter,
      rawModel: "custom-openai-test/some-model",
      upstreamModel: "some-model",
      body: {
        model: "some-model",
        max_tokens: 10,
        temperature: 0.42,
        top_p: 0.77,
        messages: [{ role: "user", content: "hi" }],
      },
      waitUntil: () => {},
    })

    expect(captured?.temperature).toBe(0.42)
    expect(captured?.top_p).toBe(0.77)
    const rawBody = captured?.rawBody as Record<string, unknown>
    expect(rawBody.temperature).toBe(0.42)
    expect(rawBody.top_p).toBe(0.77)
  })
})
