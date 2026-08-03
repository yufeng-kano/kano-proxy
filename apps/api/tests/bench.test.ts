import { describe, expect, it } from "vitest"
import { benchKey, benchedUntil, earliestBenchExpiry, isBenched, markBenched } from "../src/pool/bench"
import type { Env } from "../src/env"
import { fakeKV } from "./helpers/fake_d1"

describe("benchKey", () => {
  it("formats stable KV key", () => {
    expect(benchKey("user_1", "claude-code", "acct_9")).toBe(
      "bench:user_1:claude-code:acct_9",
    )
  })

  it("includes provider and account segments", () => {
    const k = benchKey("u", "grok", "a")
    expect(k.startsWith("bench:")).toBe(true)
    expect(k.split(":")).toEqual(["bench", "u", "grok", "a"])
  })
})

function buildEnv(): Env {
  return { BENCH: fakeKV() } as unknown as Env
}

describe("benchedUntil", () => {
  it("returns null when the account was never benched", async () => {
    const env = buildEnv()
    expect(await benchedUntil(env, "user_1", "grok", "acc_1")).toBeNull()
  })

  it("returns the stored epoch-ms while the bench is still active", async () => {
    const env = buildEnv()
    const until = Date.now() + 60_000
    await env.BENCH.put(benchKey("user_1", "grok", "acc_1"), String(until))
    expect(await benchedUntil(env, "user_1", "grok", "acc_1")).toBe(until)
  })

  it("returns null and deletes an expired key (same cleanup isBenched used to do inline)", async () => {
    const env = buildEnv()
    const key = benchKey("user_1", "grok", "acc_1")
    await env.BENCH.put(key, String(Date.now() - 1_000))
    expect(await benchedUntil(env, "user_1", "grok", "acc_1")).toBeNull()
    expect(await env.BENCH.get(key)).toBeNull()
  })
})

describe("isBenched (now backed by benchedUntil)", () => {
  it("true right after markBenched, matching the stored cooldown", async () => {
    const env = buildEnv()
    await markBenched(env, "user_1", "grok", "acc_1", 60_000)
    expect(await isBenched(env, "user_1", "grok", "acc_1")).toBe(true)
  })

  it("false for an account with no bench entry", async () => {
    const env = buildEnv()
    expect(await isBenched(env, "user_1", "grok", "acc_1")).toBe(false)
  })
})

describe("earliestBenchExpiry", () => {
  it("returns null for an empty id list", async () => {
    const env = buildEnv()
    expect(await earliestBenchExpiry(env, "user_1", "grok", [])).toBeNull()
  })

  it("returns null when none of the given ids are currently benched", async () => {
    const env = buildEnv()
    expect(await earliestBenchExpiry(env, "user_1", "grok", ["acc_1", "acc_2"])).toBeNull()
  })

  it("returns the EARLIEST expiry across several benched accounts, not the first or last id", async () => {
    const env = buildEnv()
    const now = Date.now()
    await env.BENCH.put(benchKey("user_1", "grok", "acc_1"), String(now + 120_000))
    await env.BENCH.put(benchKey("user_1", "grok", "acc_2"), String(now + 30_000))
    await env.BENCH.put(benchKey("user_1", "grok", "acc_3"), String(now + 90_000))
    expect(
      await earliestBenchExpiry(env, "user_1", "grok", ["acc_1", "acc_2", "acc_3"]),
    ).toBe(now + 30_000)
  })

  it("ignores ids with no bench entry mixed in with benched ones", async () => {
    const env = buildEnv()
    const until = Date.now() + 30_000
    await env.BENCH.put(benchKey("user_1", "grok", "acc_2"), String(until))
    expect(
      await earliestBenchExpiry(env, "user_1", "grok", ["acc_1", "acc_2", "acc_3"]),
    ).toBe(until)
  })
})
