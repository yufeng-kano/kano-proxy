import { decryptJson, encryptJson } from "../crypto/token_crypto"
import { listAccounts, updateAccountPayload, type AccountRow } from "../db/accounts"
import type { Env, ProviderId } from "../env"
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

const FAILOVER_STATUS = new Set([401, 403, 429])

export function shouldBenchStatus(status: number): boolean {
  return FAILOVER_STATUS.has(status)
}

export async function listUsableAccounts(
  env: Env,
  userId: string,
  provider: ProviderId,
  exclude: Set<string> = new Set(),
): Promise<AccountRow[]> {
  const rows = await listAccounts(env.DB, userId, provider)
  const out: AccountRow[] = []
  for (const row of rows) {
    if (exclude.has(row.id)) continue
    if (await isBenched(env, userId, provider, row.id)) continue
    out.push(row)
  }
  return out
}

export async function acquireAccount(
  env: Env,
  userId: string,
  provider: ProviderId,
  exclude: Set<string> = new Set(),
): Promise<AcquiredAccount | null> {
  const candidates = await listUsableAccounts(env, userId, provider, exclude)
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
  provider: ProviderId,
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
