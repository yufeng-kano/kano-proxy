import { getAccount, type AccountRow } from "../db/accounts"
import type { Env } from "../env"

const DEFAULT_COOLDOWN_MS = 300_000

/** Reads an account row's bench state without a separate D1 query. Expired values are never deleted. */
export function benchUntilFromRow(row: Pick<AccountRow, "bench_until">, now = Date.now()): number | null {
  if (!row.bench_until) return null
  const until = Date.parse(row.bench_until)
  return Number.isFinite(until) && until > now ? until : null
}

/** Bench-until epoch-ms for one account, or null when no current D1 bench exists. */
export async function benchedUntil(
  env: Env,
  userId: string,
  _provider: string,
  accountId: string,
): Promise<number | null> {
  const row = await getAccount(env.DB, userId, accountId)
  return row ? benchUntilFromRow(row) : null
}

export async function isBenched(env: Env, userId: string, provider: string, accountId: string): Promise<boolean> {
  return (await benchedUntil(env, userId, provider, accountId)) !== null
}

/**
 * Extend an account bench atomically. A shorter concurrent penalty never
 * truncates an existing longer one; reason changes only when the extension wins.
 */
export async function markBenched(
  env: Env,
  userId: string,
  provider: string,
  accountId: string,
  cooldownMs = DEFAULT_COOLDOWN_MS,
  reason = "refresh_failed",
): Promise<void> {
  const until = new Date(Date.now() + cooldownMs).toISOString()
  await env.DB
    .prepare(
      `UPDATE upstream_accounts
       SET bench_until = ?, bench_reason = ?
       WHERE id = ? AND user_id = ? AND provider = ?
         AND (bench_until IS NULL OR bench_until < ?)`,
    )
    .bind(until, reason, accountId, userId, provider, until)
    .run()
}

/** Unpause is deliberately idempotent and clears both D1 bench fields. */
export async function clearBench(
  env: Env,
  userId: string,
  provider: string,
  accountId: string,
): Promise<void> {
  await env.DB
    .prepare(
      `UPDATE upstream_accounts
       SET bench_until = NULL, bench_reason = NULL
       WHERE id = ? AND user_id = ? AND provider = ?`,
    )
    .bind(accountId, userId, provider)
    .run()
}

/** Earliest current D1 bench expiry across the supplied account ids. */
export async function earliestBenchExpiry(
  env: Env,
  userId: string,
  provider: string,
  accountIds: string[],
): Promise<number | null> {
  let earliest: number | null = null
  for (const accountId of accountIds) {
    const until = await benchedUntil(env, userId, provider, accountId)
    if (until !== null && (earliest === null || until < earliest)) earliest = until
  }
  return earliest
}
