import { newId, nowIso } from "../utils/id"

export type ModelGroupRow = {
  id: string
  user_id: string
  /** Display name (free text, unique per user) — a label, never part of the URL. */
  name: string
  /**
   * The endpoint's URL id (`/g/<slug>/…`), unique per user, mutable
   * (docs/providers.md § Model groups). Added in `0014_group_endpoints.sql`.
   */
  slug: string
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

/**
 * One callable model of a group — 1..20 per group, name unique within the
 * group, each with its own ordered target list (docs/database.md
 * `model_group_models`). Added in `0014_group_endpoints.sql`.
 */
export type ModelGroupModelRow = {
  id: string
  user_id: string
  group_id: string
  name: string
  targets_json: string
  created_at: string
  updated_at: string
}

/**
 * Normalized target shape (docs/database.md `model_group_models.targets_json`):
 * `account_id` pins the target to one `upstream_accounts` row — `null` for
 * an unpinned (whole-pool) target. No FK; a deleted account just makes the
 * target skip at resolve time, mirroring the custom-provider convention.
 */
export type GroupTarget = { model: string; account_id: string | null }

/** Wire/storage shape of one group model: a callable name plus its targets. */
export type GroupModelInput = { name: string; targets: GroupTarget[] }

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
 * Resolves an endpoint slug to its group, scoped to `userId` — a slug must
 * never resolve cross-user (docs/api.md § Group endpoints). Exact match; the
 * `UNIQUE(user_id, slug)` constraint makes the hit unambiguous.
 */
export async function getGroupBySlug(
  db: D1Database,
  userId: string,
  slug: string,
): Promise<ModelGroupRow | null> {
  return (
    (await db
      .prepare(`SELECT * FROM model_groups WHERE user_id = ? AND slug = ?`)
      .bind(userId, slug)
      .first<ModelGroupRow>()) ?? null
  )
}

/**
 * A group's models. No `ORDER BY`: real SQLite (and `FakeD1`) both return an
 * unordered-clause `SELECT` in insertion order for a plain table scan, which
 * best preserves the order the caller submitted — the model list carries no
 * priority semantics (unlike each model's `targets`), so this is a display
 * nicety, not a correctness requirement.
 */
export async function listModelsForGroup(
  db: D1Database,
  groupId: string,
): Promise<ModelGroupModelRow[]> {
  const res = await db
    .prepare(`SELECT * FROM model_group_models WHERE group_id = ?`)
    .bind(groupId)
    .all<ModelGroupModelRow>()
  return res.results ?? []
}

/**
 * Resolves a request's `model` on a group endpoint: exact, case-sensitive
 * match against the group's model names (docs/providers.md § Model groups
 * "Resolution"). Indexed point lookup via `UNIQUE(group_id, name)` — never a
 * JSON scan.
 */
export async function getGroupModelByName(
  db: D1Database,
  groupId: string,
  name: string,
): Promise<ModelGroupModelRow | null> {
  return (
    (await db
      .prepare(`SELECT * FROM model_group_models WHERE group_id = ? AND name = ?`)
      .bind(groupId, name)
      .first<ModelGroupModelRow>()) ?? null
  )
}

/**
 * Atomic replace of a group's whole model set: delete the current rows and
 * insert the new list in one D1 batch (same batch-as-transaction convention
 * as `reorderCustomProviders`) — never a visible in-between state with zero
 * or partial models. Callers validate first (`validateGroupModels`); this
 * function trusts its input.
 */
export async function replaceGroupModels(
  db: D1Database,
  input: { userId: string; groupId: string; models: GroupModelInput[] },
): Promise<void> {
  const ts = nowIso()
  const statements = [
    db.prepare(`DELETE FROM model_group_models WHERE group_id = ?`).bind(input.groupId),
    ...input.models.map((model) =>
      db
        .prepare(
          `INSERT INTO model_group_models (id, user_id, group_id, name, targets_json, created_at, updated_at)
           VALUES (?, ?, ?, ?, ?, ?, ?)`,
        )
        .bind(
          newId("mgmodel"),
          input.userId,
          input.groupId,
          model.name,
          JSON.stringify(model.targets),
          ts,
          ts,
        ),
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
  input: { userId: string; name: string; slug: string; strategy?: string },
): Promise<ModelGroupRow> {
  const id = newId("mgrp")
  const ts = nowIso()
  const strategy = input.strategy ?? "ordered"
  await db
    .prepare(
      `INSERT INTO model_groups (id, user_id, name, slug, strategy, created_at, updated_at)
       VALUES (?, ?, ?, ?, ?, ?, ?)`,
    )
    .bind(id, input.userId, input.name, input.slug, strategy, ts, ts)
    .run()
  return {
    id,
    user_id: input.userId,
    name: input.name,
    slug: input.slug,
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
  patch: { name?: string; slug?: string; strategy?: string },
): Promise<void> {
  const ts = nowIso()
  await db
    .prepare(
      `UPDATE model_groups SET
         name = COALESCE(?, name),
         slug = COALESCE(?, slug),
         strategy = COALESCE(?, strategy),
         updated_at = ?
       WHERE id = ?`,
    )
    .bind(patch.name ?? null, patch.slug ?? null, patch.strategy ?? null, ts, id)
    .run()
}

/**
 * Deletes the group and its model rows in one batch. The schema's
 * `ON DELETE CASCADE` would cover the child rows on real D1, but the explicit
 * delete keeps the behavior identical under `FakeD1` (no FK enforcement) and
 * costs one statement in the same transaction.
 */
export async function deleteModelGroup(db: D1Database, userId: string, id: string): Promise<boolean> {
  const existing = await getModelGroupById(db, userId, id)
  if (!existing) return false
  await db.batch([
    db.prepare(`DELETE FROM model_group_models WHERE group_id = ?`).bind(id),
    db.prepare(`DELETE FROM model_groups WHERE id = ? AND user_id = ?`).bind(id, userId),
  ])
  return true
}

/**
 * Tolerant parse, normalizing every entry to `{model, account_id}`:
 * - A plain `"provider/model"` string (still-accepted wire shorthand) is
 *   `{model: entry, account_id: null}`.
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
