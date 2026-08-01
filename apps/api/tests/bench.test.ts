import { describe, expect, it } from "vitest"
import { benchKey } from "../src/pool/bench"

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
