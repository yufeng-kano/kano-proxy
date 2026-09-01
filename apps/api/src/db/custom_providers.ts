import { newId, nowIso } from "../utils/id"

export type CustomProviderRow = {
  id: string
  user_id: string
  slug: string
  name: string
  format: "openai" | "anthropic"
  base_url: string
  /** Complete Anthropic-shaped `/v1/messages/count_tokens` URL, openai format only. Nullable. */
  count_tokens_url: string | null
  models_mode: "auto" | "manual"
  manual_models_json: string | null
  sort_order: number
  created_at: string
  updated_at: string
}

export async function listCustomProviders(
  db: D1Database,
  userId: string,
): Promise<CustomProviderRow[]> {
  const res = await db
    .prepare(
      `SELECT * FROM custom_providers
       WHERE user_id = ?
       ORDER BY sort_order ASC, created_at ASC`,
    )
    .bind(userId)
    .all<CustomProviderRow>()
  return res.results ?? []
}

export async function getCustomProviderById(
  db: D1Database,
  userId: string,
  id: string,
): Promise<CustomProviderRow | null> {
  return (
    (await db
      .prepare(`SELECT * FROM custom_providers WHERE id = ? AND user_id = ?`)
      .bind(id, userId)
      .first<CustomProviderRow>()) ?? null
  )
}

/** Scoped to userId — a custom slug must never resolve cross-user. */
export async function getCustomProviderBySlug(
  db: D1Database,
  userId: string,
  slug: string,
): Promise<CustomProviderRow | null> {
  return (
    (await db
      .prepare(`SELECT * FROM custom_providers WHERE user_id = ? AND slug = ?`)
      .bind(userId, slug)
      .first<CustomProviderRow>()) ?? null
  )
}

export async function countCustomProviders(db: D1Database, userId: string): Promise<number> {
  const row = await db
    .prepare(`SELECT COUNT(*) as c FROM custom_providers WHERE user_id = ?`)
    .bind(userId)
    .first<{ c: number }>()
  return row?.c ?? 0
}

/**
 * Slug namespace shared with cli_providers (docs/cli.md § Data model): the
 * cross-table guard rides inside the INSERT so it cannot race a concurrent
 * CLI create for the same slug. Returns null on that conflict.
 */
export async function insertCustomProvider(
  db: D1Database,
  input: {
    userId: string
    slug: string
    name: string
    format: "openai" | "anthropic"
    baseUrl: string
    countTokensUrl: string | null
    modelsMode: "auto" | "manual"
    manualModelsJson: string | null
  },
): Promise<CustomProviderRow | null> {
  const id = newId("cprov")
  const ts = nowIso()
  const count = await db
    .prepare(`SELECT COUNT(*) as c FROM custom_providers WHERE user_id = ?`)
    .bind(input.userId)
    .first<{ c: number }>()
  const max = await db
    .prepare(`SELECT COALESCE(MAX(sort_order), 0) as m FROM custom_providers WHERE user_id = ?`)
    .bind(input.userId)
    .first<{ m: number }>()
  const sortOrder = count?.c ? Math.max(max?.m ?? 0, count.c - 1) + 1 : 0
  const res = await db
    .prepare(
      `INSERT INTO custom_providers
       (id, user_id, slug, name, format, base_url, count_tokens_url, models_mode, manual_models_json, sort_order, created_at, updated_at)
       SELECT ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?
       WHERE NOT EXISTS (SELECT 1 FROM cli_providers WHERE user_id = ? AND slug = ?)`,
    )
    .bind(
      id,
      input.userId,
      input.slug,
      input.name,
      input.format,
      input.baseUrl,
      input.countTokensUrl,
      input.modelsMode,
      input.manualModelsJson,
      sortOrder,
      ts,
      ts,
      input.userId,
      input.slug,
    )
    .run()
  if ((res.meta.changes ?? 0) === 0) return null
  return {
    id,
    user_id: input.userId,
    slug: input.slug,
    name: input.name,
    format: input.format,
    base_url: input.baseUrl,
    count_tokens_url: input.countTokensUrl,
    models_mode: input.modelsMode,
    manual_models_json: input.manualModelsJson,
    sort_order: sortOrder,
    created_at: ts,
    updated_at: ts,
  }
}

/** Renumber a user's complete custom-provider list in one D1 batch transaction. */
export async function reorderCustomProviders(
  db: D1Database,
  userId: string,
  orderedIds: string[],
): Promise<void> {
  const statements = orderedIds.map((id, sortOrder) =>
    db
      .prepare(
        `UPDATE custom_providers
         SET sort_order = ?, updated_at = ?
         WHERE id = ? AND user_id = ?`,
      )
      .bind(sortOrder, nowIso(), id, userId),
  )
  await db.batch(statements)
}

/**
 * Partial update — an omitted field (`undefined`, bound as SQL NULL) keeps
 * its stored value via COALESCE, same convention as `updateAccountIdentity`.
 * To clear manual_models_json, pass `"[]"` (a real value), not `undefined`.
 *
 * `countTokensUrl` is nullable *and* clearable, which the COALESCE convention
 * above can't express on its own (a bound `null` would just mean "omitted,
 * keep the stored value" — indistinguishable from an actual clear-to-NULL).
 * So it gets its own statement, run only when the caller actually sent the
 * field (`!== undefined`): a direct `= ?` assignment, where `null` really
 * does write NULL and a string really does write that string.
 */
export async function updateCustomProviderFields(
  db: D1Database,
  id: string,
  patch: {
    name?: string
    baseUrl?: string
    countTokensUrl?: string | null
    modelsMode?: "auto" | "manual"
    manualModelsJson?: string | null
  },
): Promise<void> {
  const ts = nowIso()
  await db
    .prepare(
      `UPDATE custom_providers SET
         name = COALESCE(?, name),
         base_url = COALESCE(?, base_url),
         models_mode = COALESCE(?, models_mode),
         manual_models_json = COALESCE(?, manual_models_json),
         updated_at = ?
       WHERE id = ?`,
    )
    .bind(
      patch.name ?? null,
      patch.baseUrl ?? null,
      patch.modelsMode ?? null,
      patch.manualModelsJson ?? null,
      ts,
      id,
    )
    .run()

  if (patch.countTokensUrl !== undefined) {
    await db
      .prepare(`UPDATE custom_providers SET count_tokens_url = ?, updated_at = ? WHERE id = ?`)
      .bind(patch.countTokensUrl, ts, id)
      .run()
  }
}

export async function deleteCustomProvider(
  db: D1Database,
  userId: string,
  id: string,
): Promise<boolean> {
  const r = await db
    .prepare(`DELETE FROM custom_providers WHERE id = ? AND user_id = ?`)
    .bind(id, userId)
    .run()
  return (r.meta.changes ?? 0) > 0
}
