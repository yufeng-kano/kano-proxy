import { describe, expect, it } from "vitest"
import { listModelsForUser } from "../src/catalog/models"
import type { Env } from "../src/env"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

function buildEnv(db: FakeD1): Env {
  return {
    DB: db as unknown as D1Database,
    CACHE: fakeKV(),
    BENCH: fakeKV(),
  } as unknown as Env
}

/** Seeds a group row plus its `model_group_aliases` rows — the catalog lists one entry per alias, not per group. */
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

describe("listModelsForUser — model groups", () => {
  it("lists a group with id = alias, provider = 'group', regardless of target usability", async () => {
    const db = new FakeD1()
    seedGroup(db)
    const { models, providers } = await listModelsForUser(buildEnv(db), "user_1")
    const group = models.find((m) => m.id === "opus")
    expect(group).toMatchObject({
      id: "opus",
      provider: "group",
      upstream: "opus",
      display_name: "opus",
      available: true,
      owned_by: "group",
    })
    expect(providers.find((p) => p.provider === "group")?.models).toHaveLength(1)
  })

  it("lists one catalog entry per alias, all sharing the group's display name", async () => {
    const db = new FakeD1()
    seedGroup(db, { name: "OpenAI GPT-4o family", aliases: ["gpt-4o", "gpt-4", "gpt-4-turbo"] })
    const { models, providers } = await listModelsForUser(buildEnv(db), "user_1")
    const groupModels = models.filter((m) => m.provider === "group")
    expect(groupModels).toHaveLength(3)
    expect(groupModels.map((m) => m.id).sort()).toEqual(["gpt-4", "gpt-4-turbo", "gpt-4o"])
    for (const m of groupModels) {
      expect(m.display_name).toBe("OpenAI GPT-4o family")
      // upstream mirrors id (the alias), never the shared display name.
      expect(m.upstream).toBe(m.id)
    }
    expect(providers.find((p) => p.provider === "group")?.models).toHaveLength(3)
  })

  it("two groups list their own aliases independently, each with its own display_name", async () => {
    const db = new FakeD1()
    seedGroup(db, { id: "mgrp_a", name: "Group A", aliases: ["alias-a1", "alias-a2"] })
    seedGroup(db, {
      id: "mgrp_b",
      name: "Group B",
      aliases: ["alias-b"],
      targets_json: JSON.stringify(["grok/grok-4.5"]),
    })
    const { models } = await listModelsForUser(buildEnv(db), "user_1")
    const groupModels = models.filter((m) => m.provider === "group")
    expect(groupModels).toHaveLength(3)
    expect(groupModels.find((m) => m.id === "alias-a1")?.display_name).toBe("Group A")
    expect(groupModels.find((m) => m.id === "alias-a2")?.display_name).toBe("Group A")
    expect(groupModels.find((m) => m.id === "alias-b")?.display_name).toBe("Group B")
  })

  it("never expands a group into its targets in the catalog", async () => {
    const db = new FakeD1()
    seedGroup(db, { targets_json: JSON.stringify(["claude-code/claude-opus-5", "grok/grok-4.5"]) })
    const { models } = await listModelsForUser(buildEnv(db), "user_1")
    expect(models.filter((m) => m.provider === "group")).toHaveLength(1)
    expect(models.some((m) => m.id === "claude-code/claude-opus-5")).toBe(false)
  })

  it("never resolves another user's group", async () => {
    const db = new FakeD1()
    seedGroup(db, { user_id: "user_1" })
    const { models } = await listModelsForUser(buildEnv(db), "user_2")
    expect(models.find((m) => m.id === "opus")).toBeUndefined()
  })

  it("empty when the user has no groups", async () => {
    const db = new FakeD1()
    const { models, providers } = await listModelsForUser(buildEnv(db), "user_1")
    expect(models.filter((m) => m.provider === "group")).toHaveLength(0)
    expect(providers.find((p) => p.provider === "group")?.models).toEqual([])
  })
})
