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

function seedGroup(db: FakeD1, overrides: Partial<{ id: string; user_id: string; name: string; targets_json: string }> = {}) {
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

describe("listModelsForUser — model groups", () => {
  it("lists a group with id = bare name, provider = 'group', regardless of target usability", async () => {
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
