import type { Env } from "../env"

const DEFAULT_COOLDOWN_MS = 300_000

export function benchKey(userId: string, provider: string, accountId: string): string {
  return `bench:${userId}:${provider}:${accountId}`
}

export async function isBenched(env: Env, userId: string, provider: string, accountId: string): Promise<boolean> {
  const until = await env.BENCH.get(benchKey(userId, provider, accountId))
  if (!until) return false
  const t = Number(until)
  if (!Number.isFinite(t) || t <= Date.now()) {
    await env.BENCH.delete(benchKey(userId, provider, accountId))
    return false
  }
  return true
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
