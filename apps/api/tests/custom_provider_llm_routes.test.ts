/**
 * End-to-end routing for custom providers on the LLM surfaces
 * (`/openai/v1`, `/anthropic`) — exercises the real Hono route handlers,
 * apiKeyAuth, resolveModel, and dispatch, not just the adapters in isolation.
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

async function seedCustomProvider(
  db: FakeD1,
  opts: { slug: string; format: "openai" | "anthropic"; userId: string; countTokensUrl?: string | null },
): Promise<void> {
  db.seed("custom_providers", [
    {
      id: `cprov_${opts.slug}`,
      user_id: opts.userId,
      slug: opts.slug,
      name: opts.slug,
      format: opts.format,
      base_url: opts.format === "openai" ? "https://upstream.example.com/v1" : "https://upstream.example.com",
      count_tokens_url: opts.countTokensUrl ?? null,
      models_mode: "auto",
      manual_models_json: null,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
  const encrypted = await encryptJson(TOKEN_KEY, { access_token: "sk-upstream-test-key" })
  db.seed("upstream_accounts", [
    {
      id: `acc_${opts.slug}`,
      user_id: opts.userId,
      provider: opts.slug,
      external_account_id: null,
      label: opts.slug,
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

function authHeaders(): Record<string, string> {
  return { authorization: `Bearer ${API_KEY_PLAINTEXT}`, "content-type": "application/json" }
}

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

describe("custom provider routing — /openai/v1/chat/completions", () => {
  it("routes a custom openai-format slug to its base URL with Bearer auth", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedCustomProvider(db, { slug: "my-oa", format: "openai", userId: "user_1" })
    let capturedUrl: string | undefined
    let capturedAuth: string | undefined
    globalThis.fetch = (async (url: string, init?: RequestInit) => {
      capturedUrl = url
      capturedAuth = (init?.headers as Record<string, string>).authorization
      return new Response(
        JSON.stringify({ id: "x", choices: [{ message: { role: "assistant", content: "hi" } }] }),
        { status: 200, headers: { "content-type": "application/json" } },
      )
    }) as typeof fetch

    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({ model: "my-oa/gpt-4o", messages: [{ role: "user", content: "hi" }] }),
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(200)
    expect(capturedUrl).toBe("https://upstream.example.com/v1/chat/completions")
    expect(capturedAuth).toBe("Bearer sk-upstream-test-key")
  })
})

describe("custom provider routing — /anthropic/v1/messages", () => {
  it("routes a custom anthropic-format slug natively with x-api-key auth", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedCustomProvider(db, { slug: "my-claude", format: "anthropic", userId: "user_1" })
    let capturedUrl: string | undefined
    let capturedHeaders: Record<string, string> | undefined
    let capturedBody: Record<string, unknown> | undefined
    globalThis.fetch = (async (url: string, init?: RequestInit) => {
      capturedUrl = url
      capturedHeaders = init?.headers as Record<string, string>
      capturedBody = JSON.parse(String(init?.body))
      return new Response(
        JSON.stringify({ id: "msg_1", content: [{ type: "text", text: "hi" }], usage: {} }),
        { status: 200 },
      )
    }) as typeof fetch

    const res = await app.request(
      "/anthropic/v1/messages",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({ model: "my-claude/claude-3", messages: [{ role: "user", content: "hi" }] }),
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(200)
    expect(capturedUrl).toBe("https://upstream.example.com/v1/messages")
    expect(capturedHeaders?.["x-api-key"]).toBe("sk-upstream-test-key")
    expect(capturedBody?.model).toBe("claude-3")
  })

  it("routes a custom openai-format slug via the Anthropic→OpenAI conversion path", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedCustomProvider(db, { slug: "my-oa2", format: "openai", userId: "user_1" })
    let capturedUrl: string | undefined
    globalThis.fetch = (async (url: string) => {
      capturedUrl = url
      return new Response(
        JSON.stringify({ id: "x", choices: [{ message: { role: "assistant", content: "hi" } }] }),
        { status: 200, headers: { "content-type": "application/json" } },
      )
    }) as typeof fetch

    const res = await app.request(
      "/anthropic/v1/messages",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "my-oa2/gpt-4o",
          max_tokens: 100,
          messages: [{ role: "user", content: "hi" }],
        }),
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(200)
    expect(capturedUrl).toBe("https://upstream.example.com/v1/chat/completions")
    const json = (await res.json()) as Record<string, unknown>
    expect(json.type).toBe("message")
  })
})

describe("custom provider routing — /anthropic/v1/messages/count_tokens", () => {
  it("forwards a custom anthropic-format slug through the same pool/failover loop", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedCustomProvider(db, { slug: "my-claude2", format: "anthropic", userId: "user_1" })
    let capturedUrl: string | undefined
    globalThis.fetch = (async (url: string) => {
      capturedUrl = url
      return new Response(JSON.stringify({ input_tokens: 7 }), { status: 200 })
    }) as typeof fetch

    const res = await app.request(
      "/anthropic/v1/messages/count_tokens",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({ model: "my-claude2/claude-3", messages: [{ role: "user", content: "hi" }] }),
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(200)
    expect(capturedUrl).toBe("https://upstream.example.com/v1/messages/count_tokens")
  })

  it("rejects a custom openai-format slug with the same 400 grok/codex get", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedCustomProvider(db, { slug: "my-oa3", format: "openai", userId: "user_1" })
    globalThis.fetch = (async () => {
      throw new Error("must not call upstream — count_tokens has no openai equivalent")
    }) as typeof fetch

    const res = await app.request(
      "/anthropic/v1/messages/count_tokens",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({ model: "my-oa3/gpt-4o", messages: [{ role: "user", content: "hi" }] }),
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(400)
    const json = (await res.json()) as Record<string, unknown>
    expect(json).toEqual({
      type: "error",
      error: {
        type: "invalid_request_error",
        message: "count_tokens is only supported for claude-code models",
      },
    })
  })

  it("forwards a custom openai-format slug with count_tokens_url configured to that exact URL", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedCustomProvider(db, {
      slug: "my-oa4",
      format: "openai",
      userId: "user_1",
      countTokensUrl: "https://count.example.com/anthropic/count_tokens",
    })
    let capturedUrl: string | undefined
    let capturedHeaders: Record<string, string> | undefined
    let capturedBody: Record<string, unknown> | undefined
    globalThis.fetch = (async (url: string, init?: RequestInit) => {
      capturedUrl = url
      capturedHeaders = init?.headers as Record<string, string>
      capturedBody = JSON.parse(String(init?.body))
      return new Response(JSON.stringify({ input_tokens: 9 }), { status: 200 })
    }) as typeof fetch

    const res = await app.request(
      "/anthropic/v1/messages/count_tokens",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({ model: "my-oa4/gpt-4o", messages: [{ role: "user", content: "hi" }] }),
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(200)
    expect(capturedUrl).toBe("https://count.example.com/anthropic/count_tokens")
    expect(capturedHeaders?.authorization).toBe("Bearer sk-upstream-test-key")
    expect(capturedHeaders?.["x-api-key"]).toBe("sk-upstream-test-key")
    expect(capturedBody?.model).toBe("gpt-4o")
  })
})
