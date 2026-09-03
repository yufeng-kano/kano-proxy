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
import {
  groupModelCandidates,
  poolCandidates,
  resolveCandidates,
  resolveGroupModelCandidates,
} from "../src/routing/candidates"
import type { ModelGroupModelRow, ModelGroupRow } from "../src/db/model_groups"
import { getAdapter } from "../src/providers"
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
    slug: "my-group",
    strategy: "ordered",
    created_at: "2026-01-01T00:00:00.000Z",
    updated_at: "2026-01-01T00:00:00.000Z",
    ...overrides,
  }
}

function modelRow(targets: unknown[], overrides: Partial<ModelGroupModelRow> = {}): ModelGroupModelRow {
  return {
    id: "mgmodel_1",
    user_id: "user_1",
    group_id: "mgrp_1",
    name: "opus",
    targets_json: JSON.stringify(targets),
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

describe("groupModelCandidates — expanding one group model's targets", () => {
  it("unpinned target contributes every pool account in priority order", async () => {
    const db = new FakeD1()
    const high = seedAccount(db, "claude-code", "user_1", 10)
    const low = seedAccount(db, "claude-code", "user_1", 1)
    const { candidates, resolvedTargets } = await groupModelCandidates(
      envWith(db),
      "user_1",
      modelRow(["claude-code/claude-opus-5"]),
    )
    expect(resolvedTargets).toHaveLength(1)
    expect(candidates.map((c) => c.account.id)).toEqual([high, low])
  })

  it("pinned target contributes exactly its one account", async () => {
    const db = new FakeD1()
    seedAccount(db, "claude-code", "user_1", 10)
    const pinned = seedAccount(db, "claude-code", "user_1", 1)
    const { candidates } = await groupModelCandidates(
      envWith(db),
      "user_1",
      modelRow([{ model: "claude-code/claude-opus-5", account_id: pinned }]),
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
    const { candidates } = await groupModelCandidates(
      envWith(db),
      "user_1",
      modelRow([{ model: "claude-code/claude-opus-5", account_id: pinned }, "grok/grok-4.5"]),
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
    const { candidates, resolvedTargets } = await groupModelCandidates(
      envWith(db),
      "user_1",
      modelRow(["gone/some-model", "claude-code/claude-opus-5"]),
    )
    expect(resolvedTargets).toHaveLength(1)
    expect(resolvedTargets[0]!.provider).toBe("claude-code")
    expect(candidates.map((c) => c.account.id)).toEqual([acc])
  })

  it("a pinned target whose account was deleted contributes nothing for that target", async () => {
    const db = new FakeD1()
    const grokAcc = seedAccount(db, "grok")
    const { candidates, resolvedTargets } = await groupModelCandidates(
      envWith(db),
      "user_1",
      modelRow([
        { model: "claude-code/claude-opus-5", account_id: "acc_never_existed" },
        "grok/grok-4.5",
      ]),
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
    const { candidates } = await groupModelCandidates(
      envWith(db),
      "user_1",
      modelRow([{ model: "claude-code/claude-opus-5", account_id: grokAcc }, "codex/gpt-5.2"]),
    )
    expect(candidates.map((c) => c.account.id)).toEqual([codexAcc])
  })

  it("resolves a custom-provider target to its adapter", async () => {
    const db = new FakeD1()
    seedCustomProvider(db, "my-endpoint", "openai")
    const acc = seedAccount(db, "my-endpoint")
    const { candidates } = await groupModelCandidates(
      envWith(db),
      "user_1",
      modelRow(["my-endpoint/gpt-4o"]),
    )
    expect(candidates).toHaveLength(1)
    expect(candidates[0]!.account.id).toBe(acc)
    expect(candidates[0]!.isBuiltin).toBe(false)
    expect(candidates[0]!.customProvider?.slug).toBe("my-endpoint")
    expect(typeof candidates[0]!.adapter.chatCompletions).toBe("function")
  })

  it("no target resolves at all: resolvedTargets and candidates are both empty (invalid_model at the route level)", async () => {
    const db = new FakeD1()
    const { candidates, resolvedTargets } = await groupModelCandidates(
      envWith(db),
      "user_1",
      modelRow(["gone-1/model", "gone-2/model"]),
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

  it("group model: primary is the first resolved target, candidates span every target, groupName is slug/name", async () => {
    const db = new FakeD1()
    const group = groupRow()
    db.seed("model_groups", [group as unknown as Record<string, unknown>])
    db.seed("model_group_models", [
      modelRow(["claude-code/claude-opus-5", "grok/grok-4.5"]) as unknown as Record<string, unknown>,
    ])
    const cc = seedAccount(db, "claude-code")
    const grok = seedAccount(db, "grok")
    const resolved = await resolveGroupModelCandidates(envWith(db), "user_1", group, "opus")
    expect(resolved!.primary.provider).toBe("claude-code")
    expect(resolved!.groupName).toBe("my-group/opus")
    expect(resolved!.candidates.map((c) => c.account.id)).toEqual([cc, grok])
  })

  it("bare names no longer resolve on the shared bases; unknown provider misses too (invalid_model)", async () => {
    const db = new FakeD1()
    db.seed("model_groups", [groupRow() as unknown as Record<string, unknown>])
    db.seed("model_group_models", [
      modelRow(["claude-code/claude-opus-5"]) as unknown as Record<string, unknown>,
    ])
    seedAccount(db, "claude-code")
    // Even a name a group defines is a miss on the shared resolution path.
    expect(await resolveCandidates(envWith(db), "user_1", "opus")).toBeNull()
    expect(await resolveCandidates(envWith(db), "user_1", "not-a-provider/model")).toBeNull()
  })

  it("a name the group does not define misses as null (invalid_model)", async () => {
    const db = new FakeD1()
    const group = groupRow()
    db.seed("model_groups", [group as unknown as Record<string, unknown>])
    db.seed("model_group_models", [
      modelRow(["claude-code/claude-opus-5"]) as unknown as Record<string, unknown>,
    ])
    seedAccount(db, "claude-code")
    expect(await resolveGroupModelCandidates(envWith(db), "user_1", group, "other")).toBeNull()
  })

  it("primary is target-index-0 regardless of bench state — selection among candidates is dispatch's job, not resolution's", async () => {
    const db = new FakeD1()
    const group = groupRow()
    db.seed("model_groups", [group as unknown as Record<string, unknown>])
    db.seed("model_group_models", [
      modelRow(["claude-code/claude-opus-5", "grok/grok-4.5"]) as unknown as Record<string, unknown>,
    ])
    // claude-code has zero bound accounts at all — target 0 still resolves
    // (its prefix is valid) and is still `primary`; only grok has an account.
    const grok = seedAccount(db, "grok")
    const resolved = await resolveGroupModelCandidates(envWith(db), "user_1", group, "opus")
    expect(resolved!.primary.provider).toBe("claude-code")
    expect(resolved!.candidates.map((c) => c.account.id)).toEqual([grok])
  })
})

describe("model eligibility — Claude Code Fable seat rule (docs/providers.md § Routing module \"Candidates\")", () => {
  function seedClaude(
    db: FakeD1,
    id: string,
    priority: number,
    meta: Record<string, unknown> | null,
    snapshotAccount?: Record<string, unknown>,
  ): string {
    db.seed("upstream_accounts", [
      {
        id,
        user_id: "user_1",
        provider: "claude-code",
        external_account_id: null,
        label: id,
        priority,
        encrypted_payload: "encrypted",
        account_meta_json: meta ? JSON.stringify(meta) : null,
        usage_snapshot_json: snapshotAccount
          ? JSON.stringify({ windows: [], account: snapshotAccount, error: null, stale: false })
          : null,
        usage_fetched_at: null,
        usage_fetching_at: null,
        created_at: "2026-01-01T00:00:00.000Z",
        updated_at: "2026-01-01T00:00:00.000Z",
      },
    ])
    return id
  }
  const TEAM_STANDARD = { plan_type: "claude_team", rate_limit_tier: "default_raven" }
  const TEAM_PREMIUM = { plan_type: "claude_team", rate_limit_tier: "default_claude_max_5x" }
  const MAX = { plan_type: "claude_max", rate_limit_tier: "default_claude_max_5x" }

  it("a standard Team seat contributes nothing for a claude-fable model — the Max account below it is the only candidate", async () => {
    const db = new FakeD1()
    seedClaude(db, "acc_team", 15, TEAM_STANDARD)
    seedClaude(db, "acc_max", 14, MAX)
    const fable = await resolveCandidates(envWith(db), "user_1", "claude-code/claude-fable-5-1")
    expect(fable!.candidates.map((c) => c.account.id)).toEqual(["acc_max"])
  })

  it("the same pool serves a non-Fable model in full priority order", async () => {
    const db = new FakeD1()
    seedClaude(db, "acc_team", 15, TEAM_STANDARD)
    seedClaude(db, "acc_max", 14, MAX)
    const sonnet = await resolveCandidates(envWith(db), "user_1", "claude-code/claude-sonnet-5")
    expect(sonnet!.candidates.map((c) => c.account.id)).toEqual(["acc_team", "acc_max"])
  })

  it("a Team Premium seat stays a Fable candidate", async () => {
    const db = new FakeD1()
    seedClaude(db, "acc_premium", 15, TEAM_PREMIUM)
    const fable = await resolveCandidates(envWith(db), "user_1", "claude-code/claude-fable-5-1")
    expect(fable!.candidates.map((c) => c.account.id)).toEqual(["acc_premium"])
  })

  it("fails open: an account with no stored profile is a Fable candidate", async () => {
    const db = new FakeD1()
    seedClaude(db, "acc_fresh", 15, null)
    const fable = await resolveCandidates(envWith(db), "user_1", "claude-code/claude-fable-5-1")
    expect(fable!.candidates.map((c) => c.account.id)).toEqual(["acc_fresh"])
  })

  it("the usage snapshot's account facts win over account_meta_json (a seat upgraded since login)", async () => {
    const db = new FakeD1()
    seedClaude(db, "acc_upgraded", 15, TEAM_STANDARD, TEAM_PREMIUM)
    const fable = await resolveCandidates(envWith(db), "user_1", "claude-code/claude-fable-5-1")
    expect(fable!.candidates.map((c) => c.account.id)).toEqual(["acc_upgraded"])
  })

  it("every account ineligible ⇒ an empty candidate list (dispatch's no_upstream_account), never a bench", async () => {
    const db = new FakeD1()
    seedClaude(db, "acc_team", 15, TEAM_STANDARD)
    const fable = await resolveCandidates(envWith(db), "user_1", "claude-code/claude-fable-5-1")
    expect(fable).not.toBeNull()
    expect(fable!.candidates).toEqual([])
  })

  it("a group target pinned to an ineligible account contributes nothing for Fable, and the next target carries on", async () => {
    const db = new FakeD1()
    seedClaude(db, "acc_team", 15, TEAM_STANDARD)
    seedClaude(db, "acc_max", 14, MAX)
    const pinnedFable = await poolCandidates(envWith(db), "user_1", {
      provider: "claude-code",
      upstreamModel: "claude-fable-5-1",
      isBuiltin: true,
      adapter: getAdapter("claude-code"),
      accountId: "acc_team",
    })
    expect(pinnedFable).toEqual([])
    const pinnedSonnet = await poolCandidates(envWith(db), "user_1", {
      provider: "claude-code",
      upstreamModel: "claude-sonnet-5",
      isBuiltin: true,
      adapter: getAdapter("claude-code"),
      accountId: "acc_team",
    })
    expect(pinnedSonnet.map((c) => c.account.id)).toEqual(["acc_team"])
  })

  it("providers without a rule are untouched — every grok account is a candidate for any model", async () => {
    const db = new FakeD1()
    const acc = seedAccount(db, "grok")
    const resolved = await resolveCandidates(envWith(db), "user_1", "grok/claude-fable-5-1")
    expect(resolved!.candidates.map((c) => c.account.id)).toEqual([acc])
  })
})
