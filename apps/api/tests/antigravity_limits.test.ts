import { describe, expect, it } from "vitest"
import {
  ANTIGRAVITY_QUOTA_BENCH_MS,
  ANTIGRAVITY_SHORT_COOLDOWN_MS,
  antigravityBenchUntil,
  antigravityRetryDelayMs,
  classifyAntigravity429,
  isAntigravityNoCapacity,
  parseProtoDurationMs,
} from "../src/providers/antigravity_limits"

function resourceExhausted(reason: string, details: unknown[] = []): unknown {
  return {
    error: {
      code: 429,
      status: "RESOURCE_EXHAUSTED",
      message: `Quota trouble: ${reason}`,
      details,
    },
  }
}

function errorInfo(reason: string, metadata?: Record<string, unknown>): unknown {
  return {
    "@type": "type.googleapis.com/google.rpc.ErrorInfo",
    reason,
    ...(metadata ? { metadata } : {}),
  }
}

function retryInfo(retryDelay: string): unknown {
  return { "@type": "type.googleapis.com/google.rpc.RetryInfo", retryDelay }
}

describe("parseProtoDurationMs", () => {
  it("reads whole and fractional second durations", () => {
    expect(parseProtoDurationMs("17s")).toBe(17_000)
    expect(parseProtoDurationMs("1.5s")).toBe(1_500)
  })

  it("refuses anything that is not a proto duration", () => {
    expect(parseProtoDurationMs("17")).toBeNull()
    expect(parseProtoDurationMs("17ms")).toBeNull()
    expect(parseProtoDurationMs(17)).toBeNull()
    expect(parseProtoDurationMs(undefined)).toBeNull()
  })
})

describe("antigravityRetryDelayMs", () => {
  it("prefers a RetryInfo detail", () => {
    const body = resourceExhausted("x", [
      errorInfo("RATE_LIMIT_EXCEEDED", { quotaResetDelay: "600s" }),
      retryInfo("12s"),
    ])
    expect(antigravityRetryDelayMs(body)).toBe(12_000)
  })

  it("falls back to ErrorInfo metadata.quotaResetDelay", () => {
    const body = resourceExhausted("x", [
      errorInfo("RATE_LIMIT_EXCEEDED", { quotaResetDelay: "600s" }),
    ])
    expect(antigravityRetryDelayMs(body)).toBe(600_000)
  })

  it("falls back to an 'after Ns' phrase in the message", () => {
    expect(
      antigravityRetryDelayMs({ error: { message: "Try again after 45s." } }),
    ).toBe(45_000)
  })

  it("is null when nothing states a delay", () => {
    expect(antigravityRetryDelayMs({ error: { message: "nope" } })).toBeNull()
    expect(antigravityRetryDelayMs(null)).toBeNull()
  })
})

describe("classifyAntigravity429", () => {
  it("classifies an explicit QUOTA_EXHAUSTED reason", () => {
    const body = resourceExhausted("quota", [errorInfo("QUOTA_EXHAUSTED")])
    expect(classifyAntigravity429(body)).toEqual({
      kind: "quota_exhausted",
      retryAfterMs: null,
    })
  })

  it("treats a short RATE_LIMIT_EXCEEDED delay as a transient throttle", () => {
    const body = resourceExhausted("rl", [
      errorInfo("RATE_LIMIT_EXCEEDED"),
      retryInfo("20s"),
    ])
    expect(classifyAntigravity429(body)).toEqual({
      kind: "rate_limited",
      retryAfterMs: 20_000,
    })
  })

  it("promotes a long RATE_LIMIT_EXCEEDED delay to quota exhaustion", () => {
    const body = resourceExhausted("rl", [
      errorInfo("RATE_LIMIT_EXCEEDED"),
      retryInfo("3600s"),
    ])
    expect(classifyAntigravity429(body)).toEqual({
      kind: "quota_exhausted",
      retryAfterMs: 3_600_000,
    })
  })

  it("uses the threshold boundary inclusively — exactly 5 minutes is quota", () => {
    const body = resourceExhausted("rl", [
      errorInfo("RATE_LIMIT_EXCEEDED"),
      retryInfo(`${ANTIGRAVITY_SHORT_COOLDOWN_MS / 1000}s`),
    ])
    expect(classifyAntigravity429(body).kind).toBe("quota_exhausted")
  })

  it("leaves a RATE_LIMIT_EXCEEDED with no delay unclassified", () => {
    const body = resourceExhausted("rl", [errorInfo("RATE_LIMIT_EXCEEDED")])
    expect(classifyAntigravity429(body).kind).toBe("unknown")
  })

  it("falls back to the quota keyword when no structured reason is present", () => {
    const body = {
      error: { status: "RESOURCE_EXHAUSTED", message: "model quota exhausted for today" },
    }
    expect(classifyAntigravity429(body).kind).toBe("quota_exhausted")
  })

  it("does not classify a non-RESOURCE_EXHAUSTED 429", () => {
    const body = { error: { status: "UNAVAILABLE", message: "backend busy" } }
    expect(classifyAntigravity429(body).kind).toBe("unknown")
  })
})

describe("antigravityBenchUntil", () => {
  const now = 1_700_000_000_000

  it("benches an hour when quota is spent with no upstream reset", () => {
    const body = resourceExhausted("quota", [errorInfo("QUOTA_EXHAUSTED")])
    expect(antigravityBenchUntil(body, now)).toBe(now + ANTIGRAVITY_QUOTA_BENCH_MS)
  })

  it("prefers the upstream reset over the heuristic when one is given", () => {
    const body = resourceExhausted("quota", [errorInfo("QUOTA_EXHAUSTED"), retryInfo("90s")])
    expect(antigravityBenchUntil(body, now)).toBe(now + 90_000)
  })

  it("benches exactly the throttle window for a transient rate limit", () => {
    const body = resourceExhausted("rl", [errorInfo("RATE_LIMIT_EXCEEDED"), retryInfo("20s")])
    expect(antigravityBenchUntil(body, now)).toBe(now + 20_000)
  })

  it("leaves the routing module's default alone when unclassified", () => {
    expect(antigravityBenchUntil({ error: { message: "?" } }, now)).toBeNull()
  })
})

describe("isAntigravityNoCapacity", () => {
  it("recognizes a no-capacity body on 429 and 503", () => {
    const body = { error: { message: "no capacity available for this model" } }
    expect(isAntigravityNoCapacity(429, body)).toBe(true)
    expect(isAntigravityNoCapacity(503, body)).toBe(true)
  })

  it("ignores other statuses and unrelated bodies", () => {
    expect(isAntigravityNoCapacity(500, { error: { message: "no capacity" } })).toBe(false)
    expect(isAntigravityNoCapacity(429, { error: { message: "quota" } })).toBe(false)
  })
})
