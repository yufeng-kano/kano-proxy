import { afterEach, describe, expect, it } from "vitest"
import { app } from "../src/index"
import { hashApiKey } from "../src/crypto/keys"
import { encryptJson } from "../src/crypto/token_crypto"
import type { Env } from "../src/env"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

const TOKEN_KEY = "test-token-encryption-key-not-secret"
const API_KEY_PLAINTEXT = "sk-kano-proxy-test-client-key-0001"

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
  opts: { slug: string; format: "openai" | "anthropic"; userId: string; baseUrl?: string },
): Promise<void> {
  db.seed("custom_providers", [
    {
      id: `cprov_${opts.slug}`,
      user_id: opts.userId,
      slug: opts.slug,
      name: opts.slug,
      format: opts.format,
      base_url: opts.baseUrl ?? (opts.format === "openai" ? "https://upstream.example.com/v1" : "https://upstream.example.com"),
      count_tokens_url: null,
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

async function seedBuiltinAccount(
  db: FakeD1,
  opts: { provider: string; userId: string; id?: string },
): Promise<void> {
  const encrypted = await encryptJson(TOKEN_KEY, { access_token: "test-token" })
  db.seed("upstream_accounts", [
    {
      id: opts.id ?? `acc_${opts.provider}`,
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

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

describe("POST /openai/v1/audio/transcriptions", () => {
  it("rejects unauthenticated requests with 401", async () => {
    const db = new FakeD1()
    const formData = new FormData()
    formData.append("file", new Blob(["fake-audio"], { type: "audio/wav" }), "test.wav")
    formData.append("model", "openrouter/openai/whisper-large-v3-turbo")

    const res = await app.request(
      "/openai/v1/audio/transcriptions",
      {
        method: "POST",
        body: formData,
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(401)
  })

  it("returns 400 when model is missing", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    const formData = new FormData()
    formData.append("file", new Blob(["fake-audio"], { type: "audio/wav" }), "test.wav")

    const res = await app.request(
      "/openai/v1/audio/transcriptions",
      {
        method: "POST",
        headers: { authorization: `Bearer ${API_KEY_PLAINTEXT}` },
        body: formData,
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(400)
    const json = (await res.json()) as { error: { code: string; message: string } }
    expect(json.error.code).toBe("invalid_model")
  })

  it("returns 400 when file is missing", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    const formData = new FormData()
    formData.append("model", "openrouter/openai/whisper-large-v3-turbo")

    const res = await app.request(
      "/openai/v1/audio/transcriptions",
      {
        method: "POST",
        headers: { authorization: `Bearer ${API_KEY_PLAINTEXT}` },
        body: formData,
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(400)
    const json = (await res.json()) as { error: { code: string; message: string } }
    expect(json.error.code).toBe("invalid_request")
  })

  it("returns 400 invalid_model when provider is unknown", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    const formData = new FormData()
    formData.append("file", new Blob(["fake-audio"], { type: "audio/wav" }), "test.wav")
    formData.append("model", "unknown-prov/whisper")

    const res = await app.request(
      "/openai/v1/audio/transcriptions",
      {
        method: "POST",
        headers: { authorization: `Bearer ${API_KEY_PLAINTEXT}` },
        body: formData,
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(400)
    const json = (await res.json()) as { error: { code: string } }
    expect(json.error.code).toBe("invalid_model")
  })

  it("returns 400 unsupported_modality for built-in providers without audio transcription", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedBuiltinAccount(db, { provider: "claude-code", userId: "user_1" })
    const formData = new FormData()
    formData.append("file", new Blob(["fake-audio"], { type: "audio/wav" }), "test.wav")
    formData.append("model", "claude-code/claude-sonnet-5")

    const res = await app.request(
      "/openai/v1/audio/transcriptions",
      {
        method: "POST",
        headers: { authorization: `Bearer ${API_KEY_PLAINTEXT}` },
        body: formData,
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(400)
    const json = (await res.json()) as { error: { code: string; message: string } }
    expect(json.error.code).toBe("unsupported_modality")
    expect(json.error.message).toContain("audio transcription is not supported")
  })

  it("returns 400 unsupported_modality for custom anthropic-format provider", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedCustomProvider(db, { slug: "my-anthropic", format: "anthropic", userId: "user_1" })
    const formData = new FormData()
    formData.append("file", new Blob(["fake-audio"], { type: "audio/wav" }), "test.wav")
    formData.append("model", "my-anthropic/claude-3-5-sonnet")

    const res = await app.request(
      "/openai/v1/audio/transcriptions",
      {
        method: "POST",
        headers: { authorization: `Bearer ${API_KEY_PLAINTEXT}` },
        body: formData,
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(400)
    const json = (await res.json()) as { error: { code: string } }
    expect(json.error.code).toBe("unsupported_modality")
  })

  it("forwards audio transcription to custom openai provider with rewritten model and auth", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedCustomProvider(db, {
      slug: "openrouter",
      format: "openai",
      userId: "user_1",
      baseUrl: "https://openrouter.ai/api/v1",
    })

    let capturedUrl: string | undefined
    let capturedHeaders: HeadersInit | undefined
    let capturedFormData: FormData | undefined

    globalThis.fetch = (async (url: string, init?: RequestInit) => {
      capturedUrl = url
      capturedHeaders = init?.headers
      capturedFormData = init?.body as FormData
      return new Response(JSON.stringify({ text: "Transcription result test" }), {
        status: 200,
        headers: { "content-type": "application/json" },
      })
    }) as typeof fetch

    const formData = new FormData()
    formData.append("file", new Blob(["audio-bytes"], { type: "audio/wav" }), "speech.wav")
    formData.append("model", "openrouter/openai/whisper-large-v3-turbo")
    formData.append("language", "en")
    formData.append("response_format", "json")

    const res = await app.request(
      "/openai/v1/audio/transcriptions",
      {
        method: "POST",
        headers: { authorization: `Bearer ${API_KEY_PLAINTEXT}` },
        body: formData,
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(200)
    const body = (await res.json()) as { text: string }
    expect(body.text).toBe("Transcription result test")
    expect(capturedUrl).toBe("https://openrouter.ai/api/v1/audio/transcriptions")
    expect((capturedHeaders as Record<string, string>)?.authorization).toBe("Bearer sk-upstream-test-key")
    expect(capturedFormData?.get("model")).toBe("openai/whisper-large-v3-turbo")
    expect(capturedFormData?.get("language")).toBe("en")
    expect(capturedFormData?.get("response_format")).toBe("json")
    expect(capturedFormData?.get("file")).toBeInstanceOf(Blob)
  })

  it("passes through plain text or subtitle format responses from upstream", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedCustomProvider(db, {
      slug: "groq",
      format: "openai",
      userId: "user_1",
      baseUrl: "https://api.groq.com/openai/v1",
    })

    globalThis.fetch = (async () => {
      return new Response("1\n00:00:00,000 --> 00:00:02,000\nHello world\n", {
        status: 200,
        headers: { "content-type": "text/plain; charset=utf-8" },
      })
    }) as typeof fetch

    const formData = new FormData()
    formData.append("file", new Blob(["audio-bytes"], { type: "audio/mp3" }), "test.mp3")
    formData.append("model", "groq/whisper-large-v3")
    formData.append("response_format", "srt")

    const res = await app.request(
      "/openai/v1/audio/transcriptions",
      {
        method: "POST",
        headers: { authorization: `Bearer ${API_KEY_PLAINTEXT}` },
        body: formData,
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(200)
    expect(res.headers.get("content-type")).toContain("text/plain")
    const text = await res.text()
    expect(text).toContain("00:00:00,000 --> 00:00:02,000")
  })

  it("benches account on 401/402/403/429 and fails over to next candidate", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")

    // Seed custom provider with 2 accounts
    db.seed("custom_providers", [
      {
        id: "cprov_groq",
        user_id: "user_1",
        slug: "groq",
        name: "groq",
        format: "openai",
        base_url: "https://api.groq.com/openai/v1",
        count_tokens_url: null,
        models_mode: "auto",
        manual_models_json: null,
        created_at: "2026-01-01T00:00:00.000Z",
        updated_at: "2026-01-01T00:00:00.000Z",
      },
    ])
    const encrypted1 = await encryptJson(TOKEN_KEY, { access_token: "sk-key-1" })
    const encrypted2 = await encryptJson(TOKEN_KEY, { access_token: "sk-key-2" })
    db.seed("upstream_accounts", [
      {
        id: "acc_1",
        user_id: "user_1",
        provider: "groq",
        external_account_id: null,
        label: "acc 1",
        priority: 10,
        encrypted_payload: encrypted1,
        account_meta_json: null,
        usage_snapshot_json: null,
        usage_fetched_at: null,
        usage_fetching_at: null,
        created_at: "2026-01-01T00:00:00.000Z",
        updated_at: "2026-01-01T00:00:00.000Z",
      },
      {
        id: "acc_2",
        user_id: "user_1",
        provider: "groq",
        external_account_id: null,
        label: "acc 2",
        priority: 5,
        encrypted_payload: encrypted2,
        account_meta_json: null,
        usage_snapshot_json: null,
        usage_fetched_at: null,
        usage_fetching_at: null,
        created_at: "2026-01-01T00:00:00.000Z",
        updated_at: "2026-01-01T00:00:00.000Z",
      },
    ])

    let attempt = 0
    globalThis.fetch = (async (_url: string, init?: RequestInit) => {
      attempt++
      const auth = (init?.headers as Record<string, string>)?.authorization
      if (auth === "Bearer sk-key-1") {
        return new Response(JSON.stringify({ error: { message: "rate limit", code: "rate_limit_exceeded" } }), {
          status: 429,
          headers: { "content-type": "application/json" },
        })
      }
      return new Response(JSON.stringify({ text: "successful failover transcript" }), {
        status: 200,
        headers: { "content-type": "application/json" },
      })
    }) as typeof fetch

    const formData = new FormData()
    formData.append("file", new Blob(["audio-bytes"], { type: "audio/wav" }), "test.wav")
    formData.append("model", "groq/whisper-large-v3")

    const res = await app.request(
      "/openai/v1/audio/transcriptions",
      {
        method: "POST",
        headers: { authorization: `Bearer ${API_KEY_PLAINTEXT}` },
        body: formData,
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(200)
    const json = (await res.json()) as { text: string }
    expect(json.text).toBe("successful failover transcript")
    expect(attempt).toBe(2)
  })
})

describe("POST /g/:slug/openai/v1/audio/transcriptions (Group endpoints)", () => {
  it("returns 404 for unknown group slug", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    const formData = new FormData()
    formData.append("file", new Blob(["fake-audio"], { type: "audio/wav" }), "test.wav")
    formData.append("model", "whisper")

    const res = await app.request(
      "/g/unknown-group/openai/v1/audio/transcriptions",
      {
        method: "POST",
        headers: { authorization: `Bearer ${API_KEY_PLAINTEXT}` },
        body: formData,
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(404)
  })

  it("resolves group model name and forwards to configured custom openai target", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedCustomProvider(db, {
      slug: "openrouter",
      format: "openai",
      userId: "user_1",
      baseUrl: "https://openrouter.ai/api/v1",
    })

    db.seed("model_groups", [
      {
        id: "grp_1",
        user_id: "user_1",
        slug: "voice-tools",
        name: "Voice Tools",
        strategy: "ordered",
        created_at: "2026-01-01T00:00:00.000Z",
        updated_at: "2026-01-01T00:00:00.000Z",
      },
    ])
    db.seed("model_group_models", [
      {
        id: "mgm_1",
        user_id: "user_1",
        group_id: "grp_1",
        name: "whisper",
        targets_json: JSON.stringify([{ model: "openrouter/openai/whisper-large-v3-turbo", account_id: null }]),
        created_at: "2026-01-01T00:00:00.000Z",
        updated_at: "2026-01-01T00:00:00.000Z",
      },
    ])

    let capturedUrl: string | undefined
    let capturedFormData: FormData | undefined

    globalThis.fetch = (async (url: string, init?: RequestInit) => {
      capturedUrl = url
      capturedFormData = init?.body as FormData
      return new Response(JSON.stringify({ text: "group audio transcription" }), {
        status: 200,
        headers: { "content-type": "application/json" },
      })
    }) as typeof fetch

    const formData = new FormData()
    formData.append("file", new Blob(["audio-bytes"], { type: "audio/wav" }), "test.wav")
    formData.append("model", "whisper")

    const res = await app.request(
      "/g/voice-tools/openai/v1/audio/transcriptions",
      {
        method: "POST",
        headers: { authorization: `Bearer ${API_KEY_PLAINTEXT}` },
        body: formData,
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(200)
    const json = (await res.json()) as { text: string }
    expect(json.text).toBe("group audio transcription")
    expect(capturedUrl).toBe("https://openrouter.ai/api/v1/audio/transcriptions")
    expect(capturedFormData?.get("model")).toBe("openai/whisper-large-v3-turbo")
  })
})

describe("POST /openai/v1/audio/transcriptions advanced behavior", () => {
  it("captures usage when present in upstream response", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedCustomProvider(db, {
      slug: "openrouter",
      format: "openai",
      userId: "user_1",
    })

    globalThis.fetch = (async () => {
      return new Response(
        JSON.stringify({
          text: "transcribed text",
          usage: { prompt_tokens: 15, completion_tokens: 5, total_tokens: 20 },
        }),
        {
          status: 200,
          headers: {
            "content-type": "application/json",
            "content-disposition": "attachment; filename=\"transcript.json\"",
          },
        },
      )
    }) as typeof fetch

    const formData = new FormData()
    formData.append("file", new Blob(["audio-bytes"], { type: "audio/wav" }), "test.wav")
    formData.append("model", "openrouter/openai/whisper-large-v3-turbo")

    const res = await app.request(
      "/openai/v1/audio/transcriptions",
      {
        method: "POST",
        headers: { authorization: `Bearer ${API_KEY_PLAINTEXT}` },
        body: formData,
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(200)
    expect(res.headers.get("content-disposition")).toBe('attachment; filename="transcript.json"')
    const json = (await res.json()) as { text: string; usage: { prompt_tokens: number } }
    expect(json.text).toBe("transcribed text")

    // Check request_logs in D1
    const logRows = db.rows("request_logs")
    expect(logRows.length).toBeGreaterThan(0)
    const lastRow = logRows[logRows.length - 1]
    expect(lastRow.prompt_tokens).toBe(15)
    expect(lastRow.completion_tokens).toBe(5)
  })

  it("retries 529 once on the same account before failing over", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedCustomProvider(db, {
      slug: "groq",
      format: "openai",
      userId: "user_1",
    })

    let calls = 0
    globalThis.fetch = (async () => {
      calls++
      if (calls === 1) {
        return new Response(JSON.stringify({ error: "overloaded" }), { status: 529 })
      }
      return new Response(JSON.stringify({ text: "recovered after 529" }), {
        status: 200,
        headers: { "content-type": "application/json" },
      })
    }) as typeof fetch

    const formData = new FormData()
    formData.append("file", new Blob(["audio-bytes"], { type: "audio/wav" }), "test.wav")
    formData.append("model", "groq/whisper-large-v3")

    const res = await app.request(
      "/openai/v1/audio/transcriptions",
      {
        method: "POST",
        headers: { authorization: `Bearer ${API_KEY_PLAINTEXT}` },
        body: formData,
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(200)
    expect(calls).toBe(2)
    const json = (await res.json()) as { text: string }
    expect(json.text).toBe("recovered after 529")
  })
})

  it("logs errorCode upstream_error when upstream returns non-2xx without failover", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedCustomProvider(db, {
      slug: "groq",
      format: "openai",
      userId: "user_1",
    })

    globalThis.fetch = (async () => {
      return new Response(JSON.stringify({ error: { message: "corrupt audio file" } }), {
        status: 400,
        headers: { "content-type": "application/json" },
      })
    }) as typeof fetch

    const formData = new FormData()
    formData.append("file", new Blob(["bad-bytes"], { type: "audio/wav" }), "test.wav")
    formData.append("model", "groq/whisper-large-v3")

    const res = await app.request(
      "/openai/v1/audio/transcriptions",
      {
        method: "POST",
        headers: { authorization: `Bearer ${API_KEY_PLAINTEXT}` },
        body: formData,
      },
      buildEnv(db),
      execCtx,
    )

    expect(res.status).toBe(400)
    await res.text()
    const logRows = db.rows("request_logs")
    const lastRow = logRows[logRows.length - 1]
    expect(lastRow.error_code).toBe("upstream_error")
    expect(lastRow.status_code).toBe(400)
  })
