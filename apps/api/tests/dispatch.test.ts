/**
 * Direct unit-level tests for proxy/dispatch.ts functions that need a short,
 * injectable idle timeout (real HTTP-route tests can't practically wait
 * 120s) or a hand-built adapter to observe exactly what reaches it. Dispatch
 * wiring reachable through the real Hono routes (invalid_model,
 * loop_detected, no_upstream_account, token capture) is covered in
 * request_logging.test.ts instead.
 */
import { afterEach, describe, expect, it, vi } from "vitest"
import { encryptJson } from "../src/crypto/token_crypto"
import type { Env } from "../src/env"
import {
  dispatchAnthropicMessages,
  dispatchAnthropicViaOpenAI,
  dispatchChatCompletions,
} from "../src/proxy/dispatch"
import { benchKey, isBenched } from "../src/pool/bench"
import { getAdapter } from "../src/providers"
import type { ProviderAdapter } from "../src/providers/types"
import type { RoutingCandidate } from "../src/routing/types"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

afterEach(() => {
  vi.useRealTimers()
})

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
      usage_snapshot_json: null,
      usage_fetched_at: null,
      usage_fetching_at: null,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
}

/**
 * Several accounts for the same provider, highest priority first (matches
 * `listAccounts`' `ORDER BY priority DESC, created_at DESC` — all rows here
 * share `created_at`, so priority alone decides acquire order).
 */
async function seedAccounts(
  db: FakeD1,
  opts: { userId: string; provider: string; ids: string[] },
): Promise<void> {
  const rows = await Promise.all(
    opts.ids.map(async (id, i) => ({
      id,
      user_id: opts.userId,
      provider: opts.provider,
      external_account_id: null,
      label: opts.provider,
      priority: opts.ids.length - i,
      encrypted_payload: await encryptJson(TOKEN_KEY, { access_token: `upstream-test-token-${id}` }),
      account_meta_json: null,
      usage_snapshot_json: null,
      usage_fetched_at: null,
      usage_fetching_at: null,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    })),
  )
  db.seed("upstream_accounts", rows)
}

/** A bound account whose payload is not valid encrypted JSON — decryptJson always throws for it. */
function seedUndecryptableAccount(db: FakeD1, opts: { userId: string; provider: string }): void {
  db.seed("upstream_accounts", [
    {
      id: "acc_bad",
      user_id: opts.userId,
      provider: opts.provider,
      external_account_id: null,
      label: null,
      priority: 1,
      encrypted_payload: "!!!not-valid-base64!!!",
      account_meta_json: null,
      usage_snapshot_json: null,
      usage_fetched_at: null,
      usage_fetching_at: null,
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
      body: { model: "claude-opus-5", max_tokens: 10, messages: [], stream: true },
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

describe("dispatchAnthropicViaOpenAI — metadata.user_id → prompt_cache_key", () => {
  async function dispatchWith(body: Record<string, unknown>) {
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
      body,
      waitUntil: () => {},
    })
    return captured!
  }

  const sessionId =
    "user_ab12_account_11111111-2222-3333-4444-555555555555_session_0e35a1af-fe45-49c8-b0cc-fb1c58b1b06e"

  it("lands in the named field but never in the converted rawBody", async () => {
    const captured = await dispatchWith({
      model: "some-model",
      max_tokens: 10,
      metadata: { user_id: sessionId },
      messages: [{ role: "user", content: "hi" }],
    })
    expect(captured.prompt_cache_key).toBe(sessionId)
    const rawBody = captured.rawBody as Record<string, unknown>
    expect("prompt_cache_key" in rawBody).toBe(false)
  })

  it("stays unset without metadata", async () => {
    const captured = await dispatchWith({
      model: "some-model",
      max_tokens: 10,
      messages: [{ role: "user", content: "hi" }],
    })
    expect(captured.prompt_cache_key).toBeUndefined()
  })
})

/**
 * Regression for the `400 Invalid 'prompt_cache_key': string too long` that
 * v2.7.2 only half-fixed. Asserting on `buildCodexRequestBody`'s return value
 * missed it, because the over-long value went out on the `session_id` header
 * — which the Responses backend validates under the `prompt_cache_key` name
 * too. These drive the real /anthropic → codex path down to the fetch
 * boundary and check everything a length limit can apply to.
 */
describe("dispatchAnthropicViaOpenAI → codex — nothing over 64 chars reaches the wire", () => {
  const originalFetch = globalThis.fetch
  afterEach(() => {
    globalThis.fetch = originalFetch
  })

  /**
   * A 150-char `metadata.user_id` with no `_session_<uuid>` tail — the two
   * properties production proved the real Claude Code id has: the upstream
   * 400 said `got 150`, and every branch of the fit rule returns ≤ 64, so the
   * value reaching the wire was unfitted; a `_session_<uuid>` tail would have
   * collapsed it to a 36-char uuid on both the header and the body. The exact
   * shape below is illustrative — the fit is deliberately shape-agnostic.
   */
  const claudeCodeUserId = JSON.stringify({
    device_id: "a".repeat(64),
    account_uuid: "",
    session_id: "0e35a1af-fe45-49c8-b0cc-fb1c58b1b06e",
  })

  async function captureUpstream(userId: string, stream: boolean) {
    const db = new FakeD1()
    await seedAccount(db, { userId: "user_1", provider: "codex" })
    let captured: { headers: Headers; body: Record<string, unknown> } | undefined
    globalThis.fetch = (async (_url: string, init?: RequestInit) => {
      captured = { headers: new Headers(init?.headers), body: JSON.parse(String(init?.body)) }
      return new Response("", { status: 200 })
    }) as typeof fetch

    const res = await dispatchAnthropicViaOpenAI(buildEnv(db), {
      userId: "user_1",
      apiKeyId: "key_1",
      provider: "codex",
      adapter: getAdapter("codex"),
      rawModel: "codex/gpt-5.6-terra",
      upstreamModel: "gpt-5.6-terra",
      body: {
        model: "gpt-5.6-terra",
        max_tokens: 10,
        stream,
        metadata: { user_id: userId },
        messages: [{ role: "user", content: "hi" }],
      },
      waitUntil: () => {},
    })
    // Eager streaming commit returns headers before the upstream call runs, so
    // the stubbed fetch is only reached once the body has been drained.
    await drainBody(res.body)
    return captured!
  }

  it("fits both the session_id header and the body field on a streaming turn", async () => {
    expect(claudeCodeUserId.length).toBe(150)
    const captured = await captureUpstream(claudeCodeUserId, true)
    expect(captured.headers.get("session_id")!.length).toBeLessThanOrEqual(64)
    expect(String(captured.body.prompt_cache_key).length).toBeLessThanOrEqual(64)
  })

  it("fits them on a non-streaming turn too", async () => {
    const captured = await captureUpstream(claudeCodeUserId, false)
    expect(captured.headers.get("session_id")!.length).toBeLessThanOrEqual(64)
    expect(String(captured.body.prompt_cache_key).length).toBeLessThanOrEqual(64)
  })

  it("still sends the bare uuid verbatim for a `_session_<uuid>` id", async () => {
    const uuid = "0e35a1af-fe45-49c8-b0cc-fb1c58b1b06e"
    const captured = await captureUpstream(
      `user_ab12_account_11111111-2222-3333-4444-555555555555_session_${uuid}`,
      true,
    )
    expect(captured.headers.get("session_id")).toBe(uuid)
    expect(captured.body.prompt_cache_key).toBe(uuid)
  })
})

describe("dispatchChatCompletions — 402 benches the account and fails over (Item 1)", () => {
  it("an upstream 402 (e.g. OpenRouter 'Insufficient credits') benches that account; the loop retries the next one and succeeds", async () => {
    const db = new FakeD1()
    const provider = "openrouter"
    await seedAccounts(db, { userId: "user_1", provider, ids: ["acc_1", "acc_2"] })
    const env = buildEnv(db)

    const calls: string[] = []
    const adapter: ProviderAdapter = {
      id: provider,
      async chatCompletions(_env, account) {
        calls.push(account.row.id)
        if (calls.length === 1) {
          return new Response(
            JSON.stringify({
              error: "insufficient_credits",
              message: "Insufficient credits to complete this request",
            }),
            { status: 402, headers: { "content-type": "application/json" } },
          )
        }
        return new Response(
          JSON.stringify({
            choices: [{ message: { role: "assistant", content: "hi" }, finish_reason: "stop" }],
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        )
      },
    }

    const res = await dispatchChatCompletions(env, {
      userId: "user_1",
      apiKeyId: "key_1",
      provider,
      adapter,
      waitUntil: () => {},
      req: {
        model: `${provider}/some-model`,
        rawModel: `${provider}/some-model`,
        upstreamModel: "some-model",
        messages: [{ role: "user", content: "hi" }],
        rawBody: {},
      },
    })

    expect(res.status).toBe(200)
    // acc_1 has the higher priority so it is tried first; the 402 makes the
    // loop fail over to acc_2 rather than retrying acc_1 in place.
    expect(calls).toEqual(["acc_1", "acc_2"])

    expect(await isBenched(env, "user_1", provider, "acc_1")).toBe(true)
    expect(await isBenched(env, "user_1", provider, "acc_2")).toBe(false)

    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ provider, status_code: 200, account_id: "acc_2" })
  })
})

describe("dispatchChatCompletions — all-benched pool → 503 + Retry-After, not a fatal 400 (Item 2)", () => {
  it("zero accounts bound: 400 no_upstream_account is unchanged, no Retry-After", async () => {
    const db = new FakeD1()
    const env = buildEnv(db)
    // No seedAccounts call — the user has no grok account at all.

    const res = await dispatchChatCompletions(env, {
      userId: "user_1",
      apiKeyId: "key_1",
      provider: "grok",
      waitUntil: () => {},
      req: {
        model: "grok/grok-4.5",
        rawModel: "grok/grok-4.5",
        upstreamModel: "grok-4.5",
        messages: [{ role: "user", content: "hi" }],
        rawBody: {},
      },
    })

    expect(res.status).toBe(400)
    expect(res.headers.get("retry-after")).toBeNull()
    expect(await res.json()).toEqual({
      error: {
        message: "No usable grok account for this user",
        type: "invalid_request_error",
        code: "no_upstream_account",
      },
    })
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ status_code: 400, error_code: "no_upstream_account" })
  })

  it("accounts bound but every one benched: 503 upstream_unavailable, Retry-After = seconds until the earliest bench expiry", async () => {
    const db = new FakeD1()
    const provider = "grok"
    await seedAccounts(db, { userId: "user_1", provider, ids: ["acc_1", "acc_2"] })
    const env = buildEnv(db)

    vi.useFakeTimers()
    vi.setSystemTime(new Date("2026-01-01T12:00:00.000Z"))
    const now = Date.now()
    // acc_2's cooldown (45s) is earlier than acc_1's (120s) — the header
    // must reflect the EARLIEST expiry across the pool, not the first row.
    await env.BENCH.put(benchKey("user_1", provider, "acc_1"), String(now + 120_000))
    await env.BENCH.put(benchKey("user_1", provider, "acc_2"), String(now + 45_000))

    const res = await dispatchChatCompletions(env, {
      userId: "user_1",
      apiKeyId: "key_1",
      provider,
      waitUntil: () => {},
      req: {
        model: "grok/grok-4.5",
        rawModel: "grok/grok-4.5",
        upstreamModel: "grok-4.5",
        messages: [{ role: "user", content: "hi" }],
        rawBody: {},
      },
    })

    expect(res.status).toBe(503)
    expect(res.headers.get("retry-after")).toBe("45")
    expect(await res.json()).toEqual({
      error: { message: "All upstream accounts unavailable", code: "upstream_unavailable" },
    })
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      provider,
      status_code: 503,
      error_code: "upstream_unavailable",
    })
  })

  it("accounts bound, none benched, but every credential fails to decrypt: 503 without Retry-After", async () => {
    const db = new FakeD1()
    const provider = "grok"
    seedUndecryptableAccount(db, { userId: "user_1", provider })
    const env = buildEnv(db)

    const res = await dispatchChatCompletions(env, {
      userId: "user_1",
      apiKeyId: "key_1",
      provider,
      waitUntil: () => {},
      req: {
        model: "grok/grok-4.5",
        rawModel: "grok/grok-4.5",
        upstreamModel: "grok-4.5",
        messages: [{ role: "user", content: "hi" }],
        rawBody: {},
      },
    })

    expect(res.status).toBe(503)
    expect(res.headers.get("retry-after")).toBeNull()
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ status_code: 503, error_code: "upstream_unavailable" })
  })
})

describe("dispatchAnthropicMessages — all-benched pool → 503 + Retry-After, not a fatal 400 (Item 2)", () => {
  it("zero accounts bound: 400 no_upstream_account is unchanged, no Retry-After", async () => {
    const db = new FakeD1()
    const env = buildEnv(db)

    const res = await dispatchAnthropicMessages(env, {
      userId: "user_1",
      apiKeyId: "key_1",
      body: { model: "claude-opus-5", max_tokens: 10, messages: [] },
      headers: new Headers(),
      model: "claude-code/claude-opus-5",
      provider: "claude-code",
      waitUntil: () => {},
    })

    expect(res.status).toBe(400)
    expect(res.headers.get("retry-after")).toBeNull()
    expect(await res.json()).toEqual({
      type: "error",
      error: { type: "invalid_request_error", message: "No usable claude-code account" },
    })
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ status_code: 400, error_code: "no_upstream_account" })
  })

  it("accounts bound but every one benched: 503 upstream_unavailable, Retry-After = seconds until the earliest bench expiry", async () => {
    const db = new FakeD1()
    const provider = "claude-code"
    await seedAccounts(db, { userId: "user_1", provider, ids: ["acc_1", "acc_2"] })
    const env = buildEnv(db)

    vi.useFakeTimers()
    vi.setSystemTime(new Date("2026-01-01T12:00:00.000Z"))
    const now = Date.now()
    await env.BENCH.put(benchKey("user_1", provider, "acc_1"), String(now + 300_000))
    await env.BENCH.put(benchKey("user_1", provider, "acc_2"), String(now + 10_000))

    const res = await dispatchAnthropicMessages(env, {
      userId: "user_1",
      apiKeyId: "key_1",
      body: { model: "claude-opus-5", max_tokens: 10, messages: [] },
      headers: new Headers(),
      model: "claude-code/claude-opus-5",
      provider,
      waitUntil: () => {},
    })

    expect(res.status).toBe(503)
    expect(res.headers.get("retry-after")).toBe("10")
    expect(await res.json()).toEqual({
      type: "error",
      error: { type: "api_error", message: "upstream_unavailable" },
    })
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      provider,
      status_code: 503,
      error_code: "upstream_unavailable",
    })
  })

  it("accounts bound, none benched, but every credential fails to decrypt: 503 without Retry-After", async () => {
    const db = new FakeD1()
    const provider = "claude-code"
    seedUndecryptableAccount(db, { userId: "user_1", provider })
    const env = buildEnv(db)

    const res = await dispatchAnthropicMessages(env, {
      userId: "user_1",
      apiKeyId: "key_1",
      body: { model: "claude-opus-5", max_tokens: 10, messages: [] },
      headers: new Headers(),
      model: "claude-code/claude-opus-5",
      provider,
      waitUntil: () => {},
    })

    expect(res.status).toBe(503)
    expect(res.headers.get("retry-after")).toBeNull()
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ status_code: 503, error_code: "upstream_unavailable" })
  })
})

describe("dispatch candidate-walk exhaustion", () => {
  it("turns two OpenAI 429s into the standard non-stream 503 and keeps the last candidate in the log", async () => {
    const db = new FakeD1()
    await seedAccounts(db, { userId: "user_1", provider: "grok", ids: ["acc_1", "acc_2"] })
    const env = buildEnv(db)
    const adapter: ProviderAdapter = {
      id: "grok",
      async chatCompletions() {
        return new Response('{"error":{"message":"limited"}}', { status: 429 })
      },
    }

    const res = await dispatchChatCompletions(env, {
      userId: "user_1", apiKeyId: "key_1", provider: "grok", adapter, waitUntil: () => {},
      req: { model: "grok/grok-4.5", rawModel: "grok/grok-4.5", upstreamModel: "grok-4.5", messages: [], rawBody: {} },
    })

    expect(res.status).toBe(503)
    expect(res.headers.get("retry-after")).not.toBeNull()
    expect(await res.json()).toMatchObject({ error: { code: "upstream_unavailable" } })
    expect(db.rows("request_logs")[0]).toMatchObject({ account_id: "acc_2", status_code: 503, error_code: "upstream_unavailable" })
  })

  it("turns two OpenAI 429s into the standard eager-stream terminal error frame", async () => {
    const db = new FakeD1()
    await seedAccounts(db, { userId: "user_1", provider: "grok", ids: ["acc_1", "acc_2"] })
    const env = buildEnv(db)
    const { waitUntil, drain } = collectWaitUntil()
    const adapter: ProviderAdapter = {
      id: "grok",
      async chatCompletions() {
        return new Response('{"error":{"message":"limited"}}', { status: 429 })
      },
    }

    const res = await dispatchChatCompletions(env, {
      userId: "user_1", apiKeyId: "key_1", provider: "grok", adapter, waitUntil,
      req: { model: "grok/grok-4.5", rawModel: "grok/grok-4.5", upstreamModel: "grok-4.5", messages: [], stream: true, rawBody: {} },
    })

    expect(res.status).toBe(200)
    expect(await drainBody(res.body)).toContain('"code":"upstream_unavailable"')
    await drain()
    expect(db.rows("request_logs")[0]).toMatchObject({ account_id: "acc_2", error_code: "upstream_unavailable" })
  })

  it("passes a non-bench 400 through verbatim without trying a later candidate", async () => {
    const db = new FakeD1()
    await seedAccounts(db, { userId: "user_1", provider: "grok", ids: ["acc_1", "acc_2"] })
    let calls = 0
    const res = await dispatchChatCompletions(buildEnv(db), {
      userId: "user_1", apiKeyId: "key_1", provider: "grok", waitUntil: () => {},
      adapter: {
        id: "grok",
        async chatCompletions() {
          calls++
          return new Response("upstream says no", { status: 400, headers: { "content-type": "text/plain" } })
        },
      },
      req: { model: "grok/grok-4.5", rawModel: "grok/grok-4.5", upstreamModel: "grok-4.5", messages: [], rawBody: {} },
    })

    expect(res.status).toBe(400)
    expect(await res.text()).toBe("upstream says no")
    expect(calls).toBe(1)
  })
})

describe("dispatchChatCompletions — bottom-of-loop 503 also carries Retry-After (Item 2)", () => {
  it("8 consecutive 429s from a pool of 8 accounts exhaust the loop; the final 503 carries Retry-After from the just-benched accounts", async () => {
    const db = new FakeD1()
    const provider = "grok"
    const ids = Array.from({ length: 8 }, (_, i) => `acc_${i + 1}`)
    await seedAccounts(db, { userId: "user_1", provider, ids })
    const env = buildEnv(db)
    // Real time here (not fake timers): each iteration decrypts a real
    // AES-GCM payload via acquireAccount, and the default 300s cooldown
    // leaves a huge margin over any realistic in-memory test execution time
    // — ceil() still lands on exactly 300 as long as the whole loop runs in
    // well under a second, which 8 in-memory iterations comfortably do.

    const adapter: ProviderAdapter = {
      id: provider,
      async chatCompletions() {
        return new Response(JSON.stringify({ error: "rate_limited" }), {
          status: 429,
          headers: { "content-type": "application/json" },
        })
      },
    }

    const res = await dispatchChatCompletions(env, {
      userId: "user_1",
      apiKeyId: "key_1",
      provider,
      adapter,
      waitUntil: () => {},
      req: {
        model: `${provider}/some-model`,
        rawModel: `${provider}/some-model`,
        upstreamModel: "some-model",
        messages: [{ role: "user", content: "hi" }],
        rawBody: {},
      },
    })

    expect(res.status).toBe(503)
    expect(res.headers.get("retry-after")).toBe("300")
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ status_code: 503, error_code: "upstream_unavailable" })
    for (const id of ids) {
      expect(await isBenched(env, "user_1", provider, id)).toBe(true)
    }
  })
})


describe("dispatchChatCompletions — eager streaming commit", () => {
  it("stream:true returns 200 + event-stream BEFORE a slow upstream resolves", async () => {
    const db = new FakeD1()
    await seedAccount(db, { userId: "user_1", provider: "grok" })
    const { waitUntil, drain } = collectWaitUntil()

    let release!: (res: Response) => void
    const gate = new Promise<Response>((resolve) => {
      release = resolve
    })

    const adapter: ProviderAdapter = {
      id: "grok",
      async chatCompletions() {
        return gate
      },
    }

    const resPromise = dispatchChatCompletions(buildEnv(db), {
      userId: "user_1",
      apiKeyId: "key_1",
      provider: "grok",
      adapter,
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

    // Headers must commit without waiting for the hung adapter.
    const res = await resPromise
    expect(res.status).toBe(200)
    expect(res.headers.get("content-type") || "").toContain("text/event-stream")

    release(
      new Response('data: {"choices":[{"delta":{"content":"hi"}}]}\n\ndata: [DONE]\n\n', {
        status: 200,
        headers: { "content-type": "text/event-stream" },
      }),
    )
    const text = await drainBody(res.body)
    expect(text).toContain("hi")
    await drain()
  })

  it("stream:true + no account → 200 + in-stream no_upstream_account + log status 200", async () => {
    const db = new FakeD1()
    // No seedAccount — unbound pool.
    const { waitUntil, drain } = collectWaitUntil()

    const res = await dispatchChatCompletions(buildEnv(db), {
      userId: "user_1",
      apiKeyId: "key_1",
      provider: "grok",
      adapter: {
        id: "grok",
        async chatCompletions() {
          throw new Error("must not be called")
        },
      },
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
    expect(res.headers.get("content-type") || "").toContain("text/event-stream")
    const text = await drainBody(res.body)
    expect(text).toContain("no_upstream_account")
    expect(text).toContain("invalid_request_error")
    await drain()
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      provider: "grok",
      status_code: 200,
      error_code: "no_upstream_account",
    })
  })

  it("stream:true + client cancel while adapter hung → error_code client_abort", async () => {
    const db = new FakeD1()
    await seedAccount(db, { userId: "user_1", provider: "grok" })
    const { waitUntil, drain } = collectWaitUntil()

    // Never resolves — client cancels during TTFB wait.
    const adapter: ProviderAdapter = {
      id: "grok",
      async chatCompletions() {
        return new Promise(() => {})
      },
    }

    const res = await dispatchChatCompletions(buildEnv(db), {
      userId: "user_1",
      apiKeyId: "key_1",
      provider: "grok",
      adapter,
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
    const reader = res.body!.getReader()
    // Give the producer a tick to start the hung acquire/adapter path.
    await new Promise((r) => setTimeout(r, 10))
    await reader.cancel()
    await drain()
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      status_code: 200,
      error_code: "client_abort",
    })
  })
})

describe("dispatchAnthropicMessages — eager streaming commit", () => {
  it("stream:true + no account → 200 + event:error + log no_upstream_account status 200", async () => {
    const db = new FakeD1()
    const { waitUntil, drain } = collectWaitUntil()

    const res = await dispatchAnthropicMessages(buildEnv(db), {
      userId: "user_1",
      apiKeyId: "key_1",
      body: { model: "claude-opus-5", max_tokens: 10, messages: [], stream: true },
      headers: new Headers(),
      model: "claude-code/claude-opus-5",
      provider: "claude-code",
      adapter: {
        id: "claude-code",
        async chatCompletions() {
          throw new Error("not used")
        },
        async messages() {
          throw new Error("must not be called")
        },
      },
      waitUntil,
    })
    expect(res.status).toBe(200)
    expect(res.headers.get("content-type") || "").toContain("text/event-stream")
    const text = await drainBody(res.body)
    expect(text).toContain("event: error")
    expect(text).toContain("invalid_request_error")
    expect(text).toContain("No usable claude-code account")
    await drain()
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      provider: "claude-code",
      status_code: 200,
      error_code: "no_upstream_account",
    })
  })
})

describe("dispatchChatCompletions — 520/522/524 (upstream edge failure) benches and fails over", () => {
  it.each([520, 522, 524])(
    "a %d benches that account and the loop retries the next one — arrives as a response status before anything streamed, so in-request failover is safe",
    async (status) => {
      const db = new FakeD1()
      const provider = "grok"
      await seedAccounts(db, { userId: "user_1", provider, ids: ["acc_1", "acc_2"] })
      const env = buildEnv(db)

      const calls: string[] = []
      const adapter: ProviderAdapter = {
        id: provider,
        async chatCompletions(_env, account) {
          calls.push(account.row.id)
          if (calls.length === 1) {
            return new Response("upstream edge failure", { status })
          }
          return new Response(
            JSON.stringify({
              choices: [{ message: { role: "assistant", content: "hi" }, finish_reason: "stop" }],
            }),
            { status: 200, headers: { "content-type": "application/json" } },
          )
        },
      }

      const res = await dispatchChatCompletions(env, {
        userId: "user_1",
        apiKeyId: "key_1",
        provider,
        adapter,
        waitUntil: () => {},
        req: {
          model: `${provider}/some-model`,
          rawModel: `${provider}/some-model`,
          upstreamModel: "some-model",
          messages: [{ role: "user", content: "hi" }],
          rawBody: {},
        },
      })

      expect(res.status).toBe(200)
      expect(calls).toEqual(["acc_1", "acc_2"])
      expect(await isBenched(env, "user_1", provider, "acc_1")).toBe(true)
      expect(await isBenched(env, "user_1", provider, "acc_2")).toBe(false)
    },
  )
})

describe("routing module facts — limit-aware skip integration", () => {
  function seedAccountsWithSnapshots(
    db: FakeD1,
    opts: { userId: string; provider: string; accounts: Array<{ id: string; priority: number; usage_snapshot_json: string | null }> },
  ): void {
    db.seed(
      "upstream_accounts",
      opts.accounts.map((a) => ({
        id: a.id,
        user_id: opts.userId,
        provider: opts.provider,
        external_account_id: null,
        label: opts.provider,
        priority: a.priority,
        encrypted_payload: "will-be-set-below",
        account_meta_json: null,
        usage_snapshot_json: a.usage_snapshot_json,
        usage_fetched_at: null,
        usage_fetching_at: null,
        created_at: "2026-01-01T00:00:00.000Z",
        updated_at: "2026-01-01T00:00:00.000Z",
      })),
    )
  }

  async function encryptAllPayloads(db: FakeD1): Promise<void> {
    for (const row of db.rows("upstream_accounts")) {
      row.encrypted_payload = await encryptJson(TOKEN_KEY, { access_token: `tok-${row.id as string}` })
    }
  }

  function exhaustedSnapshot(resetsAt: string): string {
    return JSON.stringify({
      windows: [{ label: "5h", utilization: 100, resets_at: resetsAt }],
      error: null,
      stale: false,
      edgeBlocked: false,
    })
  }

  it("an account with a >=100% window and a future resets_at is skipped without a live call; a healthy sibling wins", async () => {
    const db = new FakeD1()
    const provider = "claude-code"
    const future = new Date(Date.now() + 3_600_000).toISOString()
    seedAccountsWithSnapshots(db, {
      userId: "user_1",
      provider,
      accounts: [
        { id: "acc_limited", priority: 10, usage_snapshot_json: exhaustedSnapshot(future) },
        { id: "acc_ok", priority: 1, usage_snapshot_json: null },
      ],
    })
    await encryptAllPayloads(db)
    const env = buildEnv(db)

    const calls: string[] = []
    const adapter: ProviderAdapter = {
      id: provider,
      async chatCompletions(_env, account) {
        calls.push(account.row.id)
        return new Response(
          JSON.stringify({ choices: [{ message: { role: "assistant", content: "hi" }, finish_reason: "stop" }] }),
          { status: 200, headers: { "content-type": "application/json" } },
        )
      },
    }

    const res = await dispatchChatCompletions(env, {
      userId: "user_1",
      apiKeyId: "key_1",
      provider,
      adapter,
      waitUntil: () => {},
      req: {
        model: `${provider}/some-model`,
        rawModel: `${provider}/some-model`,
        upstreamModel: "some-model",
        messages: [{ role: "user", content: "hi" }],
        rawBody: {},
      },
    })

    expect(res.status).toBe(200)
    // The limited account (higher priority) is never called at all.
    expect(calls).toEqual(["acc_ok"])
  })

  it("an account whose window's resets_at has already passed is NOT skipped, even off a stale snapshot", async () => {
    const db = new FakeD1()
    const provider = "claude-code"
    const past = new Date(Date.now() - 3_600_000).toISOString()
    seedAccountsWithSnapshots(db, {
      userId: "user_1",
      provider,
      accounts: [{ id: "acc_1", priority: 1, usage_snapshot_json: exhaustedSnapshot(past) }],
    })
    await encryptAllPayloads(db)
    const env = buildEnv(db)

    const calls: string[] = []
    const adapter: ProviderAdapter = {
      id: provider,
      async chatCompletions(_env, account) {
        calls.push(account.row.id)
        return new Response(
          JSON.stringify({ choices: [{ message: { role: "assistant", content: "hi" }, finish_reason: "stop" }] }),
          { status: 200, headers: { "content-type": "application/json" } },
        )
      },
    }

    const res = await dispatchChatCompletions(env, {
      userId: "user_1",
      apiKeyId: "key_1",
      provider,
      adapter,
      waitUntil: () => {},
      req: {
        model: `${provider}/some-model`,
        rawModel: `${provider}/some-model`,
        upstreamModel: "some-model",
        messages: [{ role: "user", content: "hi" }],
        rawBody: {},
      },
    })

    expect(res.status).toBe(200)
    expect(calls).toEqual(["acc_1"])
  })

  it("a malformed snapshot is ignored (fail open) — the account is tried normally", async () => {
    const db = new FakeD1()
    const provider = "claude-code"
    seedAccountsWithSnapshots(db, {
      userId: "user_1",
      provider,
      accounts: [{ id: "acc_1", priority: 1, usage_snapshot_json: "{not json" }],
    })
    await encryptAllPayloads(db)
    const env = buildEnv(db)

    const calls: string[] = []
    const adapter: ProviderAdapter = {
      id: provider,
      async chatCompletions(_env, account) {
        calls.push(account.row.id)
        return new Response(
          JSON.stringify({ choices: [{ message: { role: "assistant", content: "hi" }, finish_reason: "stop" }] }),
          { status: 200, headers: { "content-type": "application/json" } },
        )
      },
    }

    const res = await dispatchChatCompletions(env, {
      userId: "user_1",
      apiKeyId: "key_1",
      provider,
      adapter,
      waitUntil: () => {},
      req: {
        model: `${provider}/some-model`,
        rawModel: `${provider}/some-model`,
        upstreamModel: "some-model",
        messages: [{ role: "user", content: "hi" }],
        rawBody: {},
      },
    })

    expect(res.status).toBe(200)
    expect(calls).toEqual(["acc_1"])
  })

  it("every candidate limited: 503 upstream_unavailable with Retry-After from the window's resets_at, no live call", async () => {
    const db = new FakeD1()
    const provider = "claude-code"
    const future = new Date(Date.now() + 1_800_000).toISOString()
    seedAccountsWithSnapshots(db, {
      userId: "user_1",
      provider,
      accounts: [{ id: "acc_1", priority: 1, usage_snapshot_json: exhaustedSnapshot(future) }],
    })
    await encryptAllPayloads(db)
    const env = buildEnv(db)

    const adapter: ProviderAdapter = {
      id: provider,
      async chatCompletions() {
        throw new Error("must not be called — the candidate is already known-unusable")
      },
    }

    const res = await dispatchChatCompletions(env, {
      userId: "user_1",
      apiKeyId: "key_1",
      provider,
      adapter,
      waitUntil: () => {},
      req: {
        model: `${provider}/some-model`,
        rawModel: `${provider}/some-model`,
        upstreamModel: "some-model",
        messages: [{ role: "user", content: "hi" }],
        rawBody: {},
      },
    })

    expect(res.status).toBe(503)
    expect(res.headers.get("retry-after")).toBe(String(Math.ceil((Date.parse(future) - Date.now()) / 1000)))
  })
})

describe("cross-target failover — the flattened candidate list crosses provider/adapter boundaries within one request", () => {
  it("target 1's account benches on a 429; the walk continues into target 2's own (different-provider) candidate within the same request", async () => {
    const db = new FakeD1()
    await seedAccounts(db, { userId: "user_1", provider: "claude-code", ids: ["acc_cc"] })
    await seedAccounts(db, { userId: "user_1", provider: "grok", ids: ["acc_grok"] })
    const env = buildEnv(db)

    const ccRow = db.rows("upstream_accounts").find((r) => r.id === "acc_cc")!
    const grokRow = db.rows("upstream_accounts").find((r) => r.id === "acc_grok")!

    const calls: string[] = []
    const ccAdapter: ProviderAdapter = {
      id: "claude-code",
      async chatCompletions(_env, account) {
        calls.push(`claude-code:${account.row.id}`)
        return new Response(JSON.stringify({ error: "rate_limited" }), { status: 429 })
      },
    }
    const grokAdapter: ProviderAdapter = {
      id: "grok",
      async chatCompletions(_env, account) {
        calls.push(`grok:${account.row.id}`)
        return new Response(
          JSON.stringify({ choices: [{ message: { role: "assistant", content: "hi" }, finish_reason: "stop" }] }),
          { status: 200, headers: { "content-type": "application/json" } },
        )
      },
    }

    const candidates: RoutingCandidate[] = [
      {
        targetIndex: 0,
        pinned: false,
        provider: "claude-code",
        upstreamModel: "claude-opus-5",
        isBuiltin: true,
        adapter: ccAdapter,
        account: ccRow as any,
      },
      {
        targetIndex: 1,
        pinned: false,
        provider: "grok",
        upstreamModel: "grok-4.5",
        isBuiltin: true,
        adapter: grokAdapter,
        account: grokRow as any,
      },
    ]

    const res = await dispatchChatCompletions(env, {
      userId: "user_1",
      apiKeyId: "key_1",
      provider: "claude-code",
      candidates,
      groupName: "opus",
      waitUntil: () => {},
      req: {
        model: "opus",
        rawModel: "opus",
        upstreamModel: "claude-opus-5",
        messages: [{ role: "user", content: "hi" }],
        rawBody: {},
      },
    })

    expect(res.status).toBe(200)
    expect(calls).toEqual(["claude-code:acc_cc", "grok:acc_grok"])
    expect(await isBenched(env, "user_1", "claude-code", "acc_cc")).toBe(true)
  })
})

describe("dispatchChatCompletions — strategy option is accepted and defaults to ordered (no other value exists yet)", () => {
  it("omitting strategy behaves identically to passing 'ordered' explicitly", async () => {
    const db = new FakeD1()
    await seedAccounts(db, { userId: "user_1", provider: "grok", ids: ["acc_1", "acc_2"] })
    const env = buildEnv(db)
    const adapter: ProviderAdapter = {
      id: "grok",
      async chatCompletions(_env, account) {
        return new Response(
          JSON.stringify({
            choices: [{ message: { role: "assistant", content: account.row.id }, finish_reason: "stop" }],
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        )
      },
    }
    const reqOpts = {
      userId: "user_1",
      apiKeyId: "key_1",
      provider: "grok",
      adapter,
      waitUntil: () => {},
      req: {
        model: "grok/grok-4.5",
        rawModel: "grok/grok-4.5",
        upstreamModel: "grok-4.5",
        messages: [{ role: "user", content: "hi" }],
        rawBody: {},
      },
    }
    const withoutStrategy = await dispatchChatCompletions(env, reqOpts)
    const withStrategy = await dispatchChatCompletions(env, { ...reqOpts, strategy: "ordered" })
    expect(await withoutStrategy.text()).toBe(await withStrategy.text())
  })
})
