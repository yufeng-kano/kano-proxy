import { encryptJson } from "../crypto/token_crypto"
import { updateAccountPayload, type AccountRow } from "../db/accounts"
import type { Env } from "../env"

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
 * Account/target selection (list, acquire, bench-and-exclude) moved to
 * `routing/` (docs/providers.md § Routing module) — the routing module owns
 * the flat candidate walk that used to live here as a separate per-pool
 * acquire loop, plus in `providers/resolve.ts`'s group-target walk. What's
 * left here is credential persistence, shared by every adapter's OAuth
 * refresh path.
 */
export async function saveCredential(
  env: Env,
  accountId: string,
  credential: StoredCredential,
  meta?: { label?: string | null; accountMetaJson?: string | null },
): Promise<void> {
  const blob = await encryptJson(env.TOKEN_ENCRYPTION_KEY, credential)
  await updateAccountPayload(env.DB, accountId, blob, meta)
}
