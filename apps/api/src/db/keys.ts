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
}

export async function listKeys(db: D1Database, userId: string): Promise<ApiKeyRow[]> {
  const res = await db
    .prepare(
      `SELECT id, user_id, name, key_prefix, key_hash, created_at, last_used_at
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
): Promise<{ row: ApiKeyRow; plaintext: string }> {
  const { plaintext, prefix, hash } = await createApiKeyMaterial()
  const id = newId("key")
  const ts = nowIso()
  await db
    .prepare(
      `INSERT INTO api_keys (id, user_id, name, key_prefix, key_hash, created_at, last_used_at)
       VALUES (?, ?, ?, ?, ?, ?, NULL)`,
    )
    .bind(id, userId, name, prefix, hash, ts)
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
    },
  }
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
