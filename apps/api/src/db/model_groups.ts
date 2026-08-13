import { newId, nowIso } from "../utils/id"

export type ModelGroupRow = {
  id: string
  user_id: string
  /** Display name (free text, unique per user) since `0009_model_group_aliases.sql` — the callable ids live in `model_group_aliases`. */
  name: string
  targets_json: string
  /**
   * `NOT NULL DEFAULT 'ordered'` since `0010_routing_strategy.sql`
   * (docs/providers.md § Routing module). Raw column value — an
   * unrecognized value (a future strategy this deploy predates) is
   * normalized to `ordered` by `routing/strategy.ts`, not here; this layer
   * just stores and returns what's in the row.
   */
  strategy: string
  created_at: string
  updated_at: string
}

/** One callable bare id for a group — 1..10 per group, unique per user across all of that user's groups (docs/database.md `model_group_aliases`). */
export type ModelGroupAliasRow = {
  id: string
  user_id: string
  group_id: string
  alias: string
  created_at: string
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

/**
 * A group's current aliases. No `ORDER BY`: real SQLite (and `FakeD1`) both
 * return an unordered-clause `SELECT` in insertion order for a plain table
 * scan, which best preserves the order the caller submitted — aliases carry
 * no priority semantics (unlike `targets`) so this is a display nicety, not
 * a correctness requirement.
 */
export async function listAliasesForGroup(
  db: D1Database,
  groupId: string,
): Promise<ModelGroupAliasRow[]> {
  const res = await db
    .prepare(`SELECT * FROM model_group_aliases WHERE group_id = ?`)
    .bind(groupId)
    .all<ModelGroupAliasRow>()
  return res.results ?? []
}

/**
 * Resolves an alias to its group, scoped to `userId` — an alias must never
 * resolve cross-user. Exact, case-sensitive match (docs/providers.md §
 * Model groups "Aliases"). Two queries rather than a join: `FakeD1` has no
 * join support, and this is a two-row point lookup either way.
 */
export async function getGroupByAlias(
  db: D1Database,
  userId: string,
  alias: string,
): Promise<ModelGroupRow | null> {
  const aliasRow = await db
    .prepare(`SELECT * FROM model_group_aliases WHERE user_id = ? AND alias = ?`)
    .bind(userId, alias)
    .first<ModelGroupAliasRow>()
  if (!aliasRow) return null
  return getModelGroupById(db, userId, aliasRow.group_id)
}

/**
 * The subset of `aliases` already owned by a *different* group of this user
 * — a `400`-worthy cross-group conflict (docs/auth.md § Model groups).
 * `excludeGroupId` lets an update check against every other group without
 * false-positiving on the group's own current aliases (which, on a replace,
 * are about to be deleted and reinserted anyway). One query per candidate
 * alias — bounded by `MAX_ALIASES_PER_GROUP` (≤ 10), a write-path cost, not
 * a hot read path.
 */
export async function findAliasConflicts(
  db: D1Database,
  userId: string,
  aliases: string[],
  excludeGroupId?: string,
): Promise<string[]> {
  const conflicts: string[] = []
  for (const alias of aliases) {
    const row = await db
      .prepare(`SELECT * FROM model_group_aliases WHERE user_id = ? AND alias = ?`)
      .bind(userId, alias)
      .first<ModelGroupAliasRow>()
    if (row && row.group_id !== excludeGroupId) conflicts.push(alias)
  }
  return conflicts
}

/**
 * Atomic replace: delete the group's current aliases and insert the new
 * ordered list in one D1 batch (same batch-as-transaction convention as
 * `reorderCustomProviders`) — never a visible in-between state with zero or
 * partial aliases. Callers validate + resolve cross-group conflicts first
 * (`findAliasConflicts`); this function trusts its input.
 */
export async function replaceAliases(
  db: D1Database,
  input: { userId: string; groupId: string; aliases: string[] },
): Promise<void> {
  const ts = nowIso()
  const statements = [
    db.prepare(`DELETE FROM model_group_aliases WHERE group_id = ?`).bind(input.groupId),
    ...input.aliases.map((alias) =>
      db
        .prepare(
          `INSERT INTO model_group_aliases (id, user_id, group_id, alias, created_at)
           VALUES (?, ?, ?, ?, ?)`,
        )
        .bind(newId("mgalias"), input.userId, input.groupId, alias, ts),
    ),
  ]
  await db.batch(statements)
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
  input: { userId: string; name: string; targets: GroupTarget[]; strategy?: string },
): Promise<ModelGroupRow> {
  const id = newId("mgrp")
  const ts = nowIso()
  const targetsJson = JSON.stringify(input.targets)
  const strategy = input.strategy ?? "ordered"
  await db
    .prepare(
      `INSERT INTO model_groups (id, user_id, name, targets_json, strategy, created_at, updated_at)
       VALUES (?, ?, ?, ?, ?, ?, ?)`,
    )
    .bind(id, input.userId, input.name, targetsJson, strategy, ts, ts)
    .run()
  return {
    id,
    user_id: input.userId,
    name: input.name,
    targets_json: targetsJson,
    strategy,
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
  patch: { name?: string; targets?: GroupTarget[]; strategy?: string },
): Promise<void> {
  const ts = nowIso()
  await db
    .prepare(
      `UPDATE model_groups SET
         name = COALESCE(?, name),
         targets_json = COALESCE(?, targets_json),
         strategy = COALESCE(?, strategy),
         updated_at = ?
       WHERE id = ?`,
    )
    .bind(patch.name ?? null, patch.targets ? JSON.stringify(patch.targets) : null, patch.strategy ?? null, ts, id)
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
