/**
 * Audio input on `/openai/v1` (docs/api.md § Audio input): the Gemini
 * conversion, the per-adapter capability declarations, and the two
 * pre-dispatch rejections. Route-level cases go through the real Hono app so
 * a guard that never runs cannot ship green.
 */
import { afterEach, describe, expect, it } from "vitest"
import { hashApiKey } from "../src/crypto/keys"
import { encryptJson } from "../src/crypto/token_crypto"
import type { Env } from "../src/env"
import { app } from "../src/index"
import { antigravityAdapter } from "../src/providers/antigravity"
import { claudeCodeAdapter } from "../src/providers/claude-code"
import { codexAdapter } from "../src/providers/codex"
import { grokAdapter } from "../src/providers/grok"
import type { ChatCompletionRequest } from "../src/providers/types"
import { openaiToGeminiRequest } from "../src/proxy/gemini_openai"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

const TOKEN_KEY = "test-token-encryption-key-not-secret"
const API_KEY_PLAINTEXT = "sk-kano-proxy-test-client-key-0001"
const DAILY = "https://daily-cloudcode-pa.googleapis.com"
/** Stand-in for a real clip; the converter only moves the bytes. */
const WAV = "UklGRiQAAABXQVZF"

const execCtx = {
  waitUntil: (p: Promise<unknown>) => {
    p.catch(() => {})
  },
  passThroughOnException: () => {},
} as unknown as ExecutionContext

function req(patch: Partial<ChatCompletionRequest>): ChatCompletionRequest {
  return {
    model: "antigravity/gemini-3-flash",
    rawModel: "antigravity/gemini-3-flash",
    upstreamModel: "gemini-3-flash",
    messages: [],
    rawBody: {},
    ...patch,
  }
}

function audioMessages(input_audio: Record<string, unknown>): unknown[] {
  return [
    {
      role: "user",
      content: [
        { type: "text", text: "what word is this" },
        { type: "input_audio", input_audio },
      ],
    },
  ]
}

function buildEnv(db: FakeD1): Env {
  return {
    DB: db as unknown as D1Database,
    BENCH: fakeKV(),
    CACHE: fakeKV(),
    APP_URL: "https://app.example.com",
    TOKEN_ENCRYPTION_KEY: TOKEN_KEY,
  } as unknown as Env
}

async function seed(db: FakeD1, providers: string[]): Promise<void> {
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
  const encrypted = await encryptJson(TOKEN_KEY, {
    access_token: "at-1",
    refresh_token: "rt-1",
    expires_at: new Date(Date.now() + 3_600_000).toISOString(),
    extra: { project_id: "proj-42" },
  })
  db.seed(
    "upstream_accounts",
    providers.map((provider, i) => ({
      id: `acc_${provider}`,
      user_id: "user_1",
      provider,
      external_account_id: null,
      label: "a@b.com",
      custom_label: null,
      priority: i + 1,
      encrypted_payload: encrypted,
      account_meta_json: null,
      usage_snapshot_json: null,
      usage_fetched_at: null,
      usage_fetching_at: null,
      bench_until: null,
      bench_reason: null,
      refreshing_at: null,
      edge_strikes: null,
      edge_strike_at: null,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    })),
  )
}

function authHeaders(): Record<string, string> {
  return { authorization: `Bearer ${API_KEY_PLAINTEXT}`, "content-type": "application/json" }
}

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

type Call = { url: string; init: RequestInit }

function stubUpstream(response: () => Response): Call[] {
  const calls: Call[] = []
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    calls.push({ url: String(input), init: init ?? {} })
    return response()
  }) as typeof fetch
  return calls
}

function geminiOk(text: string): Response {
  return new Response(
    JSON.stringify({
      response: {
        candidates: [{ content: { role: "model", parts: [{ text }] }, finishReason: "STOP" }],
        usageMetadata: { promptTokenCount: 32, candidatesTokenCount: 1, totalTokenCount: 33 },
      },
    }),
    { status: 200, headers: { "content-type": "application/json" } },
  )
}

describe("input_audio → Gemini inlineData", () => {
  it("maps format wav to audio/wav alongside the text part", () => {
    const out = openaiToGeminiRequest(req({ messages: audioMessages({ data: WAV, format: "wav" }) }))
    expect(out.contents[0].parts).toEqual([
      { text: "what word is this" },
      { inlineData: { mimeType: "audio/wav", data: WAV } },
    ])
  })

  it("folds the mp3/mpeg spellings onto the one mime Gemini names", () => {
    for (const format of ["mp3", "MP3", "mpeg"]) {
      const out = openaiToGeminiRequest(req({ messages: audioMessages({ data: WAV, format }) }))
      expect(out.contents[0].parts![1]).toEqual({
        inlineData: { mimeType: "audio/mp3", data: WAV },
      })
    }
  })

  it("prefers a data: URL's own mime over the format field", () => {
    const out = openaiToGeminiRequest(
      req({ messages: audioMessages({ data: `data:audio/flac;base64,${WAV}`, format: "wav" }) }),
    )
    expect(out.contents[0].parts![1]).toEqual({
      inlineData: { mimeType: "audio/flac", data: WAV },
    })
  })
})

describe("audio capability per adapter", () => {
  it("declares convert for the Gemini wire and passthrough for verbatim bodies", () => {
    expect(antigravityAdapter.audioInput).toBe("convert")
    expect(grokAdapter.audioInput).toBe("passthrough")
  })

  it("leaves the Anthropic and Responses builders without an audio wire", () => {
    expect(claudeCodeAdapter.audioInput).toBeUndefined()
    expect(codexAdapter.audioInput).toBeUndefined()
  })
})

describe("audio on /openai/v1/chat/completions", () => {
  it("carries the clip to the CloudCode request as an inline part", async () => {
    const db = new FakeD1()
    await seed(db, ["antigravity"])
    const calls = stubUpstream(() => geminiOk("Banana"))

    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "antigravity/gemini-3-flash",
          messages: audioMessages({ data: WAV, format: "wav" }),
        }),
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(200)
    expect(calls[0]!.url).toBe(`${DAILY}/v1internal:generateContent`)
    const sent = JSON.parse(calls[0]!.init.body as string) as {
      request: { contents: Array<{ parts: unknown[] }> }
    }
    expect(sent.request.contents[0]!.parts).toContainEqual({
      inlineData: { mimeType: "audio/wav", data: WAV },
    })
  })

  it("rejects a provider whose wire has no audio part, before any upstream call", async () => {
    const db = new FakeD1()
    await seed(db, ["claude-code"])
    const calls = stubUpstream(() => new Response("{}", { status: 200 }))

    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "claude-code/claude-opus-5",
          messages: audioMessages({ data: WAV, format: "wav" }),
        }),
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(400)
    expect(res.headers.get("x-should-retry")).toBe("false")
    const json = (await res.json()) as { error: { code: string; message: string } }
    expect(json.error.code).toBe("unsupported_modality")
    expect(json.error.message).toContain("claude-code")
    expect(calls).toHaveLength(0)
    expect(db.rows("request_logs")[0]).toMatchObject({
      provider: "claude-code",
      status_code: 400,
      error_code: "unsupported_modality",
    })
  })

  it("rejects a format it cannot name a mime for instead of guessing one", async () => {
    const db = new FakeD1()
    await seed(db, ["antigravity"])
    const calls = stubUpstream(() => geminiOk("Banana"))

    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "antigravity/gemini-3-flash",
          messages: audioMessages({ data: WAV, format: "wma" }),
        }),
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(400)
    const json = (await res.json()) as { error: { code: string } }
    expect(json.error.code).toBe("unsupported_audio_format")
    expect(calls).toHaveLength(0)
    expect(db.rows("request_logs")[0]).toMatchObject({
      provider: "antigravity",
      error_code: "unsupported_audio_format",
    })
  })

  it("forwards the client's own part for a passthrough provider, unvalidated", async () => {
    const db = new FakeD1()
    await seed(db, ["grok"])
    const calls = stubUpstream(
      () =>
        new Response(JSON.stringify({ choices: [{ message: { content: "ok" } }] }), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
    )

    // `wma` has no mime here — a passthrough body is xAI's to judge, not ours.
    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "grok/grok-4.5",
          messages: audioMessages({ data: WAV, format: "wma" }),
        }),
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(200)
    expect(calls[0]!.url).toBe("https://api.x.ai/v1/chat/completions")
    const sent = JSON.parse(calls[0]!.init.body as string) as {
      messages: Array<{ content: unknown[] }>
    }
    expect(sent.messages[0]!.content).toContainEqual({
      type: "input_audio",
      input_audio: { data: WAV, format: "wma" },
    })
  })
})
