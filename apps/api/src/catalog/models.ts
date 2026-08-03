/**
 * Model list for bound providers.
 * Claude Code: GET api.anthropic.com/v1/models (live)
 * Grok: GET api.x.ai/v1/models (live)
 * Codex: no models endpoint → empty (UI links official docs; no invented catalog)
 */

import type { Env, ProviderId } from "../env"
import { PROVIDERS } from "../env"
import { listAccounts } from "../db/accounts"
import { listCustomProviders, type CustomProviderRow } from "../db/custom_providers"
import { decryptJson } from "../crypto/token_crypto"
import type { StoredCredential } from "../pool/acquire"
import { isBenched } from "../pool/bench"
import { getAdapter } from "../providers"
import { createCustomAnthropicAdapter } from "../providers/custom_anthropic"
import { createCustomOpenAIAdapter } from "../providers/custom_openai"
import type { UpstreamModel } from "../providers/types"
import { parseManualModels } from "../utils/custom_provider"

export type CatalogModel = {
  id: string
  /** Builtin `ProviderId`, or a custom provider's slug. */
  provider: string
  upstream: string
  display_name: string
  available: boolean
  owned_by: string
  object: "model"
}

export type ProviderModelsSection = {
  provider: string
  models: CatalogModel[]
  error: string | null
  cached: boolean
}

/**
 * Keep this one-hour KV cache (unlike the removed per-account usage cache):
 * client-facing GET /openai/v1/models and GET /anthropic/v1/models are called
 * by API clients with no frontend cache. `?refresh=true` still bypasses it.
 */
const MODELS_CACHE_TTL_SECONDS = 3600

/** `provider` is a builtin `ProviderId` or a custom provider's slug. */
function modelsCacheKey(userId: string, provider: string): string {
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
  provider: string,
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
  provider: string,
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
  provider: string,
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

function toCustomCatalogModel(slug: string, m: UpstreamModel): CatalogModel {
  return {
    id: `${slug}/${m.id}`,
    provider: slug,
    upstream: m.id,
    display_name: m.display_name || m.id,
    available: true,
    owned_by: slug,
    object: "model" as const,
  }
}

/**
 * manual → the stored manual list. auto → live listModels with an acquired
 * key (same 90s cache as built-ins, keyed by slug); on failure, or when no
 * usable key exists to query, fall back to the manual list if non-empty,
 * else empty. Never fabricates a catalog.
 */
async function fetchCustomProviderModels(
  env: Env,
  userId: string,
  row: CustomProviderRow,
  force: boolean,
): Promise<ProviderModelsSection> {
  const manualModels = (): CatalogModel[] =>
    parseManualModels(row.manual_models_json).map((id) =>
      toCustomCatalogModel(row.slug, { id, display_name: null }),
    )

  if (row.models_mode === "manual") {
    return { provider: row.slug, models: manualModels(), error: null, cached: false }
  }

  if (!force) {
    const cached = await readModelsCache(env, userId, row.slug)
    if (cached) {
      return { provider: row.slug, models: cached.models, error: cached.error, cached: true }
    }
  }

  const picked = await pickUsableAccount(env, userId, row.slug)
  if (!picked) {
    // No usable key to query live — same "don't cache aggressively" policy as built-ins.
    return { provider: row.slug, models: manualModels(), error: null, cached: false }
  }

  const adapter =
    row.format === "anthropic" ? createCustomAnthropicAdapter(row) : createCustomOpenAIAdapter(row)
  if (!adapter.listModels) {
    return { provider: row.slug, models: manualModels(), error: null, cached: false }
  }

  const result = await adapter.listModels(env, picked)
  if (result.error) {
    const fallback = manualModels()
    await writeModelsCache(env, userId, row.slug, { models: fallback, error: result.error })
    return { provider: row.slug, models: fallback, error: result.error, cached: false }
  }

  const models = result.models.map((m) => toCustomCatalogModel(row.slug, m))
  await writeModelsCache(env, userId, row.slug, { models, error: null })
  return { provider: row.slug, models, error: null, cached: false }
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

  const customRows = await listCustomProviders(env.DB, userId)
  for (const row of customRows) {
    sections.push(await fetchCustomProviderModels(env, userId, row, force))
  }

  const models = sections.flatMap((s) => s.models)
  // availableOnly has no extra filter: we never invent unavailable catalog rows
  return { models, providers: sections }
}
