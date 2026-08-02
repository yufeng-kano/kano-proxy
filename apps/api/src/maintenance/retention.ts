/**
 * Daily data-retention sweep — see docs/logging.md ("Retention sweep").
 * Deletes `request_logs` rows past the retention window, plus expired
 * `sessions` / `oauth_login_states` rows. Invoked from the `scheduled`
 * handler in index.ts (Wrangler `[triggers] crons`); never called from the
 * request path.
 *
 * Errors intentionally propagate — the caller (index.ts `scheduled`) is
 * responsible for catching/logging so a sweep failure never escapes as an
 * unhandled rejection.
 */
import type { Env } from "../env"
import { nowIso } from "../utils/id"

const DEFAULT_RETENTION_DAYS = 90

/** Rows deleted per DELETE statement, so one sweep run never issues a single huge query. */
export const REQUEST_LOG_BATCH_SIZE = 2000

/** Hard cap on batches per run — bounds run time; a larger backlog just drains over more days. */
const MAX_BATCHES_PER_RUN = 40

/** `REQUEST_LOG_RETENTION_DAYS` as a positive integer day count, defaulting on anything else. */
function retentionDays(env: Env): number {
  const raw = env.REQUEST_LOG_RETENTION_DAYS
  const parsed = typeof raw === "string" ? Number.parseInt(raw, 10) : NaN
  return Number.isFinite(parsed) && parsed >= 1 ? parsed : DEFAULT_RETENTION_DAYS
}

/**
 * Deletes `request_logs` rows with `created_at` strictly before `cutoff`, in
 * batches of `batchSize` via the portable id-subquery form (works the same
 * on D1/SQLite whether or not `created_at` alone is indexed). Loops while a
 * batch comes back full, up to `maxBatches`, so one run is always bounded —
 * exported (with overridable batch/cap) so batching and the run-time cap can
 * be exercised directly in tests without seeding tens of thousands of rows.
 */
export async function sweepRequestLogs(
  env: Env,
  cutoff: string,
  batchSize: number = REQUEST_LOG_BATCH_SIZE,
  maxBatches: number = MAX_BATCHES_PER_RUN,
): Promise<number> {
  let deleted = 0
  for (let i = 0; i < maxBatches; i++) {
    const res = await env.DB.prepare(
      `DELETE FROM request_logs WHERE id IN (SELECT id FROM request_logs WHERE created_at < ? LIMIT ${batchSize})`,
    )
      .bind(cutoff)
      .run()
    const changes = res.meta.changes ?? 0
    deleted += changes
    if (changes < batchSize) break
  }
  return deleted
}

async function deleteExpiredSessions(env: Env, now: string): Promise<number> {
  const res = await env.DB.prepare(`DELETE FROM sessions WHERE expires_at < ?`).bind(now).run()
  return res.meta.changes ?? 0
}

async function deleteExpiredOauthStates(env: Env, now: string): Promise<number> {
  const res = await env.DB.prepare(`DELETE FROM oauth_login_states WHERE expires_at < ?`)
    .bind(now)
    .run()
  return res.meta.changes ?? 0
}

export async function runRetentionSweep(
  env: Env,
): Promise<{ requestLogs: number; sessions: number; oauthStates: number }> {
  const now = nowIso()
  const cutoff = new Date(Date.now() - retentionDays(env) * 86_400_000).toISOString()

  const requestLogs = await sweepRequestLogs(env, cutoff)
  const sessions = await deleteExpiredSessions(env, now)
  const oauthStates = await deleteExpiredOauthStates(env, now)

  console.log(
    `[retention] request_logs=${requestLogs} sessions=${sessions} oauth_login_states=${oauthStates}`,
  )

  return { requestLogs, sessions, oauthStates }
}
