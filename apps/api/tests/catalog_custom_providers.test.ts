import { afterEach, describe, expect, it } from "vitest"
import { listModelsForUser, type ProviderModelsSection } from "../src/catalog/models"
import { encryptJson } from "../src/crypto/token_crypto"
import type { Env } from "../src/env"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

const FAKE_KEY = "test-token-encryption-key-not-secret"

function buildEnv(db: FakeD1): Env {
  return {
    DB: db as unknown as D1Database,
    CACHE: fakeKV(),
    BENCH: fakeKV(),
    TOKEN_ENCRYPTION_KEY: FAKE_KEY,
  } as unknown as Env
}

async function seedProvider(
  db: FakeD1,
  opts: {
    slug: string
    format: "openai" | "anthropic"
    modelsMode: "auto" | "manual"
    manualModelsJson?: string | null
    withAccount?: boolean
  },
): Promise<void> {
  db.seed("custom_providers", [
    {
      id: `cprov_${opts.slug}`,
      user_id: "user_1",
      slug: opts.slug,
      name: opts.slug,
      format: opts.format,
      base_url: opts.format === "openai" ? "https://upstream.example.com/v1" : "https://upstream.example.com",
      models_mode: opts.modelsMode,
      manual_models_json: opts.manualModelsJson ?? null,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
  if (opts.withAccount !== false) {
    const encrypted = await encryptJson(FAKE_KEY, { access_token: "sk-test" })
    db.seed("upstream_accounts", [
      {
        id: `acc_${opts.slug}`,
        user_id: "user_1",
        provider: opts.slug,
        external_account_id: null,
        label: opts.slug,
        priority: 1,
        encrypted_payload: encrypted,
        account_meta_json: null,
        created_at: "2026-01-01T00:00:00.000Z",
        updated_at: "2026-01-01T00:00:00.000Z",
      },
    ])
  }
}

function customSection(
  providers: ProviderModelsSection[],
  slug: string,
): ProviderModelsSection | undefined {
  return providers.find((p) => p.provider === slug)
}

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

describe("listModelsForUser — custom providers", () => {
  it("manual mode returns the stored list without ever fetching", async () => {
    const db = new FakeD1()
    await seedProvider(db, {
      slug: "manual-ep",
      format: "openai",
      modelsMode: "manual",
      manualModelsJson: JSON.stringify(["model-a", "model-b"]),
    })
    globalThis.fetch = (async () => {
      throw new Error("must not fetch in manual mode")
    }) as typeof fetch

    const { models, providers } = await listModelsForUser({ ...buildEnv(db) }, "user_1")
    const ids = models.filter((m) => m.provider === "manual-ep").map((m) => m.id)
    expect(ids).toEqual(["manual-ep/model-a", "manual-ep/model-b"])
    expect(customSection(providers, "manual-ep")?.error).toBeNull()
  })

  it("auto mode fetches live models and prefixes ids with the slug", async () => {
    const db = new FakeD1()
    await seedProvider(db, { slug: "auto-ep", format: "openai", modelsMode: "auto" })
    globalThis.fetch = (async () =>
      new Response(JSON.stringify({ data: [{ id: "live-model" }] }), { status: 200 })) as typeof fetch

    const { models } = await listModelsForUser(buildEnv(db), "user_1")
    const ids = models.filter((m) => m.provider === "auto-ep").map((m) => m.id)
    expect(ids).toEqual(["auto-ep/live-model"])
  })

  it("auto mode caches the live result for the 1h window (no second fetch)", async () => {
    const db = new FakeD1()
    await seedProvider(db, { slug: "cached-ep", format: "openai", modelsMode: "auto" })
    let calls = 0
    globalThis.fetch = (async () => {
      calls++
      return new Response(JSON.stringify({ data: [{ id: "m1" }] }), { status: 200 })
    }) as typeof fetch

    const env = buildEnv(db)
    await listModelsForUser(env, "user_1")
    await listModelsForUser(env, "user_1")
    expect(calls).toBe(1)
  })

  it("auto mode failure falls back to the manual list when it is non-empty", async () => {
    const db = new FakeD1()
    await seedProvider(db, {
      slug: "fallback-ep",
      format: "openai",
      modelsMode: "auto",
      manualModelsJson: JSON.stringify(["manual-fallback"]),
    })
    globalThis.fetch = (async () => new Response("server error", { status: 500 })) as typeof fetch

    const { models, providers } = await listModelsForUser(buildEnv(db), "user_1")
    const ids = models.filter((m) => m.provider === "fallback-ep").map((m) => m.id)
    expect(ids).toEqual(["fallback-ep/manual-fallback"])
    expect(customSection(providers, "fallback-ep")?.error).toBe("models 500")
  })

  it("auto mode failure with no manual list returns empty, not fabricated", async () => {
    const db = new FakeD1()
    await seedProvider(db, { slug: "empty-ep", format: "openai", modelsMode: "auto" })
    globalThis.fetch = (async () => new Response("server error", { status: 500 })) as typeof fetch

    const { models } = await listModelsForUser(buildEnv(db), "user_1")
    const ids = models.filter((m) => m.provider === "empty-ep").map((m) => m.id)
    expect(ids).toEqual([])
  })

  it("auto mode with no usable account falls back to the manual list without fetching", async () => {
    const db = new FakeD1()
    await seedProvider(db, {
      slug: "no-key-ep",
      format: "anthropic",
      modelsMode: "auto",
      manualModelsJson: JSON.stringify(["manual-only"]),
      withAccount: false,
    })
    globalThis.fetch = (async () => {
      throw new Error("must not fetch with no usable account")
    }) as typeof fetch

    const { models } = await listModelsForUser(buildEnv(db), "user_1")
    const ids = models.filter((m) => m.provider === "no-key-ep").map((m) => m.id)
    expect(ids).toEqual(["no-key-ep/manual-only"])
  })

  it("auto mode for a format=anthropic provider queries {base}/v1/models", async () => {
    const db = new FakeD1()
    await seedProvider(db, { slug: "claude-like", format: "anthropic", modelsMode: "auto" })
    let capturedUrl: string | undefined
    globalThis.fetch = (async (url: string) => {
      capturedUrl = url
      return new Response(JSON.stringify({ data: [{ id: "claude-3" }] }), { status: 200 })
    }) as typeof fetch

    await listModelsForUser(buildEnv(db), "user_1")
    expect(capturedUrl).toBe("https://upstream.example.com/v1/models")
  })
})
