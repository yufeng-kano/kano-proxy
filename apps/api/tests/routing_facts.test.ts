/**
 * Per-candidate usability facts (docs/providers.md § Routing module
 * "Facts") — computed from stored state only (KV bench + the account row's
 * `usage_snapshot_json`), never a live upstream call.
 */
import { describe, expect, it, vi, afterEach } from "vitest"
import type { Env } from "../src/env"
import { benchKey, markBenched } from "../src/pool/bench"
import { candidateFacts, usageWindowUnusableUntil } from "../src/routing/facts"
import type { AccountRow } from "../src/db/accounts"
import type { RoutingCandidate } from "../src/routing/types"
import { fakeKV } from "./helpers/fake_d1"

afterEach(() => {
  vi.useRealTimers()
})

function accountRow(overrides: Partial<AccountRow> = {}): AccountRow {
  return {
    id: "acc_1",
    user_id: "user_1",
    provider: "claude-code",
    external_account_id: null,
    label: null,
    custom_label: null,
    priority: 1,
    encrypted_payload: "encrypted",
    account_meta_json: null,
    usage_snapshot_json: null,
    usage_fetched_at: null,
    usage_fetching_at: null,
    created_at: "2026-01-01T00:00:00.000Z",
    updated_at: "2026-01-01T00:00:00.000Z",
    ...overrides,
  }
}

function snapshotJson(windows: Array<{ label: string; utilization: number | null; resets_at: string | null }>): string {
  return JSON.stringify({ windows, error: null, stale: false, edgeBlocked: false })
}

describe("usageWindowUnusableUntil", () => {
  it("a window at exactly 100% with a future resets_at marks unusable until that time", () => {
    const row = accountRow({
      usage_snapshot_json: snapshotJson([
        { label: "5h", utilization: 100, resets_at: "2026-06-01T05:00:00.000Z" },
      ]),
    })
    const now = Date.parse("2026-06-01T00:00:00.000Z")
    expect(usageWindowUnusableUntil(row, now)).toBe(Date.parse("2026-06-01T05:00:00.000Z"))
  })

  it("a window under 100% is not exhausted", () => {
    const row = accountRow({
      usage_snapshot_json: snapshotJson([
        { label: "5h", utilization: 99.9, resets_at: "2026-06-01T05:00:00.000Z" },
      ]),
    })
    expect(usageWindowUnusableUntil(row, Date.parse("2026-06-01T00:00:00.000Z"))).toBeNull()
  })

  it("self-expires: a >=100% window whose resets_at is already in the past is not exhausted, even off a stale snapshot", () => {
    const row = accountRow({
      usage_snapshot_json: snapshotJson([
        { label: "5h", utilization: 100, resets_at: "2026-06-01T00:00:00.000Z" },
      ]),
    })
    // "now" is after the window's own resets_at — a stale snapshot can never
    // bench an account past its real reset (docs/providers.md § Routing module).
    const now = Date.parse("2026-06-01T05:00:00.000Z")
    expect(usageWindowUnusableUntil(row, now)).toBeNull()
  })

  it("malformed/missing snapshot reads as no window fact (fail open)", () => {
    expect(usageWindowUnusableUntil(accountRow({ usage_snapshot_json: null }))).toBeNull()
    expect(usageWindowUnusableUntil(accountRow({ usage_snapshot_json: "not json" }))).toBeNull()
    expect(usageWindowUnusableUntil(accountRow({ usage_snapshot_json: JSON.stringify({ notWindows: [] }) }))).toBeNull()
  })

  it("multiple exhausted windows: unusable until the LATEST of their resets_at — clearing one doesn't clear the others", () => {
    const row = accountRow({
      usage_snapshot_json: snapshotJson([
        { label: "5h", utilization: 100, resets_at: "2026-06-01T01:00:00.000Z" },
        { label: "Week", utilization: 100, resets_at: "2026-06-05T00:00:00.000Z" },
      ]),
    })
    const now = Date.parse("2026-06-01T00:00:00.000Z")
    expect(usageWindowUnusableUntil(row, now)).toBe(Date.parse("2026-06-05T00:00:00.000Z"))
  })

  it("a window missing resets_at is ignored even at 100%", () => {
    const row = accountRow({
      usage_snapshot_json: snapshotJson([{ label: "5h", utilization: 100, resets_at: null }]),
    })
    expect(usageWindowUnusableUntil(row, Date.now())).toBeNull()
  })
})

function envWith(bench: ReturnType<typeof fakeKV>): Env {
  return { BENCH: bench } as unknown as Env
}

function candidateFor(account: AccountRow): RoutingCandidate {
  return {
    targetIndex: 0,
    pinned: false,
    provider: account.provider,
    upstreamModel: "claude-opus-5",
    isBuiltin: true,
    adapter: { id: account.provider, chatCompletions: async () => new Response() },
    account,
  }
}

describe("candidateFacts", () => {
  it("neither benched nor limited: usable, unusableUntil null", async () => {
    const env = envWith(fakeKV())
    const facts = await candidateFacts(env, "user_1", candidateFor(accountRow()))
    expect(facts).toEqual({ usable: true, unusableUntil: null, benchUntil: null, usageWindowUntil: null })
  })

  it("benched only: unusable until the bench expiry", async () => {
    const env = envWith(fakeKV())
    vi.useFakeTimers()
    vi.setSystemTime(new Date("2026-01-01T00:00:00.000Z"))
    await markBenched(env, "user_1", "claude-code", "acc_1", 300_000)
    const facts = await candidateFacts(env, "user_1", candidateFor(accountRow()))
    expect(facts.usable).toBe(false)
    expect(facts.unusableUntil).toBe(Date.now() + 300_000)
  })

  it("limited only: unusable until the window resets", async () => {
    const env = envWith(fakeKV())
    const now = Date.parse("2026-06-01T00:00:00.000Z")
    const row = accountRow({
      usage_snapshot_json: snapshotJson([
        { label: "5h", utilization: 100, resets_at: "2026-06-01T05:00:00.000Z" },
      ]),
    })
    const facts = await candidateFacts(env, "user_1", candidateFor(row), now)
    expect(facts).toEqual({
      usable: false,
      unusableUntil: Date.parse("2026-06-01T05:00:00.000Z"),
      benchUntil: null,
      usageWindowUntil: Date.parse("2026-06-01T05:00:00.000Z"),
    })
  })

  it("both benched and limited: unusable until whichever is later", async () => {
    const env = envWith(fakeKV())
    const now = Date.parse("2026-06-01T00:00:00.000Z")
    // Bench clears sooner (5 min) than the usage window (5h).
    await env.BENCH.put(benchKey("user_1", "claude-code", "acc_1"), String(now + 300_000))
    const row = accountRow({
      usage_snapshot_json: snapshotJson([
        { label: "5h", utilization: 100, resets_at: "2026-06-01T05:00:00.000Z" },
      ]),
    })
    const facts = await candidateFacts(env, "user_1", candidateFor(row), now)
    expect(facts.usable).toBe(false)
    expect(facts.unusableUntil).toBe(Date.parse("2026-06-01T05:00:00.000Z"))
  })
})
