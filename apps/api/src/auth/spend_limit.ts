/**
 * Per-key spend windows (docs/pricing.md).
 *
 * The window sum reads only stored `request_logs.cost` values through one
 * indexed aggregate. Enforcement memoizes that sum per isolate for 60s so a
 * busy key does not add a D1 aggregate to every request; the admin keys list
 * calls the uncached form for fresh display. A D1 failure reads as "unknown"
 * (null) and the middleware fails open — an infrastructure hiccup must not
 * take the proxy down.
 */

import type { Env } from "../env"
import { PROVIDERS } from "../env"
import type { ApiKeyRow } from "../db/keys"

export type SpendLimitInterval = "daily" | "weekly" | "monthly" | "total"

export const SPEND_LIMIT_INTERVALS: SpendLimitInterval[] = [
  "daily",
  "weekly",
  "monthly",
  "total",
]

export function isSpendLimitInterval(v: unknown): v is SpendLimitInterval {
  return typeof v === "string" && (SPEND_LIMIT_INTERVALS as string[]).includes(v)
}

/**
 * Inclusive ISO lower bound of the current window. Unknown values fall back
 * to monthly — the column default — rather than throwing on a hand-edited row.
 */
export function spendWindowStart(interval: string, nowMs = Date.now()): string {
  const d = new Date(nowMs)
  if (interval === "total") return "1970-01-01T00:00:00.000Z"
  if (interval === "daily") {
    return new Date(Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate())).toISOString()
  }
  if (interval === "weekly") {
    // ISO week: Monday 00:00 UTC. getUTCDay is 0 for Sunday.
    const sinceMonday = (d.getUTCDay() + 6) % 7
    return new Date(
      Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate() - sinceMonday),
    ).toISOString()
  }
  return new Date(Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), 1)).toISOString()
}

/**
 * Estimated USD this key spent in its current window; null on D1 failure.
 * `include_oauth = 0` excludes the builtin subscription providers — custom
 * (BYO-key) traffic always counts.
 */
export async function keyWindowSpend(
  env: Env,
  key: Pick<ApiKeyRow, "id" | "spend_limit_interval" | "spend_limit_include_oauth">,
  nowMs = Date.now(),
): Promise<number | null> {
  const since = spendWindowStart(key.spend_limit_interval, nowMs)
  try {
    if (key.spend_limit_include_oauth === 0) {
      const placeholders = PROVIDERS.map(() => "?").join(", ")
      const row = await env.DB.prepare(
        `SELECT COALESCE(SUM(cost), 0) as s FROM request_logs
         WHERE api_key_id = ? AND created_at >= ? AND provider NOT IN (${placeholders})`,
      )
        .bind(key.id, since, ...PROVIDERS)
        .first<{ s: number }>()
      return row?.s ?? 0
    }
    const row = await env.DB.prepare(
      `SELECT COALESCE(SUM(cost), 0) as s FROM request_logs
       WHERE api_key_id = ? AND created_at >= ?`,
    )
      .bind(key.id, since)
      .first<{ s: number }>()
    return row?.s ?? 0
  } catch {
    return null
  }
}

const MEMO_TTL_MS = 60_000
/** Keyed by id + the limit settings, so a PATCH starts a fresh memo entry. */
const memo = new Map<string, { at: number; spend: number }>()

export function _resetSpendMemoForTests(): void {
  memo.clear()
}

/** Memoized `keyWindowSpend` for the request path. A null (D1 failure) is never memoized. */
export async function keyWindowSpendCached(
  env: Env,
  key: Pick<ApiKeyRow, "id" | "spend_limit_interval" | "spend_limit_include_oauth">,
  nowMs = Date.now(),
): Promise<number | null> {
  const memoKey = `${key.id}:${key.spend_limit_interval}:${key.spend_limit_include_oauth}`
  const hit = memo.get(memoKey)
  if (hit && nowMs - hit.at < MEMO_TTL_MS) return hit.spend
  const spend = await keyWindowSpend(env, key, nowMs)
  if (spend != null) {
    memo.set(memoKey, { at: nowMs, spend })
    // The map only ever holds keys seen by this isolate, but a long-lived
    // isolate serving many keys should not grow it unboundedly.
    if (memo.size > 1000) {
      for (const k of memo.keys()) {
        if (memo.size <= 500) break
        memo.delete(k)
      }
    }
  }
  return spend
}
