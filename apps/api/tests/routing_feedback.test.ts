/**
 * Penalties (docs/providers.md § Routing module "Penalties") — what a
 * failed upstream response costs the account.
 */
import { describe, expect, it } from "vitest"
import type { AccountRow } from "../src/db/accounts"
import { isEdgeTimeoutStatus, penaltyForOutcome, isBenchStatus } from "../src/routing/feedback"

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

const NOW = Date.parse("2026-06-01T00:00:00.000Z")

describe("isBenchStatus / penaltyForOutcome — status classification", () => {
  it.each([401, 402, 403, 429])("benches on %d", (status) => {
    expect(isBenchStatus(status)).toBe(true)
    expect(penaltyForOutcome(status, new Headers(), accountRow(), NOW)).not.toBeNull()
  })

  it.each([520, 522, 524])("records a request-local edge-timeout strike on %d", (status) => {
    expect(isEdgeTimeoutStatus(status)).toBe(true)
    expect(isBenchStatus(status)).toBe(false)
    expect(penaltyForOutcome(status, new Headers(), accountRow(), NOW)).toBeNull()
  })

  it.each([200, 400, 404, 408, 409, 422, 500, 502, 503])("does not bench on %d", (status) => {
    expect(isBenchStatus(status)).toBe(false)
    expect(isEdgeTimeoutStatus(status)).toBe(false)
    expect(penaltyForOutcome(status, new Headers(), accountRow(), NOW)).toBeNull()
  })

  it("401/403/402 bench for a flat 300s", () => {
    for (const status of [401, 402, 403]) {
      expect(penaltyForOutcome(status, new Headers(), accountRow(), NOW)).toEqual({ cooldownMs: 300_000 })
    }
  })

})

describe("429 — reset-aware cooldown", () => {
  it("header-derived: uses the anthropic-ratelimit-*-reset header when present", () => {
    const headers = new Headers({ "anthropic-ratelimit-requests-reset": "2026-06-01T00:10:00.000Z" })
    const penalty = penaltyForOutcome(429, headers, accountRow(), NOW)
    expect(penalty).toEqual({ cooldownMs: 600_000 })
  })

  it("header-derived: takes the LATEST of multiple rate-limit reset headers", () => {
    const headers = new Headers({
      "anthropic-ratelimit-requests-reset": "2026-06-01T00:05:00.000Z",
      "anthropic-ratelimit-input-tokens-reset": "2026-06-01T00:20:00.000Z",
    })
    const penalty = penaltyForOutcome(429, headers, accountRow(), NOW)
    expect(penalty).toEqual({ cooldownMs: 1_200_000 })
  })

  it("snapshot-derived: falls back to the earliest >=100% window's resets_at when no header is present", () => {
    const row = accountRow({
      usage_snapshot_json: JSON.stringify({
        windows: [
          { label: "5h", utilization: 100, resets_at: "2026-06-01T02:00:00.000Z" },
          { label: "Week", utilization: 100, resets_at: "2026-06-03T00:00:00.000Z" },
        ],
        error: null,
        stale: false,
        edgeBlocked: false,
      }),
    })
    const penalty = penaltyForOutcome(429, new Headers(), row, NOW)
    // Earliest of the two exhausted windows, not the latest (feedback.ts
    // differs from facts.ts's "latest" combination rule on purpose — see
    // docs/providers.md § Routing module "Penalties").
    expect(penalty).toEqual({ cooldownMs: Date.parse("2026-06-01T02:00:00.000Z") - NOW })
  })

  it("fallback: 300s when neither a header nor an exhausted window is available", () => {
    const penalty = penaltyForOutcome(429, new Headers(), accountRow(), NOW)
    expect(penalty).toEqual({ cooldownMs: 300_000 })
  })

  it("a header wins over the snapshot even when both are present", () => {
    const headers = new Headers({ "anthropic-ratelimit-requests-reset": "2026-06-01T00:01:00.000Z" })
    const row = accountRow({
      usage_snapshot_json: JSON.stringify({
        windows: [{ label: "5h", utilization: 100, resets_at: "2026-06-02T00:00:00.000Z" }],
        error: null,
        stale: false,
        edgeBlocked: false,
      }),
    })
    const penalty = penaltyForOutcome(429, headers, row, NOW)
    expect(penalty).toEqual({ cooldownMs: 60_000 })
  })

  it("caps at 7 days even when the derived reset is much further out", () => {
    const headers = new Headers({ "anthropic-ratelimit-requests-reset": "2026-08-01T00:00:00.000Z" })
    const penalty = penaltyForOutcome(429, headers, accountRow(), NOW)
    expect(penalty).toEqual({ cooldownMs: 7 * 24 * 60 * 60 * 1000 })
  })

  it("a malformed reset header is ignored, falling through to the next source", () => {
    const headers = new Headers({ "anthropic-ratelimit-requests-reset": "not-a-date" })
    const penalty = penaltyForOutcome(429, headers, accountRow(), NOW)
    expect(penalty).toEqual({ cooldownMs: 300_000 })
  })

  it("a stale (already-passed) exhausted window in the snapshot is not used — falls back to 300s", () => {
    const row = accountRow({
      usage_snapshot_json: JSON.stringify({
        windows: [{ label: "5h", utilization: 100, resets_at: "2026-05-31T00:00:00.000Z" }],
        error: null,
        stale: false,
        edgeBlocked: false,
      }),
    })
    const penalty = penaltyForOutcome(429, new Headers(), row, NOW)
    expect(penalty).toEqual({ cooldownMs: 300_000 })
  })
})

describe("any other non-2xx", () => {
  it("no bench for statuses outside the penalty table (e.g. 500, 400)", () => {
    expect(penaltyForOutcome(500, new Headers(), accountRow(), NOW)).toBeNull()
    expect(penaltyForOutcome(400, new Headers(), accountRow(), NOW)).toBeNull()
  })
})
