import { newId, nowIso } from "../utils/id"

export type ModelGroupRow = {
  id: string
  user_id: string
  name: string
  targets_json: string
  created_at: string
  updated_at: string
}

/**
 * Normalized target shape (docs/database.md `model_groups.targets_json`):
 * `account_id` pins the target to one `upstream_accounts` row — `null` for
 * an unpinned (whole-pool) target. No FK; a deleted account just makes the
 * target skip at resolve time, mirroring the custom-provider convention.
 */
export type GroupTarget = { model: string; account_id: string | null }

export async function listModelGroups(db: D1Database, userId: string): Promise<ModelGroupRow[]> {
  const res = await db
    .prepare(`SELECT * FROM model_groups WHERE user_id = ? ORDER BY created_at ASC`)
    .bind(userId)
    .all<ModelGroupRow>()
  return res.results ?? []
}

export async function getModelGroupById(
  db: D1Database,
  userId: string,
  id: string,
): Promise<ModelGroupRow | null> {
  return (
    (await db
      .prepare(`SELECT * FROM model_groups WHERE id = ? AND user_id = ?`)
      .bind(id, userId)
      .first<ModelGroupRow>()) ?? null
  )
}

/** Scoped to userId — a group name must never resolve cross-user. Exact, case-sensitive match. */
export async function getModelGroupByName(
  db: D1Database,
  userId: string,
  name: string,
): Promise<ModelGroupRow | null> {
  return (
    (await db
      .prepare(`SELECT * FROM model_groups WHERE user_id = ? AND name = ?`)
      .bind(userId, name)
      .first<ModelGroupRow>()) ?? null
  )
}

export async function countModelGroups(db: D1Database, userId: string): Promise<number> {
  const row = await db
    .prepare(`SELECT COUNT(*) as c FROM model_groups WHERE user_id = ?`)
    .bind(userId)
    .first<{ c: number }>()
  return row?.c ?? 0
}

export async function insertModelGroup(
  db: D1Database,
  input: { userId: string; name: string; targets: GroupTarget[] },
): Promise<ModelGroupRow> {
  const id = newId("mgrp")
  const ts = nowIso()
  const targetsJson = JSON.stringify(input.targets)
  await db
    .prepare(
      `INSERT INTO model_groups (id, user_id, name, targets_json, created_at, updated_at)
       VALUES (?, ?, ?, ?, ?, ?)`,
    )
    .bind(id, input.userId, input.name, targetsJson, ts, ts)
    .run()
  return {
    id,
    user_id: input.userId,
    name: input.name,
    targets_json: targetsJson,
    created_at: ts,
    updated_at: ts,
  }
}

/**
 * Partial update — an omitted field (`undefined`, bound as SQL NULL) keeps
 * its stored value via COALESCE, same convention as `updateCustomProviderFields`.
 */
export async function updateModelGroupFields(
  db: D1Database,
  id: string,
  patch: { name?: string; targets?: GroupTarget[] },
): Promise<void> {
  const ts = nowIso()
  await db
    .prepare(
      `UPDATE model_groups SET
         name = COALESCE(?, name),
         targets_json = COALESCE(?, targets_json),
         updated_at = ?
       WHERE id = ?`,
    )
    .bind(patch.name ?? null, patch.targets ? JSON.stringify(patch.targets) : null, ts, id)
    .run()
}

export async function deleteModelGroup(db: D1Database, userId: string, id: string): Promise<boolean> {
  const r = await db
    .prepare(`DELETE FROM model_groups WHERE id = ? AND user_id = ?`)
    .bind(id, userId)
    .run()
  return (r.meta.changes ?? 0) > 0
}

/**
 * Tolerant parse, normalizing every entry to `{model, account_id}`:
 * - A plain `"provider/model"` string (v3.0.0 rows, and still-accepted wire
 *   shorthand) is `{model: entry, account_id: null}`.
 * - An object entry reads `model` (string) and optional `account_id`
 *   (string; anything else, including `null`/absent, normalizes to `null`).
 *   Future per-target fields (balancing weights) are simply ignored here,
 *   not stripped from storage — this parser only ever reads, never rewrites
 *   `targets_json`.
 * Anything else (non-string `model`, or an entry that's neither a string nor
 * an object) is dropped rather than throwing, so a malformed row degrades to
 * fewer targets instead of a 500.
 */
export function parseGroupTargets(json: string | null): GroupTarget[] {
  if (!json) return []
  try {
    const arr = JSON.parse(json) as unknown
    if (!Array.isArray(arr)) return []
    const out: GroupTarget[] = []
    for (const entry of arr) {
      if (typeof entry === "string") {
        out.push({ model: entry, account_id: null })
        continue
      }
      if (entry && typeof entry === "object" && typeof (entry as { model?: unknown }).model === "string") {
        const obj = entry as { model: string; account_id?: unknown }
        const accountId = typeof obj.account_id === "string" && obj.account_id ? obj.account_id : null
        out.push({ model: obj.model, account_id: accountId })
      }
    }
    return out
  } catch {
    return []
  }
}
