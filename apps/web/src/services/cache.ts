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
  CatalogModel,
  ChangelogResponse,
  CliDevice,
  CliProvider,
  CustomProvider,
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
 * stale shape into the UI. v2: usage summaries and keys grew cost/spend
 * fields (docs/pricing.md). v3: accounts grew `custom_label`. v4: model group
 * targets became `{model, account_id, account_label}` objects — a v3 entry
 * holds bare strings, which would render as empty rows. v5: custom providers
 * grew `account_id` — a v4 entry has none, so the Groups picker would show
 * every endpoint as having no account to pin. v6: a model group's `name` became
 * a display label and its callable ids moved to `aliases` — a v5 entry has no
 * aliases, so every row would render an empty Aliases column. v7: usage
 * summaries split their per-model rows by account and group alias, and model
 * groups grew `routing` — a v6 entry has neither, so the By-model table would
 * hold rows with no account and the group rows no current-route indicator.
 * v8: the usage summary's per-model rows went back to one row per (provider,
 * model) — a v7 entry holds the account/alias split, which would double-count
 * every model the By-model table now shows once. v9: log rows dropped the
 * internal `api_keys` row id and grew `api_key_removed` — a v8 entry still
 * carries that id, which the row detail must never render. v10: model groups
 * became virtual endpoints (`slug` + per-group `models`, aliases/targets
 * gone) and the shared catalog dropped its `group` section — a v9 entry has
 * neither shape and would render empty rows on the Groups page. v11: usage
 * summaries grew calendar-aligned Day/Week/Month support with rangeKey
 * caching (`day:YYYY-MM-DD`, etc.).
 */
const CACHE_SCHEMA_VERSION = 11

/** Changelog TTL — release notes change on deploy, not continuously (docs/changelog.md). */
export const CHANGELOG_CACHE_TTL_MS = 60 * 60 * 1000

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

function usageKey(userId: string, rangeKey: string | number): string {
  return `${USAGE_PREFIX}${userId}:${rangeKey}`
}

function logsKey(userId: string): string {
  return `${LOGS_PREFIX}${userId}`
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

/**
 * Clears every server-data cache (accounts, models, custom providers, model
 * groups, usage, changelog) — used on logout, so nothing the previous user's
 * session fetched survives on disk for the next person on the machine.
 *
 * Sweeps `sessionStorage` as well as `localStorage`: entries written before
 * these caches moved to localStorage may still be sitting there in a
 * long-lived tab, and they must be cleared by the same sign-out.
 */
export function clearDataCaches(userId?: string | null): void {
  for (const store of dataStores()) sweepStore(store, userId)
}

function dataStores(): Storage[] {
  const stores: Storage[] = []
  // Legacy: these caches lived in sessionStorage before the move to
  // localStorage — sweep both so pre-migration leftovers go too.
  if (typeof localStorage !== "undefined") stores.push(localStorage)
  if (typeof sessionStorage !== "undefined") stores.push(sessionStorage)
  return stores
}

function sweepStore(store: Storage, userId?: string | null): void {
  try {
    const keys: string[] = []
    for (let i = 0; i < store.length; i++) {
      const k = store.key(i)
      if (!k) continue
      // No user id in this key, so there is no per-user entry to single out:
      // it is always the one shared entry, always cleared.
      if (k === CHANGELOG_KEY) keys.push(k)
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
      if (k.startsWith(MODEL_GROUPS_PREFIX)) {
        if (userId && k !== modelGroupsKey(userId)) continue
        keys.push(k)
      }
      if (k.startsWith(CLI_PREFIX)) {
        if (userId && k !== cliKey(userId)) continue
        keys.push(k)
      }
      if (k.startsWith(USAGE_PREFIX)) {
        if (userId && !k.startsWith(`${USAGE_PREFIX}${userId}:`)) continue
        keys.push(k)
      }
      if (k.startsWith(LOGS_PREFIX)) {
        if (userId && k !== logsKey(userId)) continue
        keys.push(k)
      }
    }
    for (const k of keys) store.removeItem(k)
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
 * there is nothing key-shaped here to persist to disk.
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

export function isModelGroupsCacheFresh(
  userId: string | null | undefined,
  ttlMs = CACHE_TTL_MS,
): boolean {
  if (!userId) return false
  const entry = readTimed<ModelGroup[]>(modelGroupsKey(userId))
  if (!entry) return false
  return Date.now() - entry.savedAt < ttlMs
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
export type CliCachePayload = { devices: CliDevice[]; providers: CliProvider[] }

export function readCliCache(userId: string | null | undefined): CliCachePayload | null {
  if (!userId) return null
  return readTimed<CliCachePayload>(cliKey(userId))?.data ?? null
}

export function isCliCacheFresh(userId: string | null | undefined, ttlMs = CACHE_TTL_MS): boolean {
  if (!userId) return false
  const entry = readTimed<CliCachePayload>(cliKey(userId))
  if (!entry) return false
  return Date.now() - entry.savedAt < ttlMs
}

export function writeCliCache(userId: string | null | undefined, data: CliCachePayload): void {
  if (!userId) return
  writeTimed(cliKey(userId), data)
}

/**
 * Usage summary cache, one entry per `days` range so switching the range
 * picker can paint that range's own cache immediately. Only the aggregate
 * JSON is ever cached — no prompts/completions/secrets pass through here.
 */
export function readUsageSummaryCache(
  userId: string | null | undefined,
  rangeKey: string | number,
): UsageSummary | null {
  if (!userId) return null
  return readTimed<UsageSummary>(usageKey(userId, rangeKey))?.data ?? null
}

export function isUsageSummaryCacheFresh(
  userId: string | null | undefined,
  rangeKey: string | number,
  ttlMs = CACHE_TTL_MS,
): boolean {
  if (!userId) return false
  const entry = readTimed<UsageSummary>(usageKey(userId, rangeKey))
  if (!entry) return false
  return Date.now() - entry.savedAt < ttlMs
}

export function writeUsageSummaryCache(
  userId: string | null | undefined,
  rangeKey: string | number,
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

export function isLogsCacheFresh(
  userId: string | null | undefined,
  ttlMs = CACHE_TTL_MS,
): boolean {
  if (!userId) return false
  const entry = readTimed<LogsResponse>(logsKey(userId))
  if (!entry) return false
  return Date.now() - entry.savedAt < ttlMs
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

export function isChangelogCacheFresh(ttlMs = CHANGELOG_CACHE_TTL_MS): boolean {
  const entry = readTimed<ChangelogResponse>(CHANGELOG_KEY)
  if (!entry) return false
  return Date.now() - entry.savedAt < ttlMs
}

export function writeChangelogCache(data: ChangelogResponse): void {
  writeTimed(CHANGELOG_KEY, data)
}

// silence unused if tree-shaken elsewhere
export type { CatalogModel }
