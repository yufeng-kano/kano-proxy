import { createApiKeyMaterial } from "../crypto/keys"
import { newId, nowIso } from "../utils/id"

export type ApiKeyRow = {
  id: string
  user_id: string
  name: string
  key_prefix: string
  key_hash: string
  created_at: string
  last_used_at: string | null
  /** USD ceiling per window; NULL = unlimited (docs/pricing.md). */
  spend_limit: number | null
  spend_limit_interval: string
  /** 0/1 — whether builtin (subscription OAuth) traffic counts toward the limit. */
  spend_limit_include_oauth: number
}

export type SpendLimitFields = {
  spendLimit: number | null
  spendLimitInterval: string
  spendLimitIncludeOauth: boolean
}

export async function listKeys(db: D1Database, userId: string): Promise<ApiKeyRow[]> {
  const res = await db
    .prepare(
      `SELECT id, user_id, name, key_prefix, key_hash, created_at, last_used_at,
              spend_limit, spend_limit_interval, spend_limit_include_oauth
       FROM api_keys WHERE user_id = ? ORDER BY created_at DESC`,
    )
    .bind(userId)
    .all<ApiKeyRow>()
  return res.results ?? []
}

export async function createKey(
  db: D1Database,
  userId: string,
  name: string,
  limits?: SpendLimitFields,
): Promise<{ row: ApiKeyRow; plaintext: string }> {
  const { plaintext, prefix, hash } = await createApiKeyMaterial()
  const id = newId("key")
  const ts = nowIso()
  const spendLimit = limits?.spendLimit ?? null
  const interval = limits?.spendLimitInterval ?? "monthly"
  const includeOauth = limits ? (limits.spendLimitIncludeOauth ? 1 : 0) : 1
  await db
    .prepare(
      `INSERT INTO api_keys (id, user_id, name, key_prefix, key_hash, created_at, last_used_at,
                             spend_limit, spend_limit_interval, spend_limit_include_oauth)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    )
    .bind(id, userId, name, prefix, hash, ts, null, spendLimit, interval, includeOauth)
    .run()
  return {
    plaintext,
    row: {
      id,
      user_id: userId,
      name,
      key_prefix: prefix,
      key_hash: hash,
      created_at: ts,
      last_used_at: null,
      spend_limit: spendLimit,
      spend_limit_interval: interval,
      spend_limit_include_oauth: includeOauth,
    },
  }
}

/**
 * Rename and/or replace the limit fields. Unlike the COALESCE convention
 * elsewhere, the limit fields are set verbatim when `limits` is given —
 * `spendLimit: null` must *clear* a limit, which COALESCE cannot express.
 */
export async function updateKey(
  db: D1Database,
  userId: string,
  keyId: string,
  patch: { name?: string; limits?: SpendLimitFields },
): Promise<boolean> {
  const existing = await db
    .prepare(`SELECT id FROM api_keys WHERE id = ? AND user_id = ?`)
    .bind(keyId, userId)
    .first<{ id: string }>()
  if (!existing) return false

  if (patch.name !== undefined) {
    await db
      .prepare(`UPDATE api_keys SET name = ? WHERE id = ?`)
      .bind(patch.name, keyId)
      .run()
  }
  if (patch.limits !== undefined) {
    await db
      .prepare(
        `UPDATE api_keys SET spend_limit = ?, spend_limit_interval = ?, spend_limit_include_oauth = ? WHERE id = ?`,
      )
      .bind(
        patch.limits.spendLimit,
        patch.limits.spendLimitInterval,
        patch.limits.spendLimitIncludeOauth ? 1 : 0,
        keyId,
      )
      .run()
  }
  return true
}

export async function deleteKey(db: D1Database, userId: string, keyId: string): Promise<boolean> {
  const r = await db
    .prepare(`DELETE FROM api_keys WHERE id = ? AND user_id = ?`)
    .bind(keyId, userId)
    .run()
  return (r.meta.changes ?? 0) > 0
}

export async function findKeyByHash(
  db: D1Database,
  hash: string,
): Promise<ApiKeyRow | null> {
  return (
    (await db
      .prepare(`SELECT * FROM api_keys WHERE key_hash = ?`)
      .bind(hash)
      .first<ApiKeyRow>()) ?? null
  )
}

export async function touchKey(db: D1Database, keyId: string): Promise<void> {
  await db
    .prepare(`UPDATE api_keys SET last_used_at = ? WHERE id = ?`)
    .bind(nowIso(), keyId)
    .run()
}
