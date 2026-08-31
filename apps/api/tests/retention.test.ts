/**
 * Retention sweep (docs/logging.md "Retention sweep"): request_logs cutoff
 * boundary + batching + the per-run cap, REQUEST_LOG_RETENTION_DAYS parsing,
 * and sessions / oauth_login_states expiry. Exercises the exported pieces
 * directly against FakeD1 — see tests/helpers/fake_d1.ts for the id-subquery
 * DELETE shape this relies on. A real-workerd smoke test (wrangler dev
 * --test-scheduled) covers the actual `[triggers]` + scheduled-handler
 * wiring, which nothing here touches.
 */
import { afterEach, describe, expect, it, vi } from "vitest"
import type { Env } from "../src/env"
import { REQUEST_LOG_BATCH_SIZE, runRetentionSweep, sweepRequestLogs } from "../src/maintenance/retention"
import { FakeD1 } from "./helpers/fake_d1"

function buildEnv(db: FakeD1, retentionDays?: string): Env {
  return {
    DB: db as unknown as D1Database,
    REQUEST_LOG_RETENTION_DAYS: retentionDays,
  } as unknown as Env
}

let logCounter = 0
/** Seeds one request_logs row with the given created_at; returns its id. */
function seedLog(db: FakeD1, createdAt: string): string {
  const id = `log_${logCounter++}`
  db.seed("request_logs", [
    {
      id,
      user_id: "user_1",
      api_key_id: null,
      provider: "claude-code",
      model: "claude-code/claude-opus-5",
      account_id: null,
      status_code: 200,
      latency_ms: 100,
      prompt_tokens: null,
      completion_tokens: null,
      cache_read_input_tokens: null,
      cache_creation_input_tokens: null,
      error_code: null,
      created_at: createdAt,
    },
  ])
  return id
}

afterEach(() => {
  vi.useRealTimers()
})

describe("runRetentionSweep — request_logs cutoff", () => {
  it("boundary: a row exactly at the cutoff is kept (strict <); one just older is deleted; recent rows are kept", async () => {
    const fixedNow = new Date("2026-08-02T12:00:00.000Z")
    vi.useFakeTimers()
    vi.setSystemTime(fixedNow)

    const cutoffMs = fixedNow.getTime() - 90 * 86_400_000 // default retention window (no env override)
    const db = new FakeD1()
    const atCutoffId = seedLog(db, new Date(cutoffMs).toISOString())
    const justOlderId = seedLog(db, new Date(cutoffMs - 1).toISOString())
    const recentId = seedLog(db, new Date(fixedNow.getTime() - 86_400_000).toISOString())

    const result = await runRetentionSweep(buildEnv(db))

    expect(result.requestLogs).toBe(1)
    const remainingIds = db.rows("request_logs").map((r) => r.id)
    expect(remainingIds.sort()).toEqual([atCutoffId, recentId].sort())
    expect(remainingIds).not.toContain(justOlderId)
  })
})

describe("sweepRequestLogs — batching and the per-run cap", () => {
  it("loops across multiple batches until every old row is drained", async () => {
    const db = new FakeD1()
    const cutoff = "2026-01-01T00:00:00.000Z"
    for (let i = 0; i < 7; i++) {
      seedLog(db, new Date(Date.parse(cutoff) - (i + 1) * 1000).toISOString())
    }
    const keptId = seedLog(db, "2026-06-01T00:00:00.000Z") // well after cutoff

    // batchSize=3 over 7 old rows -> batches of 3, 3, 1 (terminates on a
    // non-full batch), well under maxBatches=40 -> never hits the cap.
    const deleted = await sweepRequestLogs(buildEnv(db), cutoff, 3, 40)

    expect(deleted).toBe(7)
    expect(db.rows("request_logs").map((r) => r.id)).toEqual([keptId])
  })

  it("the batch cap bounds a single run even though older rows remain (can't loop forever)", async () => {
    const db = new FakeD1()
    const cutoff = "2026-01-01T00:00:00.000Z"
    for (let i = 0; i < 10; i++) {
      seedLog(db, new Date(Date.parse(cutoff) - (i + 1) * 1000).toISOString())
    }

    // Every batch of 2 comes back full, so the loop would run forever
    // without the cap; maxBatches=3 must stop it at exactly 3*2 deletions.
    const deleted = await sweepRequestLogs(buildEnv(db), cutoff, 2, 3)

    expect(deleted).toBe(6)
    expect(db.rows("request_logs")).toHaveLength(4)
  })

  it("uses the real REQUEST_LOG_BATCH_SIZE default when no override is passed", async () => {
    const db = new FakeD1()
    const cutoff = "2026-01-01T00:00:00.000Z"
    expect(REQUEST_LOG_BATCH_SIZE).toBeGreaterThan(5) // sanity: this seed fits in a single default batch
    for (let i = 0; i < 5; i++) {
      seedLog(db, new Date(Date.parse(cutoff) - (i + 1) * 1000).toISOString())
    }

    const deleted = await sweepRequestLogs(buildEnv(db), cutoff)

    expect(deleted).toBe(5)
    expect(db.rows("request_logs")).toHaveLength(0)
  })
})

describe("REQUEST_LOG_RETENTION_DAYS override", () => {
  it('"30" honors a 30-day cutoff', async () => {
    const db = new FakeD1()
    const now = Date.now()
    // Comfortably on either side of the 30-day cut, clear of clock-skew
    // between this call and the sweep's own Date.now() (usage_routes.test.ts
    // uses the same margin technique for its `from`-window boundary test).
    const justOver30Id = seedLog(db, new Date(now - 31 * 86_400_000).toISOString())
    const under30Id = seedLog(db, new Date(now - 25 * 86_400_000).toISOString())

    const result = await runRetentionSweep(buildEnv(db, "30"))

    expect(result.requestLogs).toBe(1)
    const remainingIds = db.rows("request_logs").map((r) => r.id)
    expect(remainingIds).toEqual([under30Id])
    expect(remainingIds).not.toContain(justOver30Id)
  })

  for (const garbage of ["abc", "0", "-5", ""]) {
    it(`invalid override ${JSON.stringify(garbage)} falls back to the 90-day default`, async () => {
      const db = new FakeD1()
      const now = Date.now()
      const inside90Id = seedLog(db, new Date(now - 35 * 86_400_000).toISOString())
      const outside90Id = seedLog(db, new Date(now - 95 * 86_400_000).toISOString())

      const result = await runRetentionSweep(buildEnv(db, garbage))

      expect(result.requestLogs).toBe(1)
      const remainingIds = db.rows("request_logs").map((r) => r.id)
      expect(remainingIds).toEqual([inside90Id])
      expect(remainingIds).not.toContain(outside90Id)
    })
  }
})

describe("runRetentionSweep — sessions and oauth_login_states", () => {
  it("deletes expired sessions, keeps unexpired ones", async () => {
    const db = new FakeD1()
    const now = Date.now()
    db.seed("sessions", [
      {
        id: "sess_expired",
        user_id: "user_1",
        expires_at: new Date(now - 60_000).toISOString(),
        created_at: "2026-01-01T00:00:00.000Z",
      },
      {
        id: "sess_active",
        user_id: "user_1",
        expires_at: new Date(now + 60_000).toISOString(),
        created_at: "2026-01-01T00:00:00.000Z",
      },
    ])

    const result = await runRetentionSweep(buildEnv(db))

    expect(result.sessions).toBe(1)
    expect(db.rows("sessions").map((r) => r.id)).toEqual(["sess_active"])
  })

  it("deletes expired oauth_login_states, keeps unexpired ones", async () => {
    const db = new FakeD1()
    const now = Date.now()
    db.seed("oauth_login_states", [
      {
        id: "state_expired",
        kind: "provider",
        user_id: "user_1",
        provider: "claude-code",
        payload_json: "{}",
        expires_at: new Date(now - 60_000).toISOString(),
        created_at: "2026-01-01T00:00:00.000Z",
      },
      {
        id: "state_active",
        kind: "provider",
        user_id: "user_1",
        provider: "claude-code",
        payload_json: "{}",
        expires_at: new Date(now + 60_000).toISOString(),
        created_at: "2026-01-01T00:00:00.000Z",
      },
    ])

    const result = await runRetentionSweep(buildEnv(db))

    expect(result.oauthStates).toBe(1)
    expect(db.rows("oauth_login_states").map((r) => r.id)).toEqual(["state_active"])
  })

  it("purges expired cli_login_requests and keeps live ones", async () => {
    const db = new FakeD1()
    const now = Date.now()
    db.seed("cli_login_requests", [
      {
        id: "clireq_expired",
        device_name: "old-box",
        code_hash: null,
        user_id: null,
        expires_at: new Date(now - 1000).toISOString(),
        approved_at: null,
        used_at: null,
        attempts: 0,
        created_at: "2026-01-01T00:00:00.000Z",
      },
      {
        id: "clireq_active",
        device_name: "new-box",
        code_hash: null,
        user_id: null,
        expires_at: new Date(now + 60_000).toISOString(),
        approved_at: null,
        used_at: null,
        attempts: 0,
        created_at: "2026-01-01T00:00:00.000Z",
      },
    ])

    const result = await runRetentionSweep(buildEnv(db))

    expect(result.cliLoginRequests).toBe(1)
    expect(db.rows("cli_login_requests").map((r) => r.id)).toEqual(["clireq_active"])
  })

  it("returns all four counts together", async () => {
    const db = new FakeD1()
    const now = Date.now()
    seedLog(db, new Date(now - 91 * 86_400_000).toISOString())
    db.seed("sessions", [
      { id: "s1", user_id: "user_1", expires_at: new Date(now - 1000).toISOString(), created_at: "2026-01-01T00:00:00.000Z" },
    ])
    db.seed("oauth_login_states", [
      {
        id: "o1",
        kind: "google",
        user_id: null,
        provider: null,
        payload_json: "{}",
        expires_at: new Date(now - 1000).toISOString(),
        created_at: "2026-01-01T00:00:00.000Z",
      },
    ])

    const result = await runRetentionSweep(buildEnv(db))

    expect(result).toEqual({ requestLogs: 1, sessions: 1, oauthStates: 1, cliLoginRequests: 0 })
  })
})
