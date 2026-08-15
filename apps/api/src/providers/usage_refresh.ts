/**
 * Shared usage-refresh core (docs/providers.md § Usage cache) — the
 * single-flight lock/fetch/write machinery used by BOTH
 * `GET /api/providers/:provider/accounts` (routes/providers.ts) and the
 * routing module's background refresh fired on every dispatch
 * (docs/providers.md § Routing module "Facts"). The lock primitives
 * themselves (`acquireUsageLock` / `writeUsageSnapshot` / `releaseUsageLock`
 * — the compare-and-swap) live in `db/accounts.ts` and are never
 * reimplemented here; this module is the one place that calls them plus
 * `adapter.fetchUsage` and persists the result.
 */
import { decryptJson } from "../crypto/token_crypto"
import {
  acquireUsageLock,
  getAccount,
  isUsageFresh,
  readUsageSnapshot,
  releaseUsageLock,
  updateAccountIdentity,
  writeUsageSnapshot,
  type AccountRow,
  type UsageSnapshot,
} from "../db/accounts"
import type { Env } from "../env"
import type { StoredCredential } from "../pool/acquire"
import { pickAccountLabel } from "./identity"
import type { ProviderAdapter } from "./types"

export type UsagePersistResult = { ok: true; snapshot: UsageSnapshot } | { ok: false; error: string }

/**
 * Fetch usage from upstream and persist it. A thrown error releases the lock
 * without writing; a soft failure (adapter returns `error` with no windows)
 * still writes, merged with the prior snapshot's windows/account and flagged
 * `stale`, so the TTL restarts either way — one hiccup must not blank a good
 * snapshot. Caller must already hold `lockToken` from `acquireUsageLock`.
 */
export async function fetchAndPersistUsage(
  env: Env,
  row: AccountRow,
  adapter: ProviderAdapter,
  lockToken: string,
): Promise<UsagePersistResult> {
  try {
    const credential = await decryptJson<StoredCredential>(env.TOKEN_ENCRYPTION_KEY, row.encrypted_payload)
    const snap = await adapter.fetchUsage!(env, { row, credential })
    const priorMeta: Record<string, unknown> = row.account_meta_json
      ? (JSON.parse(row.account_meta_json) as Record<string, unknown>)
      : {}
    const accountMeta = { ...priorMeta, ...snap.account }
    // Prefer upstream email / username as stable pool label, same as the
    // route's inline version this replaces.
    const email = typeof snap.account.email === "string" ? snap.account.email : null
    const display =
      typeof snap.account.display_name === "string"
        ? snap.account.display_name
        : typeof snap.account.username === "string"
          ? snap.account.username
          : null
    const better = pickAccountLabel({ email, displayName: display, fallback: row.label || row.id })
    if (better && better !== row.label) {
      await updateAccountIdentity(env.DB, row.id, { label: better, accountMetaJson: JSON.stringify(accountMeta) })
    } else {
      await updateAccountIdentity(env.DB, row.id, { accountMetaJson: JSON.stringify(accountMeta) })
    }
    // A soft failure (error + no windows of its own) is a failed read wearing
    // a 200 — same rule as a thrown error: never overwrite a good snapshot
    // (docs/providers.md § Usage cache "'Failure' includes a soft failure").
    // Windows always win when they do arrive, error or not.
    const priorSnapshot = snap.windows.length === 0 && snap.error ? readUsageSnapshot(row) : null
    const persisted: UsageSnapshot = priorSnapshot
      ? {
          windows: priorSnapshot.windows,
          account: priorSnapshot.account,
          error: snap.error ?? null,
          stale: true,
          edgeBlocked: !!snap.edgeBlocked,
        }
      : {
          windows: snap.windows,
          account: snap.account,
          error: snap.error ?? null,
          stale: !!snap.stale,
          edgeBlocked: !!snap.edgeBlocked,
        }
    await writeUsageSnapshot(env.DB, row.id, lockToken, persisted)
    return { ok: true, snapshot: persisted }
  } catch (e) {
    await releaseUsageLock(env.DB, row.id, lockToken)
    return { ok: false, error: e instanceof Error ? e.message : "usage failed" }
  }
}

/**
 * Background refresh (docs/providers.md § Routing module "Facts"): fired
 * via `waitUntil` on every dispatch so limit-aware skip facts stay warm
 * while traffic flows, reusing the exact 90s-TTL single-flight cache `GET
 * /api/providers/:provider/accounts` uses — zero added request latency.
 *
 * Fresh-within-90s short-circuits with no upstream call. A lock already
 * held by another caller (another concurrent request, or the accounts-page
 * poll) also short-circuits without queuing — the stored snapshot already
 * serves every reader; there is nothing here to report a failure to, so a
 * lost race is simply a no-op rather than a queued retry.
 */
export async function refreshAccountUsageInBackground(
  env: Env,
  row: AccountRow,
  adapter: ProviderAdapter,
): Promise<void> {
  if (!adapter.fetchUsage) return
  if (isUsageFresh(row)) return
  const lockToken = await acquireUsageLock(env.DB, row.id)
  if (!lockToken) return
  // Dispatch may have refreshed this account after it loaded `row`; use the
  // post-lock D1 row so fetchUsage never sends a stale credential.
  const freshRow = await getAccount(env.DB, row.user_id, row.id)
  if (!freshRow) {
    await releaseUsageLock(env.DB, row.id, lockToken)
    return
  }
  await fetchAndPersistUsage(env, freshRow, adapter, lockToken)
}
