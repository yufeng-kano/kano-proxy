/**
 * Since v4 the shared catalog carries no `group` section at all — each group
 * is its own endpoint whose `/models` lists its names (covered in
 * tests/request_logging_groups.test.ts). This file pins the removal.
 */
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

function seedGroup(db: FakeD1): void {
  db.seed("model_groups", [
    {
      id: "mgrp_1",
      user_id: "user_1",
      name: "opus",
      slug: "team",
      strategy: "ordered",
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
  db.seed("model_group_models", [
    {
      id: "mgmodel_1",
      user_id: "user_1",
      group_id: "mgrp_1",
      name: "opus",
      targets_json: JSON.stringify(["claude-code/claude-opus-5"]),
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
}

describe("listModelsForUser — model groups are absent from the shared catalog", () => {
  it("lists no group section and no group model entries even when the user has groups", async () => {
    const db = new FakeD1()
    seedGroup(db)
    const { models, providers } = await listModelsForUser(buildEnv(db), "user_1")
    expect(models.find((m) => m.id === "opus")).toBeUndefined()
    expect(models.filter((m) => m.provider === "group")).toHaveLength(0)
    expect(providers.find((p) => p.provider === "group")).toBeUndefined()
  })

  it("never expands a group into its targets either", async () => {
    const db = new FakeD1()
    seedGroup(db)
    const { models } = await listModelsForUser(buildEnv(db), "user_1")
    expect(models.some((m) => m.id === "claude-code/claude-opus-5")).toBe(false)
  })
})
