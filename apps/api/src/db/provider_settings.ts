import { DEFAULT_STRATEGY } from "../routing/strategy"
import { nowIso } from "../utils/id"

export type ProviderSettingsRow = {
  user_id: string
  provider: string
  strategy: string
  updated_at: string
}

/**
 * Pool-level routing strategy for one (user, provider) — the direct-call
 * counterpart of `model_groups.strategy` (docs/database.md
 * `provider_settings`). `provider` is a builtin id or a custom provider's
 * slug; a row for a deleted slug is simply never read again, mirroring how
 * `upstream_accounts` rows for a removed custom provider are handled.
 *
 * A missing row means `ordered` — rows are created lazily on first write
 * (`setProviderStrategy`), never backfilled.
 */
export async function getProviderStrategy(
  db: D1Database,
  userId: string,
  provider: string,
): Promise<string> {
  const row = await db
    .prepare(`SELECT * FROM provider_settings WHERE user_id = ? AND provider = ?`)
    .bind(userId, provider)
    .first<ProviderSettingsRow>()
  return row?.strategy ?? DEFAULT_STRATEGY
}

/**
 * Upsert as two statements rather than `ON CONFLICT` — D1/SQLite supports
 * it, but this codebase's other single-row-per-key tables (e.g. accounts)
 * never needed it and the `FakeD1` test double has no INSERT-OR-UPDATE
 * shape; try the UPDATE first (its `meta.changes` says whether a row
 * existed) and INSERT only when it didn't. No transaction spans the two:
 * a same-key race would at worst run both, and the loser's UPDATE (or the
 * unique PK's natural last-write-wins in real D1) still lands on a
 * consistent single row — this endpoint is a low-frequency admin PATCH, not
 * a hot path.
 */
export async function setProviderStrategy(
  db: D1Database,
  userId: string,
  provider: string,
  strategy: string,
): Promise<void> {
  const ts = nowIso()
  const updated = await db
    .prepare(`UPDATE provider_settings SET strategy = ?, updated_at = ? WHERE user_id = ? AND provider = ?`)
    .bind(strategy, ts, userId, provider)
    .run()
  if ((updated.meta.changes ?? 0) > 0) return
  await db
    .prepare(
      `INSERT INTO provider_settings (user_id, provider, strategy, updated_at) VALUES (?, ?, ?, ?)`,
    )
    .bind(userId, provider, strategy, ts)
    .run()
}
