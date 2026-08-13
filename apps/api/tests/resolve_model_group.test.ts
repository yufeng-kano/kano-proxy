import { describe, expect, it } from "vitest"
import { resolveModel } from "../src/providers/resolve"
import { markBenched } from "../src/pool/bench"
import type { Env } from "../src/env"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

function envWith(db: FakeD1): Env {
  return { DB: db as unknown as D1Database, BENCH: fakeKV() } as unknown as Env
}

/**
 * Seeds a group row plus its `model_group_aliases` rows — resolution reads
 * the alias table, not `model_groups.name` (which is a free-text display
 * name since `0009_model_group_aliases.sql`). Defaults to a single alias
 * matching `name`, so existing tests that resolve by "opus" keep working
 * unchanged; pass `aliases` explicitly to exercise multi-alias resolution.
 */
function seedGroup(
  db: FakeD1,
  overrides: Partial<{
    id: string
    user_id: string
    name: string
    targets_json: string
    aliases: string[]
  }> = {},
) {
  const id = overrides.id ?? "mgrp_1"
  const userId = overrides.user_id ?? "user_1"
  const name = overrides.name ?? "opus"
  db.seed("model_groups", [
    {
      id,
      user_id: userId,
      name,
      targets_json: overrides.targets_json ?? JSON.stringify(["claude-code/claude-opus-5"]),
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
  const aliases = overrides.aliases ?? [name]
  db.seed(
    "model_group_aliases",
    aliases.map((alias, i) => ({
      id: `${id}_alias_${i}`,
      user_id: userId,
      group_id: id,
      alias,
      created_at: "2026-01-01T00:00:00.000Z",
    })),
  )
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

  it("v3.0.0 string-shorthand rows still resolve unpinned, same as before", async () => {
    const db = new FakeD1()
    // Bare strings, not {model, account_id} objects — the pre-pinning wire shape.
    seedGroup(db, { targets_json: JSON.stringify(["claude-code/claude-opus-5"]) })
    seedAccount(db, "claude-code")
    const resolved = await resolveModel(envWith(db), "user_1", "opus")
    expect(resolved).toMatchObject({ provider: "claude-code", upstreamModel: "claude-opus-5" })
    expect(resolved?.pinnedAccountId).toBeUndefined()
  })

  it("a resolves-by-display-name lookup (pre-alias behavior) no longer works — the display name alone is not callable", async () => {
    const db = new FakeD1()
    // A group whose display name is "opus" but whose only alias is "gpt-4o":
    // sending "opus" as model must miss — resolution reads the alias table,
    // never model_groups.name (docs/providers.md § Model groups "Display name").
    seedGroup(db, { name: "opus", aliases: ["gpt-4o"] })
    seedAccount(db, "claude-code")
    const resolved = await resolveModel(envWith(db), "user_1", "opus")
    expect(resolved).toBeNull()
  })

  describe("aliases (docs/providers.md § Model groups \"Aliases\")", () => {
    it("migration-seeded alias resolves: a pre-0009 group row plus a seeded alias matching its then-name still resolves", async () => {
      const db = new FakeD1()
      // Simulates the 0009_model_group_aliases.sql seed: the group's
      // pre-migration `name` (its only callable id at the time) becomes its
      // first alias row, with no other change to the group row itself.
      seedGroup(db, { name: "legacy-opus", aliases: ["legacy-opus"] })
      seedAccount(db, "claude-code")
      const resolved = await resolveModel(envWith(db), "user_1", "legacy-opus")
      expect(resolved).toMatchObject({ provider: "claude-code", raw: "legacy-opus" })
    })

    it("multi-alias resolution: every alias of a group resolves to the same target list", async () => {
      const db = new FakeD1()
      seedGroup(db, {
        name: "OpenAI GPT-4o family",
        aliases: ["gpt-4o", "gpt-4", "gpt-4-turbo"],
        targets_json: JSON.stringify(["codex/gpt-5.2"]),
      })
      const byFirst = await resolveModel(envWith(db), "user_1", "gpt-4o")
      const bySecond = await resolveModel(envWith(db), "user_1", "gpt-4")
      const byThird = await resolveModel(envWith(db), "user_1", "gpt-4-turbo")
      expect(byFirst).toMatchObject({ provider: "codex", upstreamModel: "gpt-5.2", raw: "gpt-4o" })
      expect(bySecond).toMatchObject({ provider: "codex", upstreamModel: "gpt-5.2", raw: "gpt-4" })
      expect(byThird).toMatchObject({
        provider: "codex",
        upstreamModel: "gpt-5.2",
        raw: "gpt-4-turbo",
      })
      // Each alias echoes itself in `group.name` — the alias the client sent,
      // never the shared display name — since that feeds request_logs.group_name.
      expect(byFirst?.group?.name).toBe("gpt-4o")
      expect(bySecond?.group?.name).toBe("gpt-4")
    })

    it("an alias belonging to one group never resolves a sibling group of the same user", async () => {
      const db = new FakeD1()
      seedGroup(db, { id: "mgrp_a", name: "Group A", aliases: ["alias-a"] })
      seedGroup(db, {
        id: "mgrp_b",
        name: "Group B",
        aliases: ["alias-b"],
        targets_json: JSON.stringify(["grok/grok-4.5"]),
      })
      seedAccount(db, "claude-code")
      seedAccount(db, "grok")
      const resolved = await resolveModel(envWith(db), "user_1", "alias-b")
      expect(resolved?.provider).toBe("grok")
    })
  })

  describe("account pinning (docs/providers.md § Model groups \"Account pinning\")", () => {
    it("a pinned target with a usable account is selected; resolved carries pinnedAccountId", async () => {
      const db = new FakeD1()
      const acc = seedAccount(db, "claude-code")
      seedGroup(db, {
        targets_json: JSON.stringify([{ model: "claude-code/claude-opus-5", account_id: acc }]),
      })
      const resolved = await resolveModel(envWith(db), "user_1", "opus")
      expect(resolved).toMatchObject({
        provider: "claude-code",
        upstreamModel: "claude-opus-5",
        pinnedAccountId: acc,
      })
    })

    it("a pinned target bypasses the pool's own priority — a lower-priority pinned account still wins when it's first in the group", async () => {
      const db = new FakeD1()
      // Two claude-code accounts; the pool's own priority order would prefer
      // whichever has the higher `priority`, but group order pins the low-
      // priority one explicitly and it must still be the one selected.
      const highPriority = seedAccount(db, "claude-code")
      const lowPriority = seedAccount(db, "claude-code")
      seedGroup(db, {
        targets_json: JSON.stringify([{ model: "claude-code/claude-opus-5", account_id: lowPriority }]),
      })
      void highPriority
      const resolved = await resolveModel(envWith(db), "user_1", "opus")
      expect(resolved?.pinnedAccountId).toBe(lowPriority)
    })

    it("a benched pinned target is skipped in favor of the next target, even though the provider's pool has other usable accounts", async () => {
      const db = new FakeD1()
      const env = envWith(db)
      const pinned = seedAccount(db, "claude-code")
      // A sibling claude-code account exists and is NOT benched — but
      // failover to it is disabled for a pinned target, so it must not win.
      seedAccount(db, "claude-code")
      seedAccount(db, "grok")
      await markBenched(env, "user_1", "claude-code", pinned, 300_000)
      seedGroup(db, {
        targets_json: JSON.stringify([
          { model: "claude-code/claude-opus-5", account_id: pinned },
          "grok/grok-4.5",
        ]),
      })
      const resolved = await resolveModel(env, "user_1", "opus")
      expect(resolved?.provider).toBe("grok")
      expect(resolved?.pinnedAccountId).toBeUndefined()
    })

    it("a pinned target whose account was deleted is skipped in favor of the next target", async () => {
      const db = new FakeD1()
      seedAccount(db, "grok")
      seedGroup(db, {
        targets_json: JSON.stringify([
          { model: "claude-code/claude-opus-5", account_id: "acc_never_existed" },
          "grok/grok-4.5",
        ]),
      })
      const resolved = await resolveModel(envWith(db), "user_1", "opus")
      expect(resolved?.provider).toBe("grok")
    })

    it("a pinned target whose account belongs to a different provider than claimed is treated as unusable", async () => {
      const db = new FakeD1()
      // acc belongs to "grok", but the target claims "claude-code" — should
      // never happen via the write-time validated REST path, but resolution
      // still defends against it rather than trusting the stored pin blindly.
      const grokAcc = seedAccount(db, "grok")
      seedAccount(db, "codex")
      seedGroup(db, {
        targets_json: JSON.stringify([
          { model: "claude-code/claude-opus-5", account_id: grokAcc },
          "codex/gpt-5.2",
        ]),
      })
      const resolved = await resolveModel(envWith(db), "user_1", "opus")
      expect(resolved?.provider).toBe("codex")
    })

    it("all pinned/unpinned targets unavailable: the fallback still dispatches the first resolvable target with any bound account", async () => {
      const db = new FakeD1()
      const env = envWith(db)
      const pinned = seedAccount(db, "claude-code")
      const grokAcc = seedAccount(db, "grok")
      await markBenched(env, "user_1", "claude-code", pinned, 300_000)
      await markBenched(env, "user_1", "grok", grokAcc, 300_000)
      seedGroup(db, {
        targets_json: JSON.stringify([
          { model: "claude-code/claude-opus-5", account_id: pinned },
          "grok/grok-4.5",
        ]),
      })
      const resolved = await resolveModel(env, "user_1", "opus")
      // First resolvable target wins the "any bound account" fallback — the
      // pinned target still counts as "has an account" (just benched).
      expect(resolved?.provider).toBe("claude-code")
      expect(resolved?.pinnedAccountId).toBe(pinned)
    })

    it("a group mixing a pinned claude-code target, an unpinned grok target, and a pinned custom-provider target — order is the routing authority across accounts, not just providers", async () => {
      const db = new FakeD1()
      seedCustomProvider(db, "official-api", "openai")
      const ccAcc1 = seedAccount(db, "claude-code")
      const ccAcc2 = seedAccount(db, "claude-code")
      const customAcc = seedAccount(db, "official-api")
      void ccAcc2
      // Order: pinned claude-code acc #1, then the same provider's acc #2
      // never gets a look-in unless #1 fails, then a pinned custom endpoint.
      seedGroup(db, {
        targets_json: JSON.stringify([
          { model: "claude-code/claude-opus-5", account_id: ccAcc1 },
          { model: "official-api/gpt-4o", account_id: customAcc },
        ]),
      })
      const resolved = await resolveModel(envWith(db), "user_1", "opus")
      expect(resolved?.provider).toBe("claude-code")
      expect(resolved?.pinnedAccountId).toBe(ccAcc1)
    })
  })
})
