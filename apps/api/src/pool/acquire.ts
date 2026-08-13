import { decryptJson, encryptJson } from "../crypto/token_crypto"
import { listAccounts, updateAccountPayload, type AccountRow } from "../db/accounts"
import type { Env } from "../env"
import { isBenched, markBenched } from "./bench"

export type StoredCredential = {
  access_token: string
  refresh_token?: string | null
  expires_at?: string | null
  account_id?: string | null
  email?: string | null
  client_id?: string | null
  token_endpoint?: string | null
  extra?: Record<string, unknown>
}

export type AcquiredAccount = {
  row: AccountRow
  credential: StoredCredential
}

/**
 * Statuses that bench an account and fail over to the next one in the pool:
 * 401/403 (auth), 429 (rate limit), 402 (billing/credit exhaustion — e.g.
 * OpenRouter's `402 Insufficient credits`; the account is unusable until
 * topped up, so retrying it per-request just burns a failing upstream
 * round-trip). See docs/api.md "Model routing".
 */
const FAILOVER_STATUS = new Set([401, 402, 403, 429])

export function shouldBenchStatus(status: number): boolean {
  return FAILOVER_STATUS.has(status)
}

/**
 * `provider` is a builtin `ProviderId` or a custom provider's slug — pool
 * semantics (acquire/bench/promote) are identical for both, so this layer
 * is typed as `string` rather than duplicated per kind.
 *
 * `pinnedAccountId` (model-group account pinning, docs/providers.md §
 * Model groups "Account pinning"): when set, the candidate set collapses to
 * that one account id — the pool's own priority is bypassed and, because
 * dispatch's failover loop re-calls this with the tried id added to
 * `exclude`, the pinned account never fails over to a sibling: the next call
 * finds its only candidate already excluded and returns empty, same as an
 * exhausted single-account pool.
 */
export async function listUsableAccounts(
  env: Env,
  userId: string,
  provider: string,
  exclude: Set<string> = new Set(),
  pinnedAccountId?: string,
): Promise<AccountRow[]> {
  const rows = await listAccounts(env.DB, userId, provider)
  const out: AccountRow[] = []
  for (const row of rows) {
    if (pinnedAccountId && row.id !== pinnedAccountId) continue
    if (exclude.has(row.id)) continue
    if (await isBenched(env, userId, provider, row.id)) continue
    out.push(row)
  }
  return out
}

export async function acquireAccount(
  env: Env,
  userId: string,
  provider: string,
  exclude: Set<string> = new Set(),
  pinnedAccountId?: string,
): Promise<AcquiredAccount | null> {
  const candidates = await listUsableAccounts(env, userId, provider, exclude, pinnedAccountId)
  for (const row of candidates) {
    try {
      const credential = await decryptJson<StoredCredential>(
        env.TOKEN_ENCRYPTION_KEY,
        row.encrypted_payload,
      )
      return { row, credential }
    } catch {
      // skip unreadable
      continue
    }
  }
  return null
}

export async function benchAccount(
  env: Env,
  userId: string,
  provider: string,
  accountId: string,
): Promise<void> {
  await markBenched(env, userId, provider, accountId)
}

export async function saveCredential(
  env: Env,
  accountId: string,
  credential: StoredCredential,
  meta?: { label?: string | null; accountMetaJson?: string | null },
): Promise<void> {
  const blob = await encryptJson(env.TOKEN_ENCRYPTION_KEY, credential)
  await updateAccountPayload(env.DB, accountId, blob, meta)
}

export { shouldBenchStatus as isFailoverStatus }
