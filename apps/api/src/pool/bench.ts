import type { Env } from "../env"

const DEFAULT_COOLDOWN_MS = 300_000

export function benchKey(userId: string, provider: string, accountId: string): string {
  return `bench:${userId}:${provider}:${accountId}`
}

/**
 * Bench-until epoch-ms for one account, or `null` when it isn't currently
 * benched (never benched, or the cooldown already elapsed — an expired key
 * is opportunistically cleaned up here, same as the old `isBenched` body).
 */
export async function benchedUntil(
  env: Env,
  userId: string,
  provider: string,
  accountId: string,
): Promise<number | null> {
  const key = benchKey(userId, provider, accountId)
  const until = await env.BENCH.get(key)
  if (!until) return null
  const t = Number(until)
  if (!Number.isFinite(t) || t <= Date.now()) {
    await env.BENCH.delete(key)
    return null
  }
  return t
}

export async function isBenched(env: Env, userId: string, provider: string, accountId: string): Promise<boolean> {
  return (await benchedUntil(env, userId, provider, accountId)) !== null
}

export async function markBenched(
  env: Env,
  userId: string,
  provider: string,
  accountId: string,
  cooldownMs = DEFAULT_COOLDOWN_MS,
): Promise<void> {
  const until = Date.now() + cooldownMs
  await env.BENCH.put(benchKey(userId, provider, accountId), String(until), {
    expirationTtl: Math.ceil(cooldownMs / 1000) + 5,
  })
}

export async function clearBench(
  env: Env,
  userId: string,
  provider: string,
  accountId: string,
): Promise<void> {
  await env.BENCH.delete(benchKey(userId, provider, accountId))
}

/**
 * Earliest known bench-expiry (epoch ms) across a set of account ids for one
 * user+provider, or `null` when none of them have a known bench-until (none
 * currently benched — e.g. every id's credential simply failed to decrypt
 * rather than being benched). Used by dispatch to compute `Retry-After` when
 * the whole pool is unavailable — see docs/api.md "Errors".
 */
export async function earliestBenchExpiry(
  env: Env,
  userId: string,
  provider: string,
  accountIds: string[],
): Promise<number | null> {
  let earliest: number | null = null
  for (const accountId of accountIds) {
    const until = await benchedUntil(env, userId, provider, accountId)
    if (until === null) continue
    if (earliest === null || until < earliest) earliest = until
  }
  return earliest
}
