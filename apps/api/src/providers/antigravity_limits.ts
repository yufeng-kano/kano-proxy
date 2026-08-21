/**
 * Antigravity 429 classification, derived from CLIProxyAPI
 * `internal/runtime/executor/antigravity_executor_credits.go`
 * (`decideAntigravity429`) and `helps/json_retry_helpers.go`
 * (`ParseRetryDelay`).
 *
 * The CloudCode backend returns one status — 429 — for two very different
 * situations, and only the body tells them apart:
 *
 * - **Quota exhausted** — the subscription's allowance for that model family
 *   is spent. Retrying in five minutes just burns the account again, so this
 *   benches long (docs/providers.md § Antigravity).
 * - **Rate limited** — a short, transient throttle that carries its own
 *   `retryDelay`. This benches for exactly that delay and fails over.
 *
 * Anything the body does not clearly place in either bucket is left
 * unclassified, and the routing module's ordinary 429 default applies.
 */

export type Antigravity429 =
  | { kind: "quota_exhausted"; retryAfterMs: number | null }
  | { kind: "rate_limited"; retryAfterMs: number }
  | { kind: "unknown"; retryAfterMs: number | null }

/**
 * A protobuf `Duration` as proto-JSON: seconds with an optional fraction and a
 * mandatory `s` suffix (`"17s"`, `"1.5s"`). Anything else is not a duration we
 * are willing to act on.
 */
export function parseProtoDurationMs(value: unknown): number | null {
  if (typeof value !== "string") return null
  const match = /^(\d+(?:\.\d+)?)s$/.exec(value.trim())
  if (!match) return null
  const seconds = Number(match[1])
  return Number.isFinite(seconds) ? Math.round(seconds * 1000) : null
}

type ErrorDetail = {
  "@type"?: unknown
  reason?: unknown
  retryDelay?: unknown
  metadata?: { quotaResetDelay?: unknown }
}

function errorDetails(body: unknown): ErrorDetail[] {
  if (!body || typeof body !== "object") return []
  const error = (body as { error?: unknown }).error
  if (!error || typeof error !== "object") return []
  const details = (error as { details?: unknown }).details
  return Array.isArray(details) ? (details as ErrorDetail[]) : []
}

/**
 * How long upstream says to wait. Google states this three different ways and
 * CLIProxyAPI reads all three in this order: a `RetryInfo` detail's
 * `retryDelay`, then an `ErrorInfo` detail's `metadata.quotaResetDelay`, then
 * an `after <n>s` phrase inside the human-readable message.
 */
export function antigravityRetryDelayMs(body: unknown): number | null {
  const details = errorDetails(body)
  for (const detail of details) {
    if (detail["@type"] !== "type.googleapis.com/google.rpc.RetryInfo") continue
    const ms = parseProtoDurationMs(detail.retryDelay)
    if (ms !== null) return ms
  }
  for (const detail of details) {
    if (detail["@type"] !== "type.googleapis.com/google.rpc.ErrorInfo") continue
    const ms = parseProtoDurationMs(detail.metadata?.quotaResetDelay)
    if (ms !== null) return ms
  }
  const message =
    body && typeof body === "object"
      ? (body as { error?: { message?: unknown } }).error?.message
      : undefined
  if (typeof message === "string") {
    const match = /after\s+(\d+)s\.?/.exec(message)
    if (match) return Number(match[1]) * 1000
  }
  return null
}

/**
 * A `RATE_LIMIT_EXCEEDED` whose delay is at least this long is treated as
 * quota exhaustion rather than a throttle — CLIProxyAPI's own
 * `antigravityShortQuotaCooldownThreshold`. Below it the account recovers on
 * its own soon enough to be worth coming back to.
 */
export const ANTIGRAVITY_SHORT_COOLDOWN_MS = 5 * 60_000

export function classifyAntigravity429(body: unknown): Antigravity429 {
  const retryAfterMs = antigravityRetryDelayMs(body)
  const status =
    body && typeof body === "object"
      ? (body as { error?: { status?: unknown } }).error?.status
      : undefined
  if (typeof status !== "string" || status.toUpperCase() !== "RESOURCE_EXHAUSTED") {
    return { kind: "unknown", retryAfterMs }
  }

  for (const detail of errorDetails(body)) {
    if (detail["@type"] !== "type.googleapis.com/google.rpc.ErrorInfo") continue
    const reason = typeof detail.reason === "string" ? detail.reason.toUpperCase() : ""
    if (reason === "QUOTA_EXHAUSTED") return { kind: "quota_exhausted", retryAfterMs }
    if (reason === "RATE_LIMIT_EXCEEDED") {
      if (retryAfterMs === null) return { kind: "unknown", retryAfterMs }
      return retryAfterMs >= ANTIGRAVITY_SHORT_COOLDOWN_MS
        ? { kind: "quota_exhausted", retryAfterMs }
        : { kind: "rate_limited", retryAfterMs }
    }
  }

  // No structured reason: fall back to the same keyword sniff CLIProxyAPI does.
  const text = JSON.stringify(body ?? "").toLowerCase()
  if (text.includes("quota_exhausted") || text.includes("quota exhausted")) {
    return { kind: "quota_exhausted", retryAfterMs }
  }
  return { kind: "unknown", retryAfterMs }
}

/**
 * Quota exhaustion with no upstream reset to go on. Google returns no reset
 * timestamp in that case, and the routing module's 300s default would put the
 * account straight back into rotation to fail again, so the adapter asks for an
 * hour instead. This number is a **chosen heuristic**, not an upstream fact —
 * see docs/providers.md § Antigravity.
 */
export const ANTIGRAVITY_QUOTA_BENCH_MS = 60 * 60_000

/** Epoch-ms this account should stay benched, or `null` to leave the default alone. */
export function antigravityBenchUntil(body: unknown, now = Date.now()): number | null {
  const verdict = classifyAntigravity429(body)
  if (verdict.kind === "quota_exhausted") {
    return now + (verdict.retryAfterMs ?? ANTIGRAVITY_QUOTA_BENCH_MS)
  }
  if (verdict.kind === "rate_limited") return now + verdict.retryAfterMs
  return null
}

/**
 * "No capacity" is a fleet-side condition, not an account one — CLIProxyAPI
 * retries the other base URL rather than penalising the credential
 * (`antigravityShouldRetryNoCapacity`).
 */
export function isAntigravityNoCapacity(status: number, body: unknown): boolean {
  if (status !== 429 && status !== 503) return false
  const text = JSON.stringify(body ?? "").toLowerCase()
  return text.includes("no capacity") || text.includes("no_capacity")
}
