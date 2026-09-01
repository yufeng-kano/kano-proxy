/** D1 access for CLI devices, login requests, and CLI providers (docs/cli.md). */

import { MAX_CUSTOM_PROVIDERS_PER_USER } from "../utils/custom_provider"
import { newId, nowIso } from "../utils/id"

export type CliDeviceRow = {
  id: string
  user_id: string
  name: string
  refresh_token_hash: string
  refresh_token_prev_hash: string | null
  last_seen_at: string | null
  created_at: string
  revoked_at: string | null
}

export type CliLoginRequestRow = {
  id: string
  device_name: string
  code_hash: string | null
  user_id: string | null
  expires_at: string
  approved_at: string | null
  used_at: string | null
  attempts: number
  created_at: string
}

export type CliProviderRow = {
  id: string
  user_id: string
  device_id: string | null
  slug: string
  name: string
  format: "openai" | "anthropic"
  models_json: string | null
  models_updated_at: string | null
  model_filter_json: string | null
  sort_order: number
  created_at: string
  updated_at: string
}

export const LOGIN_REQUEST_TTL_MS = 10 * 60 * 1000
export const MAX_LOGIN_CODE_ATTEMPTS = 5
export const MAX_CLI_DEVICES_PER_USER = 20
/** Login starts allowed per IP hash inside one request-TTL window. */
export const LOGIN_STARTS_PER_IP = 10

// ---------------------------------------------------------------------------
// Login requests

/**
 * Create a login request under the per-IP budget in one atomic statement:
 * the INSERT lands only while fewer than LOGIN_STARTS_PER_IP rows with this
 * ip_hash were created inside the window — a read-modify-write counter would
 * let parallel batches from one IP bypass the limit (docs/cli.md § Security
 * notes). Returns null when the budget is spent.
 */
export async function insertLoginRequest(
  db: D1Database,
  deviceName: string,
  ipHash: string,
): Promise<CliLoginRequestRow | null> {
  const id = newId("clireq")
  const ts = nowIso()
  const windowStart = new Date(Date.now() - LOGIN_REQUEST_TTL_MS).toISOString()
  const expires = new Date(Date.now() + LOGIN_REQUEST_TTL_MS).toISOString()
  const res = await db
    .prepare(
      `INSERT INTO cli_login_requests (id, device_name, ip_hash, expires_at, attempts, created_at)
       SELECT ?, ?, ?, ?, ?, ?
       WHERE (SELECT COUNT(*) FROM cli_login_requests WHERE ip_hash = ? AND created_at > ?) < ${LOGIN_STARTS_PER_IP}`,
    )
    .bind(id, deviceName, ipHash, expires, 0, ts, ipHash, windowStart)
    .run()
  if ((res.meta.changes ?? 0) === 0) return null
  return {
    id,
    device_name: deviceName,
    code_hash: null,
    user_id: null,
    expires_at: expires,
    approved_at: null,
    used_at: null,
    attempts: 0,
    created_at: ts,
  }
}

export async function getLoginRequest(db: D1Database, id: string): Promise<CliLoginRequestRow | null> {
  return (
    (await db.prepare(`SELECT * FROM cli_login_requests WHERE id = ?`).bind(id).first<CliLoginRequestRow>()) ?? null
  )
}

/** Approve stamps the session user + code hash in one write — the row is unauthenticated before this. */
export async function approveLoginRequest(
  db: D1Database,
  id: string,
  userId: string,
  codeHash: string,
): Promise<boolean> {
  const r = await db
    .prepare(
      `UPDATE cli_login_requests
       SET user_id = ?, code_hash = ?, approved_at = ?
       WHERE id = ? AND used_at IS NULL AND approved_at IS NULL AND expires_at > ?`,
    )
    .bind(userId, codeHash, nowIso(), id, nowIso())
    .run()
  return (r.meta.changes ?? 0) > 0
}

export async function deleteLoginRequest(db: D1Database, id: string): Promise<boolean> {
  const r = await db.prepare(`DELETE FROM cli_login_requests WHERE id = ?`).bind(id).run()
  return (r.meta.changes ?? 0) > 0
}

export async function recordLoginCodeAttempt(db: D1Database, id: string): Promise<void> {
  await db.prepare(`UPDATE cli_login_requests SET attempts = attempts + 1 WHERE id = ?`).bind(id).run()
}

export async function markLoginRequestUsed(db: D1Database, id: string): Promise<boolean> {
  const r = await db
    .prepare(`UPDATE cli_login_requests SET used_at = ? WHERE id = ? AND used_at IS NULL`)
    .bind(nowIso(), id)
    .run()
  return (r.meta.changes ?? 0) > 0
}

// ---------------------------------------------------------------------------
// Devices

export async function insertCliDevice(
  db: D1Database,
  input: { userId: string; name: string; refreshTokenHash: string },
): Promise<CliDeviceRow> {
  const id = newId("clidev")
  const ts = nowIso()
  await db
    .prepare(
      `INSERT INTO cli_devices (id, user_id, name, refresh_token_hash, refresh_token_prev_hash, last_seen_at, created_at, revoked_at)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
    )
    .bind(id, input.userId, input.name, input.refreshTokenHash, null, null, ts, null)
    .run()
  return {
    id,
    user_id: input.userId,
    name: input.name,
    refresh_token_hash: input.refreshTokenHash,
    refresh_token_prev_hash: null,
    last_seen_at: null,
    created_at: ts,
    revoked_at: null,
  }
}

export async function listCliDevices(db: D1Database, userId: string): Promise<CliDeviceRow[]> {
  const res = await db
    .prepare(`SELECT * FROM cli_devices WHERE user_id = ? ORDER BY created_at DESC`)
    .bind(userId)
    .all<CliDeviceRow>()
  return res.results ?? []
}

export async function getCliDevice(db: D1Database, id: string): Promise<CliDeviceRow | null> {
  return (await db.prepare(`SELECT * FROM cli_devices WHERE id = ?`).bind(id).first<CliDeviceRow>()) ?? null
}

/** Quota counts only live devices — a user who revoked 20 over time is not locked out forever. */
export async function countCliDevices(db: D1Database, userId: string): Promise<number> {
  const row = await db
    .prepare(`SELECT COUNT(*) as c FROM cli_devices WHERE user_id = ? AND revoked_at IS NULL`)
    .bind(userId)
    .first<{ c: number }>()
  return row?.c ?? 0
}

export async function findCliDeviceByRefreshHash(db: D1Database, hash: string): Promise<CliDeviceRow | null> {
  return (
    (await db.prepare(`SELECT * FROM cli_devices WHERE refresh_token_hash = ?`).bind(hash).first<CliDeviceRow>()) ??
    null
  )
}

export async function findCliDeviceByPrevRefreshHash(db: D1Database, hash: string): Promise<CliDeviceRow | null> {
  return (
    (await db
      .prepare(`SELECT * FROM cli_devices WHERE refresh_token_prev_hash = ?`)
      .bind(hash)
      .first<CliDeviceRow>()) ?? null
  )
}

/**
 * Rotate atomically: the WHERE re-checks the presented hash so two concurrent
 * presentations of the same refresh token cannot both win (docs/cli.md).
 */
export async function rotateCliDeviceRefreshToken(
  db: D1Database,
  deviceId: string,
  presentedHash: string,
  newHash: string,
): Promise<boolean> {
  const r = await db
    .prepare(
      `UPDATE cli_devices
       SET refresh_token_prev_hash = refresh_token_hash, refresh_token_hash = ?, last_seen_at = ?
       WHERE id = ? AND refresh_token_hash = ? AND revoked_at IS NULL`,
    )
    .bind(newHash, nowIso(), deviceId, presentedHash)
    .run()
  return (r.meta.changes ?? 0) > 0
}

/** Idempotent — a second revoke changes nothing. */
export async function revokeCliDevice(db: D1Database, userId: string, deviceId: string): Promise<boolean> {
  const r = await db
    .prepare(`UPDATE cli_devices SET revoked_at = ? WHERE id = ? AND user_id = ? AND revoked_at IS NULL`)
    .bind(nowIso(), deviceId, userId)
    .run()
  return (r.meta.changes ?? 0) > 0
}

export async function touchCliDeviceLastSeen(db: D1Database, deviceId: string): Promise<void> {
  await db.prepare(`UPDATE cli_devices SET last_seen_at = ? WHERE id = ?`).bind(nowIso(), deviceId).run()
}

// ---------------------------------------------------------------------------
// Providers

export async function listCliProviders(db: D1Database, userId: string): Promise<CliProviderRow[]> {
  const res = await db
    .prepare(`SELECT * FROM cli_providers WHERE user_id = ? ORDER BY sort_order ASC, created_at ASC`)
    .bind(userId)
    .all<CliProviderRow>()
  return res.results ?? []
}

export async function getCliProviderById(db: D1Database, userId: string, id: string): Promise<CliProviderRow | null> {
  return (
    (await db
      .prepare(`SELECT * FROM cli_providers WHERE id = ? AND user_id = ?`)
      .bind(id, userId)
      .first<CliProviderRow>()) ?? null
  )
}

/** Scoped to userId — a CLI slug must never resolve cross-user. */
export async function getCliProviderBySlug(db: D1Database, userId: string, slug: string): Promise<CliProviderRow | null> {
  return (
    (await db
      .prepare(`SELECT * FROM cli_providers WHERE user_id = ? AND slug = ?`)
      .bind(userId, slug)
      .first<CliProviderRow>()) ?? null
  )
}

export async function countCliProviders(db: D1Database, userId: string): Promise<number> {
  const row = await db
    .prepare(`SELECT COUNT(*) as c FROM cli_providers WHERE user_id = ?`)
    .bind(userId)
    .first<{ c: number }>()
  return row?.c ?? 0
}

/**
 * The slug namespace is shared with custom_providers, and a check-then-insert
 * pair can race a concurrent custom create for the same slug — so the guard
 * rides inside the INSERT itself (single-statement atomicity): the row lands
 * only while no custom provider owns the slug. Returns null on that conflict;
 * the own-table half is the UNIQUE(user_id, slug) constraint.
 */
export async function insertCliProvider(
  db: D1Database,
  input: {
    userId: string
    deviceId: string | null
    slug: string
    name: string
    format: "openai" | "anthropic"
    modelsJson: string | null
    modelFilterJson: string | null
  },
): Promise<CliProviderRow | null> {
  const id = newId("cliprov")
  const ts = nowIso()
  const max = await db
    .prepare(`SELECT COALESCE(MAX(sort_order), 0) as m FROM cli_providers WHERE user_id = ?`)
    .bind(input.userId)
    .first<{ m: number }>()
  const sortOrder = (max?.m ?? 0) + 1
  const res = await db
    .prepare(
      `INSERT INTO cli_providers
       (id, user_id, device_id, slug, name, format, models_json, models_updated_at, model_filter_json, sort_order, created_at, updated_at)
       SELECT ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?
       WHERE NOT EXISTS (SELECT 1 FROM custom_providers WHERE user_id = ? AND slug = ?)
         AND ((SELECT COUNT(*) FROM cli_providers WHERE user_id = ?) + (SELECT COUNT(*) FROM custom_providers WHERE user_id = ?)) < ${MAX_CUSTOM_PROVIDERS_PER_USER}`,
    )
    .bind(
      id,
      input.userId,
      input.deviceId,
      input.slug,
      input.name,
      input.format,
      input.modelsJson,
      input.modelsJson ? ts : null,
      input.modelFilterJson,
      sortOrder,
      ts,
      ts,
      input.userId,
      input.slug,
      input.userId,
      input.userId,
    )
    .run()
  if ((res.meta.changes ?? 0) === 0) return null
  return {
    id,
    user_id: input.userId,
    device_id: input.deviceId,
    slug: input.slug,
    name: input.name,
    format: input.format,
    models_json: input.modelsJson,
    models_updated_at: input.modelsJson ? ts : null,
    model_filter_json: input.modelFilterJson,
    sort_order: sortOrder,
    created_at: ts,
    updated_at: ts,
  }
}

export async function renameCliProvider(db: D1Database, userId: string, id: string, name: string): Promise<boolean> {
  const r = await db
    .prepare(`UPDATE cli_providers SET name = ?, updated_at = ? WHERE id = ? AND user_id = ?`)
    .bind(name, nowIso(), id, userId)
    .run()
  return (r.meta.changes ?? 0) > 0
}

export async function deleteCliProvider(db: D1Database, userId: string, id: string): Promise<boolean> {
  const r = await db.prepare(`DELETE FROM cli_providers WHERE id = ? AND user_id = ?`).bind(id, userId).run()
  return (r.meta.changes ?? 0) > 0
}

/** The agent-reported catalog — stored whole; the expose filter applies at read time only. */
export async function writeCliProviderModels(db: D1Database, providerId: string, models: string[]): Promise<void> {
  const ts = nowIso()
  await db
    .prepare(`UPDATE cli_providers SET models_json = ?, models_updated_at = ?, updated_at = ? WHERE id = ?`)
    .bind(JSON.stringify(models), ts, ts, providerId)
    .run()
}

export function parseCliModels(json: string | null): string[] {
  if (!json) return []
  try {
    const arr = JSON.parse(json) as unknown
    return Array.isArray(arr) ? arr.filter((s): s is string => typeof s === "string") : []
  } catch {
    return []
  }
}

/** The reported list with the expose-whitelist applied — what every read surface shows. */
export function exposedCliModels(row: Pick<CliProviderRow, "models_json" | "model_filter_json">): string[] {
  const reported = parseCliModels(row.models_json)
  const filter = parseCliModels(row.model_filter_json)
  if (filter.length === 0) return reported
  const allow = new Set(filter)
  return reported.filter((m) => allow.has(m))
}
