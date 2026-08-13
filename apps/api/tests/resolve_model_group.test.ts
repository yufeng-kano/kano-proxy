import { describe, expect, it } from "vitest"
import { resolveModel } from "../src/providers/resolve"
import { markBenched } from "../src/pool/bench"
import type { Env } from "../src/env"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

function envWith(db: FakeD1): Env {
  return { DB: db as unknown as D1Database, BENCH: fakeKV() } as unknown as Env
}

function seedGroup(
  db: FakeD1,
  overrides: Partial<{ id: string; user_id: string; name: string; targets_json: string }> = {},
) {
  db.seed("model_groups", [
    {
      id: "mgrp_1",
      user_id: "user_1",
      name: "opus",
      targets_json: JSON.stringify(["claude-code/claude-opus-5"]),
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
      ...overrides,
    },
  ])
}

function seedCustomProvider(db: FakeD1, slug: string, format: "openai" | "anthropic" = "openai") {
  db.seed("custom_providers", [
    {
      id: `cprov_${slug}`,
      user_id: "user_1",
      slug,
      name: slug,
      format,
      base_url: "https://upstream.example.com/v1",
      models_mode: "auto",
      manual_models_json: null,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
}

let accSeq = 0
function seedAccount(db: FakeD1, provider: string, userId = "user_1"): string {
  const id = `acc_${provider}_${accSeq++}`
  db.seed("upstream_accounts", [
    {
      id,
      user_id: userId,
      provider,
      external_account_id: null,
      label: provider,
      priority: 1,
      encrypted_payload: "encrypted",
      account_meta_json: null,
      usage_snapshot_json: null,
      usage_fetched_at: null,
      usage_fetching_at: null,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
  return id
}

describe("resolveModel — model groups", () => {
  it("returns null for an unknown group name (miss)", async () => {
    const db = new FakeD1()
    const resolved = await resolveModel(envWith(db), "user_1", "no-such-group")
    expect(resolved).toBeNull()
  })

  it("never resolves another user's group", async () => {
    const db = new FakeD1()
    seedGroup(db, { user_id: "user_1" })
    seedAccount(db, "claude-code")
    const resolved = await resolveModel(envWith(db), "user_2", "opus")
    expect(resolved).toBeNull()
  })

  it("single usable target: resolves to it, raw stays the bare group name (echo), group.name is set", async () => {
    const db = new FakeD1()
    seedGroup(db, { targets_json: JSON.stringify(["claude-code/claude-opus-5"]) })
    seedAccount(db, "claude-code")
    const resolved = await resolveModel(envWith(db), "user_1", "opus")
    expect(resolved).toMatchObject({
      provider: "claude-code",
      upstreamModel: "claude-opus-5",
      raw: "opus",
      isBuiltin: true,
      group: { name: "opus" },
    })
  })

  it("trims the input group name", async () => {
    const db = new FakeD1()
    seedGroup(db)
    seedAccount(db, "claude-code")
    const resolved = await resolveModel(envWith(db), "user_1", "  opus  ")
    expect(resolved?.raw).toBe("opus")
  })

  it("first target usable: picks the first target, never touches the second", async () => {
    const db = new FakeD1()
    seedGroup(db, {
      targets_json: JSON.stringify(["claude-code/claude-opus-5", "grok/grok-4.5"]),
    })
    seedAccount(db, "claude-code")
    seedAccount(db, "grok")
    const resolved = await resolveModel(envWith(db), "user_1", "opus")
    expect(resolved?.provider).toBe("claude-code")
    expect(resolved?.upstreamModel).toBe("claude-opus-5")
  })

  it("first target benched, second has a usable account: picks the second", async () => {
    const db = new FakeD1()
    const env = envWith(db)
    seedGroup(db, {
      targets_json: JSON.stringify(["claude-code/claude-opus-5", "grok/grok-4.5"]),
    })
    const ccAcc = seedAccount(db, "claude-code")
    seedAccount(db, "grok")
    await markBenched(env, "user_1", "claude-code", ccAcc, 300_000)

    const resolved = await resolveModel(env, "user_1", "opus")
    expect(resolved?.provider).toBe("grok")
    expect(resolved?.upstreamModel).toBe("grok-4.5")
  })

  it("a target whose custom-provider prefix was deleted is skipped, next resolvable target wins", async () => {
    const db = new FakeD1()
    // "gone" is never seeded as a custom_providers row — simulates deletion.
    seedGroup(db, {
      targets_json: JSON.stringify(["gone/some-model", "claude-code/claude-opus-5"]),
    })
    seedAccount(db, "claude-code")
    const resolved = await resolveModel(envWith(db), "user_1", "opus")
    expect(resolved?.provider).toBe("claude-code")
    expect(resolved?.isBuiltin).toBe(true)
  })

  it("resolves a custom-provider target to its adapter", async () => {
    const db = new FakeD1()
    seedCustomProvider(db, "my-endpoint", "openai")
    seedGroup(db, { targets_json: JSON.stringify(["my-endpoint/gpt-4o"]) })
    seedAccount(db, "my-endpoint")
    const resolved = await resolveModel(envWith(db), "user_1", "opus")
    expect(resolved?.provider).toBe("my-endpoint")
    expect(resolved?.isBuiltin).toBe(false)
    expect(resolved?.customProvider?.slug).toBe("my-endpoint")
  })

  it("all resolvable targets benched: dispatches to the first resolvable target that has any bound account anyway", async () => {
    const db = new FakeD1()
    const env = envWith(db)
    seedGroup(db, {
      targets_json: JSON.stringify(["claude-code/claude-opus-5", "grok/grok-4.5"]),
    })
    const ccAcc = seedAccount(db, "claude-code")
    const grokAcc = seedAccount(db, "grok")
    await markBenched(env, "user_1", "claude-code", ccAcc, 300_000)
    await markBenched(env, "user_1", "grok", grokAcc, 300_000)

    const resolved = await resolveModel(env, "user_1", "opus")
    // First resolvable target wins the "any bound account" fallback.
    expect(resolved?.provider).toBe("claude-code")
  })

  it("no target has any bound account: dispatches to the first resolvable target period (yields no_upstream_account downstream)", async () => {
    const db = new FakeD1()
    seedGroup(db, {
      targets_json: JSON.stringify(["claude-code/claude-opus-5", "grok/grok-4.5"]),
    })
    // No accounts seeded at all.
    const resolved = await resolveModel(envWith(db), "user_1", "opus")
    expect(resolved?.provider).toBe("claude-code")
  })

  it("no target resolves at all (every prefix unknown/deleted): behaves as invalid_model (null)", async () => {
    const db = new FakeD1()
    seedGroup(db, { targets_json: JSON.stringify(["gone-1/model", "gone-2/model"]) })
    const resolved = await resolveModel(envWith(db), "user_1", "opus")
    expect(resolved).toBeNull()
  })

  it("a second-priority target with a usable account wins over a first-priority target with accounts but all benched, before falling back to 'any account' semantics", async () => {
    // This is the same as the "first usable" test above, restated to make
    // the docs' ordering explicit: usable beats "has accounts but benched".
    const db = new FakeD1()
    const env = envWith(db)
    seedGroup(db, {
      targets_json: JSON.stringify(["claude-code/claude-opus-5", "grok/grok-4.5"]),
    })
    const ccAcc = seedAccount(db, "claude-code")
    seedAccount(db, "grok")
    await markBenched(env, "user_1", "claude-code", ccAcc, 300_000)
    const resolved = await resolveModel(env, "user_1", "opus")
    expect(resolved?.provider).toBe("grok")
  })
})
