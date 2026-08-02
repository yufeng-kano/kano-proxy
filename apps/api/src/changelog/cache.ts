/**
 * Release-notes cache (KV).
 *
 * Two knobs, deliberately different from the 90s per-account caches:
 *   - entries live 7 days, so a failed refetch always has something to fall
 *     back on (see the stale-serve note in docs/changelog.md)
 *   - a refetch is only attempted once the entry is an hour old
 *
 * The key is **global** — no user id, unlike `usage:v1:<userId>:…` and
 * `models:v1:<userId>:…`. Release notes are identical for every operator, and
 * a per-user key would multiply GitHub calls by the number of signed-in users
 * against a 60/hr unauthenticated budget.
 */

import type { Env } from "../env"

export const CHANGELOG_CACHE_TTL_SECONDS = 7 * 24 * 60 * 60
export const CHANGELOG_FRESH_MS = 60 * 60 * 1000

export type ChangelogRelease = {
  tag: string
  name: string
  published_at: string
  url: string
  /** Already sanitized by `sanitizeReleaseHtml` before it reaches KV. */
  body_html: string
}

export type CachedChangelog = {
  releases: ChangelogRelease[]
  latest: string | null
  error: string | null
  fetchedAt: number
}

export function changelogCacheKey(): string {
  return "changelog:v1"
}

/**
 * Returns the entry even when it is past the freshness window — the caller
 * decides whether to refetch, and needs the old value to fall back on when
 * that refetch fails. (The per-account caches discard an aged entry here;
 * doing that would make stale-serve impossible.)
 */
export async function readChangelogCache(
  env: Env,
): Promise<CachedChangelog | null> {
  try {
    const raw = await env.CACHE.get(changelogCacheKey(), "json")
    if (!raw || typeof raw !== "object") return null
    const snap = raw as CachedChangelog
    if (typeof snap.fetchedAt !== "number") return null
    if (!Array.isArray(snap.releases)) return null
    return snap
  } catch {
    return null
  }
}

export function isChangelogFresh(snap: CachedChangelog): boolean {
  return Date.now() - snap.fetchedAt < CHANGELOG_FRESH_MS
}

export async function writeChangelogCache(
  env: Env,
  snap: Omit<CachedChangelog, "fetchedAt">,
): Promise<void> {
  const payload: CachedChangelog = { ...snap, fetchedAt: Date.now() }
  try {
    await env.CACHE.put(changelogCacheKey(), JSON.stringify(payload), {
      expirationTtl: CHANGELOG_CACHE_TTL_SECONDS,
    })
  } catch {
    /* cache write must not break the request */
  }
}
