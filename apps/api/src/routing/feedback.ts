/**
 * Penalties (docs/providers.md § Routing module "Penalties") — what a
 * failed upstream response costs the account, decided in one place:
 *
 * | Upstream outcome | Penalty |
 * |---|---|
 * | 401/403 (auth), 402 (billing) | bench 300s |
 * | 429 (rate limit) | bench until the upstream reset when derivable — reset headers, else the earliest exhausted window's `resets_at`, else 300s; capped at 7 days |
 * | 520/522/524 (upstream edge failed/timed out before first byte) | request-local exclusion; the third fresh strike benches 30s |
 * | anything else non-2xx | no bench — passthrough / in-stream error, unchanged |
 *
 * Bench outcomes and edge-timeout exclusions both continue to the next
 * candidate; dispatch owns the walk and strike persistence.
 */
import { readUsageSnapshot, type AccountRow } from "../db/accounts"

const DEFAULT_COOLDOWN_MS = 300_000
export const EDGE_TIMEOUT_COOLDOWN_MS = 30_000
const MAX_COOLDOWN_MS = 7 * 24 * 60 * 60 * 1000

const AUTH_BILLING_STATUSES = new Set([401, 402, 403])
const EDGE_TIMEOUT_STATUSES = new Set([520, 522, 524])

export type Penalty = { cooldownMs: number }

/** 520/522/524 always fail over, but only record a persistent strike. */
export function isEdgeTimeoutStatus(status: number): boolean {
  return EDGE_TIMEOUT_STATUSES.has(status)
}

function clampCooldown(ms: number): number {
  return Math.min(MAX_COOLDOWN_MS, Math.max(0, ms))
}

/**
 * Latest `resets_at` among any `anthropic-ratelimit-*-reset` response
 * header (RFC 3339 timestamps) — taking the latest, not the first found, so
 * a multi-dimension 429 (requests vs. tokens, each with its own reset)
 * doesn't get retried before every dimension has actually cleared.
 */
function rateLimitResetHeaderMs(headers: Headers): number | null {
  let latest: number | null = null
  for (const [key, value] of headers.entries()) {
    if (!/^anthropic-ratelimit-.*-reset$/i.test(key)) continue
    const at = Date.parse(value)
    if (!Number.isFinite(at)) continue
    if (latest === null || at > latest) latest = at
  }
  return latest
}

/** Earliest `resets_at` among the account's currently-exhausted (`utilization >= 100`) windows, per docs/providers.md § Routing module "Penalties". */
function earliestExhaustedWindowResetMs(account: AccountRow, now: number): number | null {
  const snapshot = readUsageSnapshot(account)
  if (!snapshot) return null
  let earliest: number | null = null
  for (const w of snapshot.windows) {
    const window = w as { utilization?: number | null; resets_at?: string | null }
    if (typeof window.utilization !== "number" || window.utilization < 100) continue
    if (!window.resets_at) continue
    const at = Date.parse(window.resets_at)
    if (!Number.isFinite(at) || at <= now) continue
    if (earliest === null || at < earliest) earliest = at
  }
  return earliest
}

/**
 * Proxy-internal reset hint (epoch ms), for adapters whose upstream states the
 * reset in the **body** rather than a header — antigravity's 429 carries a
 * `RetryInfo` detail, and only its adapter can classify quota exhaustion apart
 * from a transient throttle (docs/providers.md § Antigravity). Set by an
 * adapter on the Response it hands back; no upstream ever sends it, so this
 * cannot change what the other providers already do.
 */
export const RATELIMIT_RESET_HINT_HEADER = "x-kano-ratelimit-reset"

function resetHintHeaderMs(headers: Headers): number | null {
  const raw = headers.get(RATELIMIT_RESET_HINT_HEADER)
  if (!raw) return null
  const at = Number(raw)
  return Number.isFinite(at) ? at : null
}

function rateLimitCooldownMs(headers: Headers, account: AccountRow, now: number): number {
  const hintMs = resetHintHeaderMs(headers)
  if (hintMs !== null) return clampCooldown(hintMs - now)
  const headerMs = rateLimitResetHeaderMs(headers)
  if (headerMs !== null) return clampCooldown(headerMs - now)
  const snapshotMs = earliestExhaustedWindowResetMs(account, now)
  if (snapshotMs !== null) return clampCooldown(snapshotMs - now)
  return DEFAULT_COOLDOWN_MS
}

/**
 * The penalty for one upstream attempt's outcome, or `null` when the
 * outcome is not a bench-and-try-next-candidate status — that status
 * passes through / in-stream errors exactly as before, untouched by the
 * routing module.
 */
export function penaltyForOutcome(
  status: number,
  headers: Headers,
  account: AccountRow,
  now: number = Date.now(),
): Penalty | null {
  if (AUTH_BILLING_STATUSES.has(status)) return { cooldownMs: DEFAULT_COOLDOWN_MS }
  if (status === 429) {
    return { cooldownMs: rateLimitCooldownMs(headers, account, now) }
  }
  return null
}

/** Same status set `penaltyForOutcome` benches on — kept for call sites that only need the yes/no check. */
export function isBenchStatus(status: number): boolean {
  return AUTH_BILLING_STATUSES.has(status) || status === 429
}
