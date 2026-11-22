/**
 * Server-data cache — the only module that touches browser storage for
 * anything the API returned.
 *
 * Entries live in `localStorage`, not sessionStorage: a reopened tab or a
 * browser restart paints the last known data instantly instead of an empty
 * page, and multiple tabs share one cache instead of each re-fetching
 * (docs/admin-ui.md § UX rules).
 *
 * Because that means the data now sits on disk across restarts, two rules
 * hold:
 *
 * 1. Every entry is a versioned envelope `{ v, savedAt, data }`. A read whose
 *    `v` is not the current CACHE_SCHEMA_VERSION — or that is malformed,
 *    truncated, or hand-edited — is a **miss**, never trusted data. Bumping
 *    the version retires every stale shape at once.
 * 2. Only non-secret display data is ever cached: account labels/emails,
 *    usage percentages, model ids, custom-provider `key_mask`. Never access
 *    tokens, refresh tokens, session secrets, or a provider's API key. The
 *    logout sweep (clearDataCaches) wipes what is here.
 */

import type {
  AccountsResponse,
  ChangelogResponse,
  CliDevice,
  CliProvider,
  CustomProvidersResponse,
  LogsResponse,
  ModelGroup,
  ModelsResponse,
  ProviderId,
  UsageSummary,
} from "@/types"

/** Frontend cache TTL — paired with the Providers page's 2 min poll interval. */
export const CACHE_TTL_MS = 120_000

/**
 * Envelope version. Bump whenever a cached payload's shape changes: every
 * entry written by an older build then reads as a miss instead of feeding a
 * stale shape into the UI.
 */
const CACHE_SCHEMA_VERSION = 19

/** Changelog TTL — release notes change on deploy, not continuously (docs/changelog.md). */
const CHANGELOG_CACHE_TTL_MS = 60 * 60 * 1000

const ACCOUNTS_PREFIX = "kano-proxy:accounts:"
const MODELS_PREFIX = "kano-proxy:models:"
const CUSTOM_PROVIDERS_PREFIX = "kano-proxy:custom-providers:"
const MODEL_GROUPS_PREFIX = "kano-proxy:model-groups:"
const CLI_PREFIX = "kano-proxy:cli:"
const USAGE_PREFIX = "kano-proxy:usage:"
const LOGS_PREFIX = "kano-proxy:logs:"
/**
 * Deliberately the whole key, with **no user id** appended — unlike every
 * other prefix here. Release notes are identical for every operator and hold
 * nothing user-identifying, so scoping the key would only mean each signed-in
 * user re-fetching the same public payload. It persists in `localStorage`
 * like the rest, and the logout sweep below still clears it, unconditionally.
 */
const CHANGELOG_KEY = "kano-proxy:changelog"

/** Versioned envelope every cached payload is wrapped in on disk. */
type Timed<T> = {
  v: number
  savedAt: number
  data: T
}

/** Every per-user prefix, for the logout sweep. */
const USER_PREFIXES = [
  ACCOUNTS_PREFIX,
  MODELS_PREFIX,
  CUSTOM_PROVIDERS_PREFIX,
  MODEL_GROUPS_PREFIX,
  CLI_PREFIX,
  USAGE_PREFIX,
  LOGS_PREFIX,
]

function accountsKey(userId: string, provider: ProviderId): string {
  return `${ACCOUNTS_PREFIX}${userId}:${provider}`
}

function modelsKey(userId: string): string {
  return `${MODELS_PREFIX}${userId}`
}

function customProvidersKey(userId: string): string {
  return `${CUSTOM_PROVIDERS_PREFIX}${userId}`
}

function modelGroupsKey(userId: string): string {
  return `${MODEL_GROUPS_PREFIX}${userId}`
}

function cliKey(userId: string): string {
  return `${CLI_PREFIX}${userId}`
}

function usageKey(userId: string, rangeKey: string): string {
  return `${USAGE_PREFIX}${userId}:${rangeKey}`
}

function logsKey(userId: string): string {
  return `${LOGS_PREFIX}${userId}`
}

/** Whether `key` is one of `userId`'s entries: accounts and usage hold one per provider / range. */
function ownedBy(key: string, userId: string): boolean {
  return (
    key.startsWith(`${ACCOUNTS_PREFIX}${userId}:`) ||
    key.startsWith(`${USAGE_PREFIX}${userId}:`) ||
    [modelsKey, customProvidersKey, modelGroupsKey, cliKey, logsKey].some((k) => k(userId) === key)
  )
}

/**
 * Anything that is not a current-version envelope — wrong `v`, malformed
 * JSON, missing fields — is a miss. Stale-shaped data on disk is never fed to
 * the UI, it is simply re-fetched.
 */
function readTimed<T>(storageKey: string): Timed<T> | null {
  if (typeof localStorage === "undefined") return null
  try {
    const raw = localStorage.getItem(storageKey)
    if (!raw) return null
    const parsed: unknown = JSON.parse(raw)
    if (!parsed || typeof parsed !== "object") return null
    const entry = parsed as Partial<Timed<T>>
    if (entry.v !== CACHE_SCHEMA_VERSION) return null
    if (typeof entry.savedAt !== "number" || !("data" in entry)) return null
    return { v: entry.v, savedAt: entry.savedAt, data: entry.data as T }
  } catch {
    return null
  }
}

/** A current-version entry saved less than `ttlMs` ago. */
function isFresh(storageKey: string, ttlMs: number): boolean {
  const entry = readTimed<unknown>(storageKey)
  return entry !== null && Date.now() - entry.savedAt < ttlMs
}

function writeTimed<T>(storageKey: string, data: T): void {
  if (typeof localStorage === "undefined") return
  try {
    const entry: Timed<T> = { v: CACHE_SCHEMA_VERSION, savedAt: Date.now(), data }
    localStorage.setItem(storageKey, JSON.stringify(entry))
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

export function isAccountsCacheFresh(
  userId: string | null | undefined,
  provider: ProviderId,
): boolean {
  return !!userId && isFresh(accountsKey(userId, provider), CACHE_TTL_MS)
}

export function writeAccountsCache(
  userId: string | null | undefined,
  provider: ProviderId,
  data: AccountsResponse,
): void {
  if (!userId) return
  writeTimed(accountsKey(userId, provider), data)
}

/**
 * Clears every server-data cache (accounts, models, custom providers, model
 * groups, CLI, usage, logs, changelog) — used on logout, so nothing the
 * previous user's session fetched survives on disk for the next person on the
 * machine. With a `userId`, other users' entries stay; the changelog entry,
 * which belongs to no user, always goes.
 */
export function clearDataCaches(userId?: string | null): void {
  if (typeof localStorage === "undefined") return
  try {
    const keys: string[] = []
    for (let i = 0; i < localStorage.length; i++) {
      const k = localStorage.key(i)
      if (!k) continue
      if (k === CHANGELOG_KEY) keys.push(k)
      else if (USER_PREFIXES.some((p) => k.startsWith(p)) && (!userId || ownedBy(k, userId))) keys.push(k)
    }
    for (const k of keys) localStorage.removeItem(k)
  } catch {
    /* private mode: nothing was persisted to clear */
  }
}

export function readModelsCache(
  userId: string | null | undefined,
): ModelsResponse | null {
  if (!userId) return null
  return readTimed<ModelsResponse>(modelsKey(userId))?.data ?? null
}

export function isModelsCacheFresh(userId: string | null | undefined): boolean {
  return !!userId && isFresh(modelsKey(userId), CACHE_TTL_MS)
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
 * there is nothing key-shaped here to persist to disk. Shared rows carry even
 * less (no base URL, no mask), and ride in the same per-user entry so the
 * logout and account-switch sweeps clear them with the own list.
 */
export function readCustomProvidersCache(
  userId: string | null | undefined,
): CustomProvidersResponse | null {
  if (!userId) return null
  return readTimed<CustomProvidersResponse>(customProvidersKey(userId))?.data ?? null
}

export function isCustomProvidersCacheFresh(userId: string | null | undefined): boolean {
  return !!userId && isFresh(customProvidersKey(userId), CACHE_TTL_MS)
}

export function writeCustomProvidersCache(
  userId: string | null | undefined,
  data: CustomProvidersResponse,
): void {
  if (!userId) return
  writeTimed(customProvidersKey(userId), data)
}

/**
 * Drops the model-groups and models caches for one user without touching the
 * rest. Used after a custom endpoint's prefix is renamed: the server rewrote
 * the group targets and the catalog ids, so a cache-first paint of either page
 * would show the old prefix until the TTL ran out (docs/admin-ui.md § Providers
 * page).
 */
export function invalidateModelCaches(userId: string | null | undefined): void {
  if (!userId || typeof localStorage === "undefined") return
  try {
    localStorage.removeItem(modelsKey(userId))
    localStorage.removeItem(modelGroupsKey(userId))
  } catch {
    /* */
  }
}

/**
 * Model groups cache. Names and targets only — a group is user config, and
 * the response carries nothing secret to keep off disk.
 */
export function readModelGroupsCache(
  userId: string | null | undefined,
): ModelGroup[] | null {
  if (!userId) return null
  return readTimed<ModelGroup[]>(modelGroupsKey(userId))?.data ?? null
}

export function isModelGroupsCacheFresh(userId: string | null | undefined): boolean {
  return !!userId && isFresh(modelGroupsKey(userId), CACHE_TTL_MS)
}

export function writeModelGroupsCache(
  userId: string | null | undefined,
  data: ModelGroup[],
): void {
  if (!userId) return
  writeTimed(modelGroupsKey(userId), data)
}

/**
 * CLI page cache — devices and providers together, since the page loads both
 * as one paint. Names, slugs, model ids and timestamps only; tokens never
 * reach the browser at all (docs/cli.md).
 */
type CliCachePayload = { devices: CliDevice[]; providers: CliProvider[] }

export function readCliCache(userId: string | null | undefined): CliCachePayload | null {
  if (!userId) return null
  return readTimed<CliCachePayload>(cliKey(userId))?.data ?? null
}

export function isCliCacheFresh(userId: string | null | undefined): boolean {
  return !!userId && isFresh(cliKey(userId), CACHE_TTL_MS)
}

export function writeCliCache(userId: string | null | undefined, data: CliCachePayload): void {
  if (!userId) return
  writeTimed(cliKey(userId), data)
}

/**
 * Usage summary cache, one entry per range so switching the range picker can
 * paint that range's own cache immediately. Only the aggregate JSON is ever
 * cached — no prompts/completions/secrets pass through here.
 */
export function readUsageSummaryCache(
  userId: string | null | undefined,
  rangeKey: string,
): UsageSummary | null {
  if (!userId) return null
  return readTimed<UsageSummary>(usageKey(userId, rangeKey))?.data ?? null
}

export function isUsageSummaryCacheFresh(
  userId: string | null | undefined,
  rangeKey: string,
): boolean {
  return !!userId && isFresh(usageKey(userId, rangeKey), CACHE_TTL_MS)
}

export function writeUsageSummaryCache(
  userId: string | null | undefined,
  rangeKey: string,
  data: UsageSummary,
): void {
  if (!userId) return
  writeTimed(usageKey(userId, rangeKey), data)
}

/**
 * Request log cache — the **first page of the unfiltered view** only
 * (docs/admin-ui.md § Logs page). A filtered view is one of arbitrarily many
 * and a Load-more page is a slice of a list that has moved on by the next
 * visit, so neither is worth keeping on disk. The rows carry no prompts,
 * completions, or keys — only ids, model names, and counts.
 */
export function readLogsCache(userId: string | null | undefined): LogsResponse | null {
  if (!userId) return null
  return readTimed<LogsResponse>(logsKey(userId))?.data ?? null
}

export function isLogsCacheFresh(userId: string | null | undefined): boolean {
  return !!userId && isFresh(logsKey(userId), CACHE_TTL_MS)
}

export function writeLogsCache(
  userId: string | null | undefined,
  data: LogsResponse,
): void {
  if (!userId) return
  writeTimed(logsKey(userId), data)
}

/**
 * Changelog cache. Takes no `userId` — see the CHANGELOG_KEY note above: the
 * payload is public release notes plus the running version, identical for
 * every operator. TTL defaults to an hour rather than the 2 min the other
 * domains use (docs/changelog.md § Web caching).
 */
export function readChangelogCache(): ChangelogResponse | null {
  return readTimed<ChangelogResponse>(CHANGELOG_KEY)?.data ?? null
}

export function isChangelogCacheFresh(): boolean {
  return isFresh(CHANGELOG_KEY, CHANGELOG_CACHE_TTL_MS)
}

export function writeChangelogCache(data: ChangelogResponse): void {
  writeTimed(CHANGELOG_KEY, data)
}
