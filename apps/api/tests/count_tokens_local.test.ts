/**
 * Locally answered count_tokens (docs/api.md § count_tokens): codex through
 * the relay's o200k_base tokenizer, grok / url-less custom-openai with the
 * sentinel zero. The load-bearing property in every case: a 200 with a
 * number, never an error status — a failing count_tokens sends Claude Code
 * into a parallel max_tokens:1 probe burst against the real upstream
 * (measured 2026-08-22).
 */
import { afterEach, describe, expect, it } from "vitest"
import { hashApiKey } from "../src/crypto/keys"
import type { Env } from "../src/env"
import { app } from "../src/index"
import { anthropicCountTexts } from "../src/providers/codex_count"
import { resetCodexRelayCacheForTests } from "../src/providers/codex_relay"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

const TOKEN_EXCHANGE_URL = "https://oauth2.googleapis.com/token"
const RELAY_URL = "https://kano-codex-relay-abc123-uc.a.run.app"
const API_KEY_PLAINTEXT = "sk-kano-proxy-test-client-key-0001"

const execCtx = {
  waitUntil: (p: Promise<unknown>) => {
    p.catch(() => {})
  },
  passThroughOnException: () => {},
} as unknown as ExecutionContext

async function seedApiKey(db: FakeD1): Promise<void> {
  db.seed("api_keys", [
    {
      id: "key_1",
      user_id: "user_1",
      name: "test key",
      key_prefix: API_KEY_PLAINTEXT.slice(0, 20),
      key_hash: await hashApiKey(API_KEY_PLAINTEXT),
      created_at: "2026-01-01T00:00:00.000Z",
      last_used_at: null,
    },
  ])
}

function buildEnv(db: FakeD1, relay: boolean): Env {
  return {
    DB: db as unknown as D1Database,
    BENCH: fakeKV(),
    CACHE: fakeKV(),
    APP_URL: "https://app.example.com",
    TOKEN_ENCRYPTION_KEY: "test-token-encryption-key-not-secret",
    ...(relay ? { CODEX_RELAY_URL: RELAY_URL, CODEX_RELAY_SA_KEY: JSON.stringify(FAKE_SA) } : {}),
  } as unknown as Env
}

function authHeaders(): Record<string, string> {
  return { authorization: `Bearer ${API_KEY_PLAINTEXT}`, "content-type": "application/json" }
}

/**
 * mintIdToken signs a real RS256 assertion, so the fake key must be a real
 * (test-only, throwaway) RSA private key — same approach as
 * codex_relay.test.ts `fakeServiceAccountKey`, hoisted once per file here.
 */
let FAKE_SA: { client_email: string; private_key: string }
async function ensureFakeSa(): Promise<void> {
  if (FAKE_SA) return
  const keyPair = (await crypto.subtle.generateKey(
    {
      name: "RSASSA-PKCS1-v1_5",
      modulusLength: 2048,
      publicExponent: new Uint8Array([1, 0, 1]),
      hash: "SHA-256",
    },
    true,
    ["sign", "verify"],
  )) as CryptoKeyPair
  const pkcs8 = new Uint8Array(
    (await crypto.subtle.exportKey("pkcs8", keyPair.privateKey)) as ArrayBuffer,
  )
  let bin = ""
  for (const b of pkcs8) bin += String.fromCharCode(b)
  const base64 = btoa(bin)
  const lines = base64.match(/.{1,64}/g) ?? [base64]
  FAKE_SA = {
    client_email: "kano-relay-invoker@test-project.iam.gserviceaccount.com",
    private_key: `-----BEGIN PRIVATE KEY-----\n${lines.join("\n")}\n-----END PRIVATE KEY-----\n`,
  }
}

function base64UrlJson(value: unknown): string {
  const bytes = new TextEncoder().encode(JSON.stringify(value))
  let s = ""
  for (const b of bytes) s += String.fromCharCode(b)
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "")
}

function fakeIdToken(): string {
  const exp = Math.floor(Date.now() / 1000) + 3600
  return `${base64UrlJson({ alg: "RS256", typ: "JWT" })}.${base64UrlJson({ exp })}.fake-signature`
}

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
  resetCodexRelayCacheForTests()
})

describe("count_tokens local answers", () => {
  it("grok: sentinel zero, no upstream call, no account needed", async () => {
    const db = new FakeD1()
    await seedApiKey(db)
    let fetchCalled = false
    globalThis.fetch = (async () => {
      fetchCalled = true
      return new Response("must not be called")
    }) as typeof fetch

    const res = await app.request(
      "/anthropic/v1/messages/count_tokens",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({ model: "grok/grok-4.5", messages: [{ role: "user", content: "hi" }] }),
      },
      buildEnv(db, false),
      execCtx,
    )

    expect(res.status).toBe(200)
    expect(await res.json()).toEqual({ input_tokens: 0 })
    expect(fetchCalled).toBe(false)
  })

  it("codex with the relay configured: answers the relay's o200k_base count", async () => {
    await ensureFakeSa()
    const db = new FakeD1()
    await seedApiKey(db)

    let countedTexts: string[] | undefined
    globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      if (url === TOKEN_EXCHANGE_URL) {
        return Response.json({ id_token: fakeIdToken() })
      }
      if (url === `${RELAY_URL}/count-tokens`) {
        const headers = new Headers(init?.headers)
        expect(headers.get("x-serverless-authorization")).toMatch(/^Bearer /)
        countedTexts = (JSON.parse(String(init?.body)) as { texts: string[] }).texts
        return Response.json({ tokens: 4321 }, { headers: { "x-relay-count": "1" } })
      }
      throw new Error(`unexpected fetch: ${url}`)
    }) as typeof fetch

    const res = await app.request(
      "/anthropic/v1/messages/count_tokens",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "codex/gpt-5.6-luna",
          system: "be terse",
          messages: [{ role: "user", content: "hello" }],
          tools: [{ name: "get_weather", description: "weather", input_schema: { type: "object" } }],
        }),
      },
      buildEnv(db, true),
      execCtx,
    )

    expect(res.status).toBe(200)
    expect(await res.json()).toEqual({ input_tokens: 4321 })
    expect(countedTexts).toEqual([
      "be terse",
      "hello",
      "get_weather",
      "weather",
      JSON.stringify({ type: "object" }),
    ])
  })

  it("codex with the relay unreachable: degrades to the sentinel zero, never an error status", async () => {
    await ensureFakeSa()
    const db = new FakeD1()
    await seedApiKey(db)
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      if (String(input) === TOKEN_EXCHANGE_URL) return Response.json({ id_token: fakeIdToken() })
      throw new Error("relay unreachable")
    }) as typeof fetch

    const res = await app.request(
      "/anthropic/v1/messages/count_tokens",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({ model: "codex/gpt-5.6-luna", messages: [{ role: "user", content: "hi" }] }),
      },
      buildEnv(db, true),
      execCtx,
    )

    expect(res.status).toBe(200)
    expect(await res.json()).toEqual({ input_tokens: 0 })
  })

  it("codex with no relay configured: sentinel zero without touching the network", async () => {
    const db = new FakeD1()
    await seedApiKey(db)
    let fetchCalled = false
    globalThis.fetch = (async () => {
      fetchCalled = true
      return new Response("must not be called")
    }) as typeof fetch

    const res = await app.request(
      "/anthropic/v1/messages/count_tokens",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({ model: "codex/gpt-5.6-luna", messages: [{ role: "user", content: "hi" }] }),
      },
      buildEnv(db, false),
      execCtx,
    )

    expect(res.status).toBe(200)
    expect(await res.json()).toEqual({ input_tokens: 0 })
    expect(fetchCalled).toBe(false)
  })
})

describe("anthropicCountTexts", () => {
  it("serializes system blocks, message block types, and tool declarations", () => {
    const texts = anthropicCountTexts({
      system: [{ type: "text", text: "sys" }],
      messages: [
        { role: "user", content: "plain" },
        {
          role: "assistant",
          content: [
            { type: "thinking", thinking: "hmm", signature: "sig" },
            { type: "text", text: "answer" },
            { type: "tool_use", id: "t1", name: "run", input: { cmd: "ls" } },
          ],
        },
        {
          role: "user",
          content: [
            { type: "tool_result", tool_use_id: "t1", content: [{ type: "text", text: "ok" }] },
          ],
        },
      ],
      tools: [{ name: "run", description: "runs", input_schema: { type: "object" } }],
    })
    expect(texts).toEqual([
      "sys",
      "plain",
      "hmm",
      "answer",
      "run",
      JSON.stringify({ cmd: "ls" }),
      "ok",
      "run",
      "runs",
      JSON.stringify({ type: "object" }),
    ])
  })

  it("skips images and redacted thinking rather than inventing text for them", () => {
    const texts = anthropicCountTexts({
      messages: [
        {
          role: "user",
          content: [
            { type: "image", source: { type: "base64", media_type: "image/png", data: "AAAA" } },
            { type: "redacted_thinking", data: "opaque" },
            { type: "text", text: "visible" },
          ],
        },
      ],
    })
    expect(texts).toEqual(["visible"])
  })
})
