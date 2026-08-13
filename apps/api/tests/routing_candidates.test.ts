/**
 * Candidate expansion (docs/providers.md § Routing module "Candidates") —
 * `routing/candidates.ts` is the single owner of turning a resolved request
 * into a flat, ordered `(provider, upstreamModel, account)` list. Replaces
 * the old `resolveModel`/`pickGroupTarget` coverage in
 * tests/resolve_model*.test.ts, which asserted on usability-aware selection
 * that now lives in facts.ts + dispatch's walk instead of at resolve time.
 */
import { describe, expect, it } from "vitest"
import type { Env } from "../src/env"
import { groupCandidates, poolCandidates, resolveCandidates } from "../src/routing/candidates"
import type { ModelGroupRow } from "../src/db/model_groups"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

function envWith(db: FakeD1): Env {
  return { DB: db as unknown as D1Database, BENCH: fakeKV() } as unknown as Env
}

let accSeq = 0
function seedAccount(db: FakeD1, provider: string, userId = "user_1", priority = 1): string {
  const id = `acc_${provider}_${accSeq++}`
  db.seed("upstream_accounts", [
    {
      id,
      user_id: userId,
      provider,
      external_account_id: null,
      label: provider,
      priority,
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

function groupRow(overrides: Partial<ModelGroupRow> = {}): ModelGroupRow {
  return {
    id: "mgrp_1",
    user_id: "user_1",
    name: "group",
    targets_json: JSON.stringify(["claude-code/claude-opus-5"]),
    strategy: "ordered",
    created_at: "2026-01-01T00:00:00.000Z",
    updated_at: "2026-01-01T00:00:00.000Z",
    ...overrides,
  }
}

describe("poolCandidates — direct provider/model call", () => {
  it("one candidate per pool account, in pool-priority order", async () => {
    const db = new FakeD1()
    const high = seedAccount(db, "grok", "user_1", 10)
    const low = seedAccount(db, "grok", "user_1", 1)
    const candidates = await poolCandidates(envWith(db), "user_1", {
      provider: "grok",
      upstreamModel: "grok-4.5",
      isBuiltin: true,
      adapter: { id: "grok", chatCompletions: async () => new Response() },
    })
    expect(candidates.map((c) => c.account.id)).toEqual([high, low])
    expect(candidates.every((c) => !c.pinned)).toBe(true)
    expect(candidates.every((c) => c.targetIndex === 0)).toBe(true)
  })

  it("empty pool → empty candidate list", async () => {
    const db = new FakeD1()
    const candidates = await poolCandidates(envWith(db), "user_1", {
      provider: "grok",
      upstreamModel: "grok-4.5",
      isBuiltin: true,
      adapter: { id: "grok", chatCompletions: async () => new Response() },
    })
    expect(candidates).toEqual([])
  })

  it("accountId pins to exactly that one account", async () => {
    const db = new FakeD1()
    seedAccount(db, "grok", "user_1", 10)
    const low = seedAccount(db, "grok", "user_1", 1)
    const candidates = await poolCandidates(envWith(db), "user_1", {
      provider: "grok",
      upstreamModel: "grok-4.5",
      isBuiltin: true,
      adapter: { id: "grok", chatCompletions: async () => new Response() },
      accountId: low,
    })
    expect(candidates).toHaveLength(1)
    expect(candidates[0]!.account.id).toBe(low)
    expect(candidates[0]!.pinned).toBe(true)
  })
})

describe("groupCandidates — expanding a group's targets", () => {
  it("unpinned target contributes every pool account in priority order", async () => {
    const db = new FakeD1()
    const high = seedAccount(db, "claude-code", "user_1", 10)
    const low = seedAccount(db, "claude-code", "user_1", 1)
    const { candidates, resolvedTargets } = await groupCandidates(
      envWith(db),
      "user_1",
      groupRow({ targets_json: JSON.stringify(["claude-code/claude-opus-5"]) }),
    )
    expect(resolvedTargets).toHaveLength(1)
    expect(candidates.map((c) => c.account.id)).toEqual([high, low])
  })

  it("pinned target contributes exactly its one account", async () => {
    const db = new FakeD1()
    seedAccount(db, "claude-code", "user_1", 10)
    const pinned = seedAccount(db, "claude-code", "user_1", 1)
    const { candidates } = await groupCandidates(
      envWith(db),
      "user_1",
      groupRow({
        targets_json: JSON.stringify([{ model: "claude-code/claude-opus-5", account_id: pinned }]),
      }),
    )
    expect(candidates).toHaveLength(1)
    expect(candidates[0]!.account.id).toBe(pinned)
    expect(candidates[0]!.pinned).toBe(true)
  })

  it("mixed group: pinned target 0 + unpinned target 1, flattened in target-then-pool order", async () => {
    const db = new FakeD1()
    const pinned = seedAccount(db, "claude-code", "user_1", 1)
    const grokHigh = seedAccount(db, "grok", "user_1", 10)
    const grokLow = seedAccount(db, "grok", "user_1", 1)
    const { candidates } = await groupCandidates(
      envWith(db),
      "user_1",
      groupRow({
        targets_json: JSON.stringify([
          { model: "claude-code/claude-opus-5", account_id: pinned },
          "grok/grok-4.5",
        ]),
      }),
    )
    expect(candidates.map((c) => ({ id: c.account.id, targetIndex: c.targetIndex, pinned: c.pinned }))).toEqual([
      { id: pinned, targetIndex: 0, pinned: true },
      { id: grokHigh, targetIndex: 1, pinned: false },
      { id: grokLow, targetIndex: 1, pinned: false },
    ])
  })

  it("a deleted custom-provider target is skipped; the next resolvable target's candidates still appear", async () => {
    const db = new FakeD1()
    // "gone" is never seeded as a custom_providers row — simulates deletion.
    const acc = seedAccount(db, "claude-code")
    const { candidates, resolvedTargets } = await groupCandidates(
      envWith(db),
      "user_1",
      groupRow({ targets_json: JSON.stringify(["gone/some-model", "claude-code/claude-opus-5"]) }),
    )
    expect(resolvedTargets).toHaveLength(1)
    expect(resolvedTargets[0]!.provider).toBe("claude-code")
    expect(candidates.map((c) => c.account.id)).toEqual([acc])
  })

  it("a pinned target whose account was deleted contributes nothing for that target", async () => {
    const db = new FakeD1()
    const grokAcc = seedAccount(db, "grok")
    const { candidates, resolvedTargets } = await groupCandidates(
      envWith(db),
      "user_1",
      groupRow({
        targets_json: JSON.stringify([
          { model: "claude-code/claude-opus-5", account_id: "acc_never_existed" },
          "grok/grok-4.5",
        ]),
      }),
    )
    // Both targets resolve (their provider prefixes are valid) but target 0
    // contributes zero candidates (its pinned account is gone).
    expect(resolvedTargets).toHaveLength(2)
    expect(candidates.map((c) => c.account.id)).toEqual([grokAcc])
  })

  it("a pinned target whose account belongs to a different provider contributes nothing for that target", async () => {
    const db = new FakeD1()
    const grokAcc = seedAccount(db, "grok")
    const codexAcc = seedAccount(db, "codex")
    const { candidates } = await groupCandidates(
      envWith(db),
      "user_1",
      groupRow({
        targets_json: JSON.stringify([
          { model: "claude-code/claude-opus-5", account_id: grokAcc },
          "codex/gpt-5.2",
        ]),
      }),
    )
    expect(candidates.map((c) => c.account.id)).toEqual([codexAcc])
  })

  it("resolves a custom-provider target to its adapter", async () => {
    const db = new FakeD1()
    seedCustomProvider(db, "my-endpoint", "openai")
    const acc = seedAccount(db, "my-endpoint")
    const { candidates } = await groupCandidates(
      envWith(db),
      "user_1",
      groupRow({ targets_json: JSON.stringify(["my-endpoint/gpt-4o"]) }),
    )
    expect(candidates).toHaveLength(1)
    expect(candidates[0]!.account.id).toBe(acc)
    expect(candidates[0]!.isBuiltin).toBe(false)
    expect(candidates[0]!.customProvider?.slug).toBe("my-endpoint")
    expect(typeof candidates[0]!.adapter.chatCompletions).toBe("function")
  })

  it("no target resolves at all: resolvedTargets and candidates are both empty (invalid_model at the route level)", async () => {
    const db = new FakeD1()
    const { candidates, resolvedTargets } = await groupCandidates(
      envWith(db),
      "user_1",
      groupRow({ targets_json: JSON.stringify(["gone-1/model", "gone-2/model"]) }),
    )
    expect(resolvedTargets).toEqual([])
    expect(candidates).toEqual([])
  })
})

describe("resolveCandidates — top-level combinator used by routes", () => {
  it("direct provider/model: primary + candidates from that pool, strategy defaults to ordered", async () => {
    const db = new FakeD1()
    const acc = seedAccount(db, "grok")
    const resolved = await resolveCandidates(envWith(db), "user_1", "grok/grok-4.5")
    expect(resolved).not.toBeNull()
    expect(resolved!.primary).toMatchObject({ provider: "grok", upstreamModel: "grok-4.5", isBuiltin: true })
    expect(resolved!.candidates.map((c) => c.account.id)).toEqual([acc])
    expect(resolved!.strategy).toBe("ordered")
    expect(resolved!.groupName).toBeUndefined()
  })

  it("direct call honors a PATCHed provider_settings strategy value", async () => {
    const db = new FakeD1()
    seedAccount(db, "grok")
    db.seed("provider_settings", [
      { user_id: "user_1", provider: "grok", strategy: "ordered", updated_at: "2026-01-01T00:00:00.000Z" },
    ])
    const resolved = await resolveCandidates(envWith(db), "user_1", "grok/grok-4.5")
    expect(resolved!.strategy).toBe("ordered")
  })

  it("group alias: primary is the first resolved target, candidates span every target", async () => {
    const db = new FakeD1()
    db.seed("model_groups", [groupRow({ targets_json: JSON.stringify(["claude-code/claude-opus-5", "grok/grok-4.5"]) })])
    db.seed("model_group_aliases", [
      { id: "a1", user_id: "user_1", group_id: "mgrp_1", alias: "opus", created_at: "2026-01-01T00:00:00.000Z" },
    ])
    const cc = seedAccount(db, "claude-code")
    const grok = seedAccount(db, "grok")
    const resolved = await resolveCandidates(envWith(db), "user_1", "opus")
    expect(resolved!.primary.provider).toBe("claude-code")
    expect(resolved!.groupName).toBe("opus")
    expect(resolved!.candidates.map((c) => c.account.id)).toEqual([cc, grok])
  })

  it("unknown group alias and unknown provider both miss as null (invalid_model)", async () => {
    const db = new FakeD1()
    expect(await resolveCandidates(envWith(db), "user_1", "no-such-alias")).toBeNull()
    expect(await resolveCandidates(envWith(db), "user_1", "not-a-provider/model")).toBeNull()
  })

  it("primary is target-index-0 regardless of bench state — selection among candidates is dispatch's job, not resolution's", async () => {
    const db = new FakeD1()
    db.seed("model_groups", [groupRow({ targets_json: JSON.stringify(["claude-code/claude-opus-5", "grok/grok-4.5"]) })])
    db.seed("model_group_aliases", [
      { id: "a1", user_id: "user_1", group_id: "mgrp_1", alias: "opus", created_at: "2026-01-01T00:00:00.000Z" },
    ])
    // claude-code has zero bound accounts at all — target 0 still resolves
    // (its prefix is valid) and is still `primary`; only grok has an account.
    const grok = seedAccount(db, "grok")
    const resolved = await resolveCandidates(envWith(db), "user_1", "opus")
    expect(resolved!.primary.provider).toBe("claude-code")
    expect(resolved!.candidates.map((c) => c.account.id)).toEqual([grok])
  })
})
