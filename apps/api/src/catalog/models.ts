/**
 * Model list from live upstream APIs — no hard-coded product catalogs.
 * Claude Code: GET api.anthropic.com/v1/models
 * Grok: GET api.x.ai/v1/models
 * Codex: no models endpoint → empty (documented)
 */

import type { Env, ProviderId } from "../env"
import { PROVIDERS } from "../env"
import { listAccounts } from "../db/accounts"
import { decryptJson } from "../crypto/token_crypto"
import type { StoredCredential } from "../pool/acquire"
import { isBenched } from "../pool/bench"
import { getAdapter } from "../providers"

export type CatalogModel = {
  id: string
  provider: ProviderId
  upstream: string
  display_name: string
  available: boolean
  owned_by: ProviderId
  object: "model"
}

export type ProviderModelsSection = {
  provider: ProviderId
  models: CatalogModel[]
  error: string | null
  cached: boolean
}

const MODELS_CACHE_TTL_SECONDS = 90

function modelsCacheKey(userId: string, provider: ProviderId): string {
  return `models:v1:${userId}:${provider}`
}

type CachedModels = {
  models: CatalogModel[]
  error: string | null
  fetchedAt: number
}

async function readModelsCache(
  env: Env,
  userId: string,
  provider: ProviderId,
): Promise<CachedModels | null> {
  try {
    const raw = await env.CACHE.get(modelsCacheKey(userId, provider), "json")
    if (!raw || typeof raw !== "object") return null
    const snap = raw as CachedModels
    if (typeof snap.fetchedAt !== "number") return null
    if (Date.now() - snap.fetchedAt > MODELS_CACHE_TTL_SECONDS * 1000 + 5_000) {
      return null
    }
    return snap
  } catch {
    return null
  }
}

async function writeModelsCache(
  env: Env,
  userId: string,
  provider: ProviderId,
  snap: Omit<CachedModels, "fetchedAt">,
): Promise<void> {
  try {
    await env.CACHE.put(
      modelsCacheKey(userId, provider),
      JSON.stringify({ ...snap, fetchedAt: Date.now() }),
      { expirationTtl: MODELS_CACHE_TTL_SECONDS },
    )
  } catch {
    /* */
  }
}

async function pickUsableAccount(
  env: Env,
  userId: string,
  provider: ProviderId,
): Promise<{ row: Awaited<ReturnType<typeof listAccounts>>[0]; credential: StoredCredential } | null> {
  const rows = await listAccounts(env.DB, userId, provider)
  for (const row of rows) {
    if (await isBenched(env, userId, provider, row.id)) continue
    try {
      const credential = await decryptJson<StoredCredential>(
        env.TOKEN_ENCRYPTION_KEY,
        row.encrypted_payload,
      )
      return { row, credential }
    } catch {
      continue
    }
  }
  return null
}

async function fetchProviderModels(
  env: Env,
  userId: string,
  provider: ProviderId,
  force: boolean,
): Promise<ProviderModelsSection> {
  if (!force) {
    const cached = await readModelsCache(env, userId, provider)
    if (cached) {
      return {
        provider,
        models: cached.models,
        error: cached.error,
        cached: true,
      }
    }
  }

  const picked = await pickUsableAccount(env, userId, provider)
  if (!picked) {
    const empty: ProviderModelsSection = {
      provider,
      models: [],
      error: null,
      cached: false,
    }
    // Don't cache "no account" aggressively — short TTL via skip write
    return empty
  }

  const adapter = getAdapter(provider)
  if (!adapter.listModels) {
    return {
      provider,
      models: [],
      error: "provider has no listModels",
      cached: false,
    }
  }

  let account = picked
  if (adapter.refreshIfNeeded) {
    account = await adapter.refreshIfNeeded(env, picked)
  }

  const result = await adapter.listModels(env, account)
  const models: CatalogModel[] = result.models.map((m) => ({
    id: `${provider}/${m.id}`,
    provider,
    upstream: m.id,
    display_name: m.display_name || m.id,
    available: true,
    owned_by: provider,
    object: "model" as const,
  }))

  await writeModelsCache(env, userId, provider, {
    models,
    error: result.error ?? null,
  })

  return {
    provider,
    models,
    error: result.error ?? null,
    cached: false,
  }
}

/**
 * Live models for a user. Only providers with bound accounts are queried.
 * `availableOnly` is kept for API shape; all returned models are available.
 */
export async function listModelsForUser(
  env: Env,
  userId: string,
  opts?: { availableOnly?: boolean; force?: boolean },
): Promise<{
  models: CatalogModel[]
  providers: ProviderModelsSection[]
}> {
  const force = !!opts?.force
  const sections: ProviderModelsSection[] = []

  for (const provider of PROVIDERS) {
    const section = await fetchProviderModels(env, userId, provider, force)
    sections.push(section)
  }

  const models = sections.flatMap((s) => s.models)
  // availableOnly has no extra filter: we never invent unavailable catalog rows
  return { models, providers: sections }
}
