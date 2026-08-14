import { decryptJson, encryptJson } from "../crypto/token_crypto"
import {
  acquireRefreshLock,
  getAccount,
  releaseRefreshLock,
  writeRefreshedCredential,
} from "../db/accounts"
import type { Env } from "../env"
import type { AcquiredAccount, StoredCredential } from "../pool/acquire"

const REFRESH_POLL_ATTEMPTS = 3
const REFRESH_POLL_DELAY_MS = 25

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

/**
 * OAuth refresh single-flight for every adapter entry point. The winner reloads
 * after claiming, persists its new encrypted payload while compare-releasing,
 * and losers briefly poll for that persisted credential before fail-opening to
 * their old one.
 */
export async function refreshOAuthCredential(
  env: Env,
  account: AcquiredAccount,
  needsRefresh: (credential: StoredCredential) => boolean,
  refresh: (credential: StoredCredential) => Promise<StoredCredential | null>,
): Promise<AcquiredAccount> {
  if (!needsRefresh(account.credential)) return account

  for (let attempt = 0; attempt <= REFRESH_POLL_ATTEMPTS; attempt++) {
    // Losers poll first. Only after the short wait finds a free lock do they
    // attempt a new claim, so a winner has time to publish its replacement.
    if (attempt > 0) {
      const row = await getAccount(env.DB, account.row.user_id, account.row.id)
      if (!row) return account
      try {
        const credential = await decryptJson<StoredCredential>(env.TOKEN_ENCRYPTION_KEY, row.encrypted_payload)
        if (!needsRefresh(credential)) return { row, credential }
        if (row.refreshing_at !== null && row.refreshing_at !== undefined) {
          if (attempt < REFRESH_POLL_ATTEMPTS) await delay(REFRESH_POLL_DELAY_MS)
          continue
        }
      } catch {
        return account
      }
    }

    const lockToken = await acquireRefreshLock(env.DB, account.row.id)
    if (lockToken) {
      try {
        const row = await getAccount(env.DB, account.row.user_id, account.row.id)
        if (!row) {
          await releaseRefreshLock(env.DB, account.row.id, lockToken)
          return account
        }
        const credential = await decryptJson<StoredCredential>(env.TOKEN_ENCRYPTION_KEY, row.encrypted_payload)
        if (!needsRefresh(credential)) {
          await releaseRefreshLock(env.DB, row.id, lockToken)
          return { row, credential }
        }
        const refreshed = await refresh(credential)
        if (!refreshed) {
          await releaseRefreshLock(env.DB, row.id, lockToken)
          return { row, credential }
        }
        const encrypted = await encryptJson(env.TOKEN_ENCRYPTION_KEY, refreshed)
        const persisted = await writeRefreshedCredential(env.DB, row.id, lockToken, encrypted)
        return persisted ? { row: { ...row, encrypted_payload: encrypted, refreshing_at: null }, credential: refreshed } : account
      } catch {
        await releaseRefreshLock(env.DB, account.row.id, lockToken)
        return account
      }
    }

    if (attempt < REFRESH_POLL_ATTEMPTS) await delay(REFRESH_POLL_DELAY_MS)
  }

  // Lock stayed held and its credential remained expired: preserve availability.
  return account
}
