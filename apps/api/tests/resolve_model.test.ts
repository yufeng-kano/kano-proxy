import { describe, expect, it } from "vitest"
import { resolveModel } from "../src/providers/resolve"
import type { Env } from "../src/env"
import { FakeD1 } from "./helpers/fake_d1"

function envWith(db: FakeD1): Env {
  return { DB: db as unknown as D1Database } as unknown as Env
}

function seedProvider(
  db: FakeD1,
  overrides: Partial<{
    id: string
    user_id: string
    slug: string
    name: string
    format: string
    base_url: string
    models_mode: string
    manual_models_json: string | null
  }> = {},
) {
  db.seed("custom_providers", [
    {
      id: "cprov_1",
      user_id: "user_1",
      slug: "my-endpoint",
      name: "My Endpoint",
      format: "openai",
      base_url: "https://upstream.example.com/v1",
      models_mode: "auto",
      manual_models_json: null,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
      ...overrides,
    },
  ])
}

describe("resolveModel", () => {
  it("resolves a builtin provider without touching the DB", async () => {
    const db = new FakeD1()
    const resolved = await resolveModel(envWith(db), "user_1", "claude-code/claude-opus-5")
    expect(resolved).toMatchObject({
      provider: "claude-code",
      upstreamModel: "claude-opus-5",
      raw: "claude-code/claude-opus-5",
      isBuiltin: true,
    })
    expect(resolved?.adapter.id).toBe("claude-code")
  })

  it("resolves a custom slug (format=openai) to a custom-openai adapter", async () => {
    const db = new FakeD1()
    seedProvider(db, { format: "openai" })
    const resolved = await resolveModel(envWith(db), "user_1", "my-endpoint/gpt-4o")
    expect(resolved).not.toBeNull()
    expect(resolved!.isBuiltin).toBe(false)
    expect(resolved!.provider).toBe("my-endpoint")
    expect(resolved!.upstreamModel).toBe("gpt-4o")
    expect(resolved!.customProvider?.slug).toBe("my-endpoint")
    expect(resolved!.adapter.messages).toBeUndefined()
    expect(typeof resolved!.adapter.chatCompletions).toBe("function")
  })

  it("resolves a custom slug (format=anthropic) to a custom-anthropic adapter", async () => {
    const db = new FakeD1()
    seedProvider(db, { format: "anthropic" })
    const resolved = await resolveModel(envWith(db), "user_1", "my-endpoint/claude-3")
    expect(resolved).not.toBeNull()
    expect(resolved!.isBuiltin).toBe(false)
    expect(typeof resolved!.adapter.messages).toBe("function")
    expect(typeof resolved!.adapter.countTokens).toBe("function")
  })

  it("splits on the first slash only — nested slash in the upstream id", async () => {
    const db = new FakeD1()
    seedProvider(db)
    const resolved = await resolveModel(envWith(db), "user_1", "my-endpoint/org/model-name")
    expect(resolved?.upstreamModel).toBe("org/model-name")
    expect(resolved?.provider).toBe("my-endpoint")
  })

  it("never resolves a custom slug across users", async () => {
    const db = new FakeD1()
    seedProvider(db, { user_id: "user_1" })
    const resolved = await resolveModel(envWith(db), "user_2", "my-endpoint/gpt-4o")
    expect(resolved).toBeNull()
  })

  it("returns null for an unknown slug", async () => {
    const db = new FakeD1()
    const resolved = await resolveModel(envWith(db), "user_1", "nonexistent/model")
    expect(resolved).toBeNull()
  })

  it("returns null for a bare id with no provider prefix", async () => {
    const db = new FakeD1()
    const resolved = await resolveModel(envWith(db), "user_1", "gpt-4o")
    expect(resolved).toBeNull()
  })
})
