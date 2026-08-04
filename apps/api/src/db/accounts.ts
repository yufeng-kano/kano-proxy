import { newId, nowIso } from "../utils/id"

export type AccountRow = {
  id: string
  user_id: string
  provider: string
  external_account_id: string | null
  label: string | null
  custom_label: string | null
  priority: number
  encrypted_payload: string
  account_meta_json: string | null
  usage_snapshot_json: string | null
  usage_fetched_at: string | null
  usage_fetching_at: string | null
  created_at: string
  updated_at: string
}

/**
 * `provider` is a builtin `ProviderId` or a custom provider's slug — this
 * layer only ever interpolates it into SQL, so it is typed as `string` to
 * serve both without duplicating these queries per kind.
 */
export async function listAccounts(
  db: D1Database,
  userId: string,
  provider: string,
): Promise<AccountRow[]> {
  const res = await db
    .prepare(
      `SELECT * FROM upstream_accounts
       WHERE user_id = ? AND provider = ?
       ORDER BY priority DESC, created_at DESC`,
    )
    .bind(userId, provider)
    .all<AccountRow>()
  return res.results ?? []
}

export async function getAccount(
  db: D1Database,
  userId: string,
  accountId: string,
): Promise<AccountRow | null> {
  return (
    (await db
      .prepare(`SELECT * FROM upstream_accounts WHERE id = ? AND user_id = ?`)
      .bind(accountId, userId)
      .first<AccountRow>()) ?? null
  )
}

/** Set or clear the user-owned display name without changing upstream identity. */
export async function setAccountCustomLabel(
  db: D1Database,
  userId: string,
  accountId: string,
  customLabel: string | null,
): Promise<boolean> {
  // Unlike updateAccountIdentity's COALESCE convention, null here is an
  // explicit clear and must be written as a literal NULL.
  const result = await db
    .prepare(
      `UPDATE upstream_accounts SET custom_label = ?, updated_at = ? WHERE id = ? AND user_id = ?`,
    )
    .bind(customLabel, nowIso(), accountId, userId)
    .run()
  return (result.meta.changes ?? 0) > 0
}

export async function insertAccount(
  db: D1Database,
  input: {
    userId: string
    provider: string
    encryptedPayload: string
    label?: string | null
    externalAccountId?: string | null
    accountMetaJson?: string | null
    priority?: number
  },
): Promise<AccountRow> {
  const id = newId("acc")
  const ts = nowIso()
  // New accounts get highest priority
  const max = await db
    .prepare(
      `SELECT COALESCE(MAX(priority), 0) as m FROM upstream_accounts WHERE user_id = ? AND provider = ?`,
    )
    .bind(input.userId, input.provider)
    .first<{ m: number }>()
  const priority = input.priority ?? (max?.m ?? 0) + 1
  await db
    .prepare(
      `INSERT INTO upstream_accounts
       (id, user_id, provider, external_account_id, label, priority, encrypted_payload, account_meta_json, created_at, updated_at)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    )
    .bind(
      id,
      input.userId,
      input.provider,
      input.externalAccountId ?? null,
      input.label ?? null,
      priority,
      input.encryptedPayload,
      input.accountMetaJson ?? null,
      ts,
      ts,
    )
    .run()
  return {
    id,
    user_id: input.userId,
    provider: input.provider,
    external_account_id: input.externalAccountId ?? null,
    label: input.label ?? null,
    custom_label: null,
    priority,
    encrypted_payload: input.encryptedPayload,
    account_meta_json: input.accountMetaJson ?? null,
    usage_snapshot_json: null,
    usage_fetched_at: null,
    usage_fetching_at: null,
    created_at: ts,
    updated_at: ts,
  }
}

export async function updateAccountPayload(
  db: D1Database,
  accountId: string,
  encryptedPayload: string,
  meta?: { label?: string | null; accountMetaJson?: string | null },
): Promise<void> {
  const ts = nowIso()
  if (meta) {
    await db
      .prepare(
        `UPDATE upstream_accounts SET encrypted_payload = ?, label = COALESCE(?, label),
         account_meta_json = COALESCE(?, account_meta_json), updated_at = ? WHERE id = ?`,
      )
      .bind(
        encryptedPayload,
        meta.label ?? null,
        meta.accountMetaJson ?? null,
        ts,
        accountId,
      )
      .run()
  } else {
    await db
      .prepare(`UPDATE upstream_accounts SET encrypted_payload = ?, updated_at = ? WHERE id = ?`)
      .bind(encryptedPayload, ts, accountId)
      .run()
  }
}

/**
 * Cached usage read for one account (docs/providers.md § Usage cache).
 *
 * `error` / `stale` / `edgeBlocked` are stored alongside the windows because
 * the route derives `status: "unusable"` from all three — a cache hit that
 * dropped them would silently flip an unusable account back to active.
 */
export type UsageSnapshot = {
  windows: unknown[]
  account?: Record<string, unknown>
  error: string | null
  stale: boolean
  edgeBlocked: boolean
}

/** Server-side usage TTL: within this, a read never touches upstream. */
export const USAGE_TTL_MS = 60_000

/**
 * How long a lock may be held before another caller may break it. Bounds the
 * damage from a Worker that died mid-fetch or an upstream that hung, at the
 * cost of allowing a second fetch past that point.
 */
const USAGE_LOCK_TTL_MS = 30_000

export function isUsageFresh(row: AccountRow, now: number = Date.now()): boolean {
  if (!row.usage_fetched_at || !row.usage_snapshot_json) return false
  const at = Date.parse(row.usage_fetched_at)
  return Number.isFinite(at) && now - at < USAGE_TTL_MS
}

export function readUsageSnapshot(row: AccountRow): UsageSnapshot | null {
  if (!row.usage_snapshot_json) return null
  try {
    const parsed = JSON.parse(row.usage_snapshot_json) as Partial<UsageSnapshot>
    if (!Array.isArray(parsed.windows)) return null
    return {
      windows: parsed.windows,
      account: parsed.account,
      error: parsed.error ?? null,
      stale: !!parsed.stale,
      edgeBlocked: !!parsed.edgeBlocked,
    }
  } catch {
    // A malformed blob reads as a miss, never as trusted data.
    return null
  }
}

/**
 * Single-flight lock acquire, as a compare-and-swap.
 *
 * D1 has no cross-request transactions, so this leans on SQLite's
 * single-statement atomicity: the WHERE decides the winner and `meta.changes`
 * reports it. Returns the token to release with, or `null` when another caller
 * already holds a fresh lock.
 */
export async function acquireUsageLock(
  db: D1Database,
  accountId: string,
): Promise<string | null> {
  const token = nowIso()
  const breakBefore = new Date(Date.now() - USAGE_LOCK_TTL_MS).toISOString()
  const res = await db
    .prepare(
      `UPDATE upstream_accounts
       SET usage_fetching_at = ?
       WHERE id = ? AND (usage_fetching_at IS NULL OR usage_fetching_at < ?)`,
    )
    .bind(token, accountId, breakBefore)
    .run()
  return (res.meta.changes ?? 0) > 0 ? token : null
}

/**
 * Write the snapshot and release the lock in one statement.
 *
 * The release is conditional on still holding `token` — an unconditional
 * release is a real bug: once the stale-lock breaker has handed the lock to a
 * second caller, a late release from the first would free the *second*
 * caller's lock and let a third fetch start concurrently.
 */
export async function writeUsageSnapshot(
  db: D1Database,
  accountId: string,
  token: string,
  snapshot: UsageSnapshot,
): Promise<void> {
  await db
    .prepare(
      `UPDATE upstream_accounts
       SET usage_snapshot_json = ?, usage_fetched_at = ?, usage_fetching_at = NULL
       WHERE id = ? AND usage_fetching_at = ?`,
    )
    .bind(JSON.stringify(snapshot), nowIso(), accountId, token)
    .run()
}

/**
 * Release without writing — the upstream call failed. The previous snapshot is
 * deliberately left intact so one hiccup does not blank the usage bars; the
 * error travels in the response instead.
 */
export async function releaseUsageLock(
  db: D1Database,
  accountId: string,
  token: string,
): Promise<void> {
  await db
    .prepare(
      `UPDATE upstream_accounts
       SET usage_fetching_at = NULL
       WHERE id = ? AND usage_fetching_at = ?`,
    )
    .bind(accountId, token)
    .run()
}

/** Persist display label / meta without touching secrets. */
export async function updateAccountIdentity(
  db: D1Database,
  accountId: string,
  identity: { label?: string | null; accountMetaJson?: string | null },
): Promise<void> {
  await db
    .prepare(
      `UPDATE upstream_accounts
       SET label = COALESCE(?, label),
           account_meta_json = COALESCE(?, account_meta_json),
           updated_at = ?
       WHERE id = ?`,
    )
    .bind(identity.label ?? null, identity.accountMetaJson ?? null, nowIso(), accountId)
    .run()
}

export async function promoteAccount(
  db: D1Database,
  userId: string,
  accountId: string,
): Promise<boolean> {
  const row = await getAccount(db, userId, accountId)
  if (!row) return false
  const max = await db
    .prepare(
      `SELECT COALESCE(MAX(priority), 0) as m FROM upstream_accounts WHERE user_id = ? AND provider = ?`,
    )
    .bind(userId, row.provider)
    .first<{ m: number }>()
  const priority = (max?.m ?? 0) + 1
  await db
    .prepare(`UPDATE upstream_accounts SET priority = ?, updated_at = ? WHERE id = ?`)
    .bind(priority, nowIso(), accountId)
    .run()
  return true
}

export async function removeAccount(
  db: D1Database,
  userId: string,
  accountId: string,
): Promise<boolean> {
  const r = await db
    .prepare(`DELETE FROM upstream_accounts WHERE id = ? AND user_id = ?`)
    .bind(accountId, userId)
    .run()
  return (r.meta.changes ?? 0) > 0
}

export async function userHasProvider(
  db: D1Database,
  userId: string,
  provider: string,
): Promise<boolean> {
  const row = await db
    .prepare(
      `SELECT 1 as ok FROM upstream_accounts WHERE user_id = ? AND provider = ? LIMIT 1`,
    )
    .bind(userId, provider)
    .first<{ ok: number }>()
  return !!row
}

/**
 * Bulk-delete every account row for one (user, provider) — used when a
 * custom provider is removed (its accounts have no FK to cascade on).
 * Returns the deleted rows so callers can best-effort clear their bench keys.
 */
export async function deleteAccountsForProvider(
  db: D1Database,
  userId: string,
  provider: string,
): Promise<AccountRow[]> {
  const rows = await listAccounts(db, userId, provider)
  await db
    .prepare(`DELETE FROM upstream_accounts WHERE user_id = ? AND provider = ?`)
    .bind(userId, provider)
    .run()
  return rows
}
