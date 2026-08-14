/**
 * Per-candidate usability facts (docs/providers.md § Routing module
 * "Facts") — computed from stored state ONLY. Dispatch never makes a
 * synchronous upstream call to learn these; the two sources are:
 *
 * - Bench state (KV `BENCH`, as before).
 * - Usage windows from the account row's stored `usage_snapshot_json`: any
 *   window with `utilization >= 100` marks the candidate unusable until
 *   that window's `resets_at`. Precision is bounded by snapshot freshness,
 *   but the skip self-expires at `resets_at` even with a stale snapshot —
 *   a stale snapshot can never bench an account past its real reset.
 *
 * A malformed or missing snapshot reads as "no window fact" (fail open) —
 * `readUsageSnapshot` already returns `null` for that case.
 */
import { readUsageSnapshot, type AccountRow } from "../db/accounts"
import type { Env } from "../env"
import { benchedUntil } from "../pool/bench"
import type { CandidateFacts, RoutingCandidate } from "./types"

/**
 * The latest `resets_at` among the account's currently-exhausted windows
 * (`utilization >= 100` AND `resets_at` still in the future) — the
 * candidate stays unusable until every exhausted window has cleared, not
 * just the first one. `null` when no window is currently exhausted (never
 * fetched, none over 100%, or every over-100% window's `resets_at` already
 * passed — the self-expiry the doc requires even off a stale snapshot).
 */
export function usageWindowUnusableUntil(row: AccountRow, now = Date.now()): number | null {
  const snapshot = readUsageSnapshot(row)
  if (!snapshot) return null
  let latest: number | null = null
  for (const w of snapshot.windows) {
    const window = w as { utilization?: number | null; resets_at?: string | null }
    if (typeof window.utilization !== "number" || window.utilization < 100) continue
    if (!window.resets_at) continue
    const at = Date.parse(window.resets_at)
    if (!Number.isFinite(at) || at <= now) continue
    if (latest === null || at > latest) latest = at
  }
  return latest
}

/** Bench-until and usage-window facts for one candidate. */
export async function candidateFacts(
  env: Env,
  userId: string,
  candidate: RoutingCandidate,
  now = Date.now(),
): Promise<CandidateFacts> {
  const benchUntil = await benchedUntil(env, userId, candidate.provider, candidate.account.id)
  const windowUntil = usageWindowUnusableUntil(candidate.account, now)
  const unusableUntil =
    benchUntil === null ? windowUntil : windowUntil === null ? benchUntil : Math.max(benchUntil, windowUntil)
  return {
    usable: unusableUntil === null,
    unusableUntil,
    benchUntil,
    usageWindowUntil: windowUntil,
  }
}

/** Facts for a whole candidate list, in the same order — one KV read per candidate, in parallel. */
export async function candidateFactsList(
  env: Env,
  userId: string,
  candidates: RoutingCandidate[],
  now = Date.now(),
): Promise<CandidateFacts[]> {
  return Promise.all(candidates.map((c) => candidateFacts(env, userId, c, now)))
}

/** Earliest `unusableUntil` across a set of facts, or `null` when none carry one — used for `Retry-After`. */
export function earliestUnusableUntil(facts: CandidateFacts[]): number | null {
  let earliest: number | null = null
  for (const f of facts) {
    if (f.unusableUntil === null) continue
    if (earliest === null || f.unusableUntil < earliest) earliest = f.unusableUntil
  }
  return earliest
}
