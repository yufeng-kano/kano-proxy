import { describe, expect, it } from "vitest"
import { shouldBenchStatus } from "../src/pool/acquire"

describe("shouldBenchStatus", () => {
  it("benches on 401/402/403/429 — the documented failover set (docs/api.md 'Model routing')", () => {
    expect(shouldBenchStatus(401)).toBe(true)
    expect(shouldBenchStatus(402)).toBe(true)
    expect(shouldBenchStatus(403)).toBe(true)
    expect(shouldBenchStatus(429)).toBe(true)
  })

  it("402 specifically benches — OpenRouter-style 'Insufficient credits' must not be retried in place", () => {
    expect(shouldBenchStatus(402)).toBe(true)
  })

  it("does not bench on success, other 4xx, or 5xx statuses", () => {
    expect(shouldBenchStatus(200)).toBe(false)
    expect(shouldBenchStatus(400)).toBe(false)
    expect(shouldBenchStatus(404)).toBe(false)
    expect(shouldBenchStatus(408)).toBe(false)
    expect(shouldBenchStatus(409)).toBe(false)
    expect(shouldBenchStatus(422)).toBe(false)
    expect(shouldBenchStatus(500)).toBe(false)
    expect(shouldBenchStatus(502)).toBe(false)
    expect(shouldBenchStatus(503)).toBe(false)
  })
})
