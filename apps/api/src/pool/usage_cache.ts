/**
 * Per-account usage snapshot cache (KV).
 * Default TTL 90s — avoid hammering Claude/Codex/Grok usage endpoints (429).
 * Matches lincy proxy usage_snapshot caching; kano-proxy uses 90s as product default.
 */

import type { Env } from "../env"
import type { UsageWindow } from "../providers/types"

export const USAGE_CACHE_TTL_SECONDS = 90

export type CachedUsageSnap = {
  windows: UsageWindow[]
  account: Record<string, unknown>
  stale: boolean
  error: string | null
  edgeBlocked?: boolean
  fetchedAt: number
}

export function usageCacheKey(
  userId: string,
  provider: string,
  accountId: string,
): string {
  return `usage:v1:${userId}:${provider}:${accountId}`
}

export async function readUsageCache(
  env: Env,
  userId: string,
  provider: string,
  accountId: string,
): Promise<CachedUsageSnap | null> {
  try {
    const raw = await env.CACHE.get(
      usageCacheKey(userId, provider, accountId),
      "json",
    )
    if (!raw || typeof raw !== "object") return null
    const snap = raw as CachedUsageSnap
    if (typeof snap.fetchedAt !== "number") return null
    // Extra client-side age guard if KV TTL lags
    if (Date.now() - snap.fetchedAt > USAGE_CACHE_TTL_SECONDS * 1000 + 5_000) {
      return null
    }
    return snap
  } catch {
    return null
  }
}

export async function writeUsageCache(
  env: Env,
  userId: string,
  provider: string,
  accountId: string,
  snap: Omit<CachedUsageSnap, "fetchedAt">,
): Promise<void> {
  const payload: CachedUsageSnap = { ...snap, fetchedAt: Date.now() }
  try {
    await env.CACHE.put(
      usageCacheKey(userId, provider, accountId),
      JSON.stringify(payload),
      { expirationTtl: USAGE_CACHE_TTL_SECONDS },
    )
  } catch {
    /* cache write must not break the request */
  }
}
