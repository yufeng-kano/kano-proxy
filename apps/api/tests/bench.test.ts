import { describe, expect, it } from "vitest"
import { benchedUntil, clearBench, earliestBenchExpiry, isBenched, markBenched } from "../src/pool/bench"
import type { Env } from "../src/env"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

function buildEnv(db: FakeD1): Env {
  return { DB: db as unknown as D1Database, BENCH: fakeKV() } as Env
}

function seedAccount(db: FakeD1, id: string, benchUntil: string | null = null): void {
  db.seed("upstream_accounts", [
    { id, user_id: "user_1", provider: "grok", bench_until: benchUntil, bench_reason: null },
  ])
}

describe("D1 bench state", () => {
  it("returns null for a never-benched or expired account without deleting the expired value", async () => {
    const db = new FakeD1()
    seedAccount(db, "fresh")
    seedAccount(db, "expired", new Date(Date.now() - 1_000).toISOString())
    const env = buildEnv(db)
    expect(await benchedUntil(env, "user_1", "grok", "fresh")).toBeNull()
    expect(await benchedUntil(env, "user_1", "grok", "expired")).toBeNull()
    expect(db.rows("upstream_accounts").find((row) => row.id === "expired")?.bench_until).not.toBeNull()
  })

  it("writes monotonically so a short concurrent penalty cannot shorten a longer bench", async () => {
    const db = new FakeD1()
    const longUntil = new Date(Date.now() + 7 * 24 * 60 * 60 * 1000).toISOString()
    seedAccount(db, "acc_1", longUntil)
    const env = buildEnv(db)
    await markBenched(env, "user_1", "grok", "acc_1", 30_000, "429")
    const row = db.rows("upstream_accounts")[0]!
    expect(row.bench_until).toBe(longUntil)
    expect(row.bench_reason).toBeNull()
  })

  it("clears both bench fields idempotently", async () => {
    const db = new FakeD1()
    seedAccount(db, "acc_1", new Date(Date.now() + 60_000).toISOString())
    db.rows("upstream_accounts")[0]!.bench_reason = "429"
    const env = buildEnv(db)
    await clearBench(env, "user_1", "grok", "acc_1")
    await clearBench(env, "user_1", "grok", "acc_1")
    expect(db.rows("upstream_accounts")[0]).toMatchObject({ bench_until: null, bench_reason: null })
    expect(await isBenched(env, "user_1", "grok", "acc_1")).toBe(false)
  })

  it("returns the earliest current expiry", async () => {
    const db = new FakeD1()
    const now = Date.now()
    seedAccount(db, "acc_1", new Date(now + 120_000).toISOString())
    seedAccount(db, "acc_2", new Date(now + 30_000).toISOString())
    seedAccount(db, "acc_3", new Date(now + 90_000).toISOString())
    expect(await earliestBenchExpiry(buildEnv(db), "user_1", "grok", ["acc_1", "acc_2", "acc_3"])).toBe(now + 30_000)
  })
})
