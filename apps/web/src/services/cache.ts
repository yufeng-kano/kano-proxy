import type { AccountsResponse, CatalogModel, CustomProvider, ModelsResponse, ProviderId } from "@/types"

/** Frontend cache TTL — align with backend usage cache (90s). */
export const CACHE_TTL_MS = 90_000

const ACCOUNTS_PREFIX = "kano-proxy:accounts:"
const MODELS_PREFIX = "kano-proxy:models:"
const CUSTOM_PROVIDERS_PREFIX = "kano-proxy:custom-providers:"

type Timed<T> = {
  savedAt: number
  data: T
}

function accountsKey(userId: string, provider: ProviderId): string {
  return `${ACCOUNTS_PREFIX}${userId}:${provider}`
}

function modelsKey(userId: string): string {
  return `${MODELS_PREFIX}${userId}`
}

function customProvidersKey(userId: string): string {
  return `${CUSTOM_PROVIDERS_PREFIX}${userId}`
}

function readTimed<T>(storageKey: string): Timed<T> | null {
  if (typeof sessionStorage === "undefined") return null
  try {
    const raw = sessionStorage.getItem(storageKey)
    if (!raw) return null
    const parsed = JSON.parse(raw) as Timed<T> | T
    // Back-compat: old cache was bare data without savedAt
    if (parsed && typeof parsed === "object" && "savedAt" in parsed && "data" in parsed) {
      return parsed as Timed<T>
    }
    return { savedAt: 0, data: parsed as T }
  } catch {
    return null
  }
}

function writeTimed<T>(storageKey: string, data: T): void {
  if (typeof sessionStorage === "undefined") return
  try {
    const entry: Timed<T> = { savedAt: Date.now(), data }
    sessionStorage.setItem(storageKey, JSON.stringify(entry))
  } catch {
    /* quota / private mode */
  }
}

export function readAccountsCache(
  userId: string | null | undefined,
  provider: ProviderId,
): AccountsResponse | null {
  if (!userId) return null
  return readTimed<AccountsResponse>(accountsKey(userId, provider))?.data ?? null
}

/** Returns cache age in ms, or null if missing. */
export function accountsCacheAgeMs(
  userId: string | null | undefined,
  provider: ProviderId,
): number | null {
  if (!userId) return null
  const entry = readTimed<AccountsResponse>(accountsKey(userId, provider))
  if (!entry) return null
  return Date.now() - entry.savedAt
}

export function isAccountsCacheFresh(
  userId: string | null | undefined,
  provider: ProviderId,
  ttlMs = CACHE_TTL_MS,
): boolean {
  const age = accountsCacheAgeMs(userId, provider)
  return age !== null && age < ttlMs
}

export function writeAccountsCache(
  userId: string | null | undefined,
  provider: ProviderId,
  data: AccountsResponse,
): void {
  if (!userId) return
  writeTimed(accountsKey(userId, provider), data)
}

/** Clears all session-scoped caches (accounts, models, custom providers) — used on logout. */
export function clearAccountsCache(userId?: string | null): void {
  if (typeof sessionStorage === "undefined") return
  try {
    const keys: string[] = []
    for (let i = 0; i < sessionStorage.length; i++) {
      const k = sessionStorage.key(i)
      if (!k) continue
      if (k.startsWith(ACCOUNTS_PREFIX)) {
        if (userId && !k.startsWith(`${ACCOUNTS_PREFIX}${userId}:`)) continue
        keys.push(k)
      }
      if (k.startsWith(MODELS_PREFIX)) {
        if (userId && k !== modelsKey(userId)) continue
        keys.push(k)
      }
      if (k.startsWith(CUSTOM_PROVIDERS_PREFIX)) {
        if (userId && k !== customProvidersKey(userId)) continue
        keys.push(k)
      }
    }
    for (const k of keys) sessionStorage.removeItem(k)
  } catch {
    /* */
  }
}

export function readModelsCache(
  userId: string | null | undefined,
): ModelsResponse | null {
  if (!userId) return null
  return readTimed<ModelsResponse>(modelsKey(userId))?.data ?? null
}

export function isModelsCacheFresh(
  userId: string | null | undefined,
  ttlMs = CACHE_TTL_MS,
): boolean {
  if (!userId) return false
  const entry = readTimed<ModelsResponse>(modelsKey(userId))
  if (!entry) return false
  return Date.now() - entry.savedAt < ttlMs
}

export function writeModelsCache(
  userId: string | null | undefined,
  data: ModelsResponse,
): void {
  if (!userId) return
  writeTimed(modelsKey(userId), data)
}

/**
 * Custom providers list cache. `key_mask` is non-secret display data and is
 * fine to cache — the API response never carries the plaintext `api_key`, so
 * there is nothing key-shaped here to keep out of sessionStorage.
 */
export function readCustomProvidersCache(
  userId: string | null | undefined,
): CustomProvider[] | null {
  if (!userId) return null
  return readTimed<CustomProvider[]>(customProvidersKey(userId))?.data ?? null
}

export function isCustomProvidersCacheFresh(
  userId: string | null | undefined,
  ttlMs = CACHE_TTL_MS,
): boolean {
  if (!userId) return false
  const entry = readTimed<CustomProvider[]>(customProvidersKey(userId))
  if (!entry) return false
  return Date.now() - entry.savedAt < ttlMs
}

export function writeCustomProvidersCache(
  userId: string | null | undefined,
  data: CustomProvider[],
): void {
  if (!userId) return
  writeTimed(customProvidersKey(userId), data)
}

// silence unused if tree-shaken elsewhere
export type { CatalogModel }
