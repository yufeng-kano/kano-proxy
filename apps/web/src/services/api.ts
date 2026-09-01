import type {
  AccountsResponse,
  ApiKey,
  CatalogModel,
  ChangelogResponse,
  CliDevice,
  CliLoginRequest,
  CliProvider,
  CreatedKey,
  CustomProvider,
  CustomProviderFormat,
  CustomProviderModelsMode,
  CustomProviderTestResult,
  LoginStart,
  LogsResponse,
  ModelGroup,
  ModelGroupModelInput,
  ModelsResponse,
  ProviderId,
  RoutingStrategy,
  SpendLimitInterval,
  UsageDays,
  UsageSummary,
  User,
} from "@/types"

/**
 * OAuth callback sets the session cookie on the API host (127.0.0.1:8787 locally).
 * Browser calls must hit that same host so credentials:include sends the cookie.
 * Empty string = same-origin / Vite proxy (production same-hostname deploy).
 */
const API_ORIGIN = (import.meta.env.VITE_API_ORIGIN as string | undefined)?.replace(/\/$/, "") ?? ""

function apiUrl(path: string): string {
  if (path.startsWith("http")) return path
  return `${API_ORIGIN}${path.startsWith("/") ? path : `/${path}`}`
}

/**
 * Client-facing LLM base URLs for the current deployment host.
 * Local: VITE_API_ORIGIN (Worker). Production same-hostname: window origin.
 */
export function clientBaseUrls(): { openai: string; anthropic: string } {
  const origin =
    API_ORIGIN ||
    (typeof window !== "undefined" ? window.location.origin : "")
  return {
    openai: `${origin}/openai/v1`,
    anthropic: `${origin}/anthropic`,
  }
}

/**
 * A model group's own base URLs (docs/api.md § Group endpoints) — what a
 * client's `base_url` is set to for that group's virtual endpoint.
 */
export function groupBaseUrls(slug: string): { openai: string; anthropic: string } {
  const origin =
    API_ORIGIN ||
    (typeof window !== "undefined" ? window.location.origin : "")
  return {
    openai: `${origin}/g/${slug}/openai/v1`,
    anthropic: `${origin}/g/${slug}/anthropic`,
  }
}


export class ApiError extends Error {
  status: number
  body: unknown

  constructor(status: number, message: string, body?: unknown) {
    super(message)
    this.name = "ApiError"
    this.status = status
    this.body = body
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(apiUrl(path), {
    credentials: "include",
    ...init,
    headers: {
      accept: "application/json",
      ...(init?.body ? { "content-type": "application/json" } : {}),
      ...init?.headers,
    },
  })

  const text = await res.text()
  let data: unknown = null
  if (text) {
    try {
      data = JSON.parse(text) as unknown
    } catch {
      data = text
    }
  }

  if (!res.ok) {
    const msg =
      typeof data === "object" &&
      data !== null &&
      "error" in data &&
      typeof (data as { error: unknown }).error === "string"
        ? (data as { error: string }).error
        : `Request failed (${res.status})`
    throw new ApiError(res.status, msg, data)
  }

  return data as T
}

export async function fetchMe(): Promise<User | null> {
  try {
    const data = await request<{ user: User | null }>("/api/auth/me")
    return data.user
  } catch (e) {
    if (e instanceof ApiError && e.status === 401) return null
    throw e
  }
}

export async function logout(): Promise<void> {
  await request<{ ok: boolean }>("/api/auth/logout", { method: "POST" })
}

export function loginUrl(): string {
  // Full API origin so Google OAuth starts (and cookie is set) on the Worker host.
  return apiUrl("/api/auth/login")
}

export async function listKeys(): Promise<ApiKey[]> {
  const data = await request<{ keys: ApiKey[] }>("/api/keys")
  return data.keys
}

export async function listModels(opts?: {
  refresh?: boolean
}): Promise<ModelsResponse> {
  const q = opts?.refresh ? "?refresh=true" : ""
  return request<ModelsResponse>(`/api/models${q}`)
}

export type { CatalogModel }

/** Optional per-key spend-limit fields (docs/pricing.md); `spend_limit: null` = unlimited. */
export type KeyLimitFields = {
  spend_limit: number | null
  spend_limit_interval: SpendLimitInterval
  spend_limit_include_oauth: boolean
}

export async function createKey(name?: string, limits?: KeyLimitFields): Promise<CreatedKey> {
  return request<CreatedKey>("/api/keys", {
    method: "POST",
    body: JSON.stringify({ name: name?.trim() || "default", ...limits }),
  })
}

export async function updateKey(
  id: string,
  patch: { name?: string } & Partial<KeyLimitFields>,
): Promise<ApiKey> {
  return request<ApiKey>(`/api/keys/${encodeURIComponent(id)}`, {
    method: "PATCH",
    body: JSON.stringify(patch),
  })
}

export async function revokeKey(id: string): Promise<void> {
  await request<{ ok: boolean }>(`/api/keys/${encodeURIComponent(id)}`, {
    method: "DELETE",
  })
}

export async function listAccounts(
  provider: ProviderId,
  opts?: { refresh?: boolean },
): Promise<AccountsResponse> {
  const q = opts?.refresh ? "?refresh=true" : ""
  return request<AccountsResponse>(`/api/providers/${provider}/accounts${q}`)
}

/**
 * Sets the pool's routing strategy (docs/auth.md). `ordered` is the only value
 * the server accepts today; anything else is a 400. Returns the stored value so
 * the caller paints what was saved rather than what it sent.
 */
export async function setProviderStrategy(
  provider: ProviderId,
  strategy: RoutingStrategy,
): Promise<RoutingStrategy> {
  const data = await request<{ ok: boolean; strategy: RoutingStrategy }>(
    `/api/providers/${provider}`,
    { method: "PATCH", body: JSON.stringify({ strategy }) },
  )
  return data.strategy
}

export async function promoteAccount(
  provider: ProviderId,
  id: string,
): Promise<void> {
  await request<{ ok: boolean }>(
    `/api/providers/${provider}/accounts/${encodeURIComponent(id)}/promote`,
    { method: "POST" },
  )
}

/** Clears the KV bench so the account is eligible again. Not a lock. */
export async function unpauseAccount(
  provider: ProviderId,
  id: string,
): Promise<void> {
  await request<{ ok: boolean }>(
    `/api/providers/${provider}/accounts/${encodeURIComponent(id)}/unpause`,
    { method: "POST" },
  )
}

/** Display-only rename. `null` clears it and the row falls back to the upstream identity. */
export async function renameAccount(
  provider: ProviderId,
  id: string,
  customLabel: string | null,
): Promise<void> {
  await request<{ ok: boolean; custom_label: string | null }>(
    `/api/providers/${provider}/accounts/${encodeURIComponent(id)}`,
    { method: "PATCH", body: JSON.stringify({ custom_label: customLabel }) },
  )
}

export async function removeAccount(
  provider: ProviderId,
  id: string,
): Promise<void> {
  await request<{ ok: boolean }>(
    `/api/providers/${provider}/accounts/${encodeURIComponent(id)}`,
    { method: "DELETE" },
  )
}

export async function startLogin(provider: ProviderId): Promise<LoginStart> {
  return request<LoginStart>(`/api/providers/${provider}/login`, {
    method: "POST",
  })
}

export async function completeLogin(
  provider: ProviderId,
  loginId: string,
  body?: { code?: string; value?: string },
): Promise<{ ok: boolean; token_id?: string }> {
  return request(`/api/providers/${provider}/login/${encodeURIComponent(loginId)}/complete`, {
    method: "POST",
    body: JSON.stringify(body ?? {}),
  })
}

export async function importAccount(
  provider: ProviderId,
  body: {
    access_token: string
    refresh_token?: string
    expires_at?: string
    account_id?: string
    email?: string
    label?: string
  },
): Promise<{ ok: boolean; id: string }> {
  return request(`/api/providers/${provider}/accounts/import`, {
    method: "POST",
    body: JSON.stringify(body),
  })
}

// Custom providers — user-defined BYO OpenAI-/Anthropic-compatible endpoints.
// Session-cookie auth, same as the routes above. See docs/auth.md.

export async function listCustomProviders(): Promise<CustomProvider[]> {
  const data = await request<{ providers: CustomProvider[] }>("/api/custom-providers")
  return data.providers
}

export async function createCustomProvider(body: {
  name: string
  slug: string
  format: CustomProviderFormat
  base_url: string
  api_key: string
  /** `openai` format only; a complete URL, not a base. See docs/providers.md. */
  count_tokens_url?: string
  models_mode?: CustomProviderModelsMode
  manual_models?: string[]
}): Promise<CustomProvider> {
  return request<CustomProvider>("/api/custom-providers", {
    method: "POST",
    body: JSON.stringify(body),
  })
}

/**
 * Omitted/empty `api_key` keeps the stored key. `slug`/`format` are immutable.
 *
 * `count_tokens_url` does **not** follow the key's blank-means-keep rule: it
 * holds no secret, so omitting it keeps the stored value while sending `""`
 * clears it (docs/auth.md § Custom endpoint keys).
 */
export async function updateCustomProvider(
  id: string,
  body: {
    name?: string
    base_url?: string
    api_key?: string
    count_tokens_url?: string
    models_mode?: CustomProviderModelsMode
    manual_models?: string[]
  },
): Promise<CustomProvider> {
  return request<CustomProvider>(`/api/custom-providers/${encodeURIComponent(id)}`, {
    method: "PUT",
    body: JSON.stringify(body),
  })
}

/**
 * Rewrites the display order of every custom endpoint in one call: `ids` must
 * list all of the user's providers exactly once, in the desired order. Returns
 * the full list in the new order. Display only — it does not affect routing.
 */
export async function reorderCustomProviders(ids: string[]): Promise<CustomProvider[]> {
  const data = await request<{ providers: CustomProvider[] }>("/api/custom-providers/order", {
    method: "PUT",
    body: JSON.stringify({ ids }),
  })
  return data.providers
}

/** Removes the provider and its stored key(s). */
export async function deleteCustomProvider(id: string): Promise<void> {
  await request<{ ok: boolean }>(`/api/custom-providers/${encodeURIComponent(id)}`, {
    method: "DELETE",
  })
}

/** Clears the KV bench so the endpoint is eligible again. Not a lock. */
export async function unpauseCustomProvider(id: string): Promise<void> {
  await request<{ ok: boolean }>(
    `/api/custom-providers/${encodeURIComponent(id)}/unpause`,
    { method: "POST" },
  )
}

/**
 * Connectivity probe. Pass unsaved form values (`format`/`base_url`/`api_key`)
 * pre-save, or `{id, base_url?}` to test with a saved provider's stored key.
 * Always resolves — the endpoint responds 200 with `ok:false` on failure.
 */
export async function testCustomProvider(
  body:
    | { format: CustomProviderFormat; base_url: string; api_key: string }
    | { id: string; base_url?: string },
): Promise<CustomProviderTestResult> {
  return request<CustomProviderTestResult>("/api/custom-providers/test", {
    method: "POST",
    body: JSON.stringify(body),
  })
}

// CLI devices and providers — the kano-proxy CLI's server-side management
// (docs/auth.md § CLI devices and providers). Session-cookie auth.

export async function listCliDevices(): Promise<CliDevice[]> {
  const data = await request<{ devices: CliDevice[] }>("/api/cli/devices")
  return data.devices
}

/** Idempotent. Live tunnels die at access-token expiry (docs/cli.md). */
export async function revokeCliDevice(id: string): Promise<void> {
  await request<{ ok: boolean }>(`/api/cli/devices/${encodeURIComponent(id)}/revoke`, {
    method: "POST",
  })
}

export async function listCliProviders(): Promise<CliProvider[]> {
  const data = await request<{ providers: CliProvider[] }>("/api/cli/providers")
  return data.providers
}

/** Display name only — slug and format are immutable. */
export async function renameCliProvider(id: string, name: string): Promise<void> {
  await request<{ ok: boolean }>(`/api/cli/providers/${encodeURIComponent(id)}`, {
    method: "PATCH",
    body: JSON.stringify({ name }),
  })
}

/** Removes the provider and force-closes any live tunnel socket. */
export async function deleteCliProvider(id: string): Promise<void> {
  await request<{ ok: boolean }>(`/api/cli/providers/${encodeURIComponent(id)}`, {
    method: "DELETE",
  })
}

export async function getCliLoginRequest(id: string): Promise<CliLoginRequest> {
  return request<CliLoginRequest>(`/api/cli/login-requests/${encodeURIComponent(id)}`)
}

/** The returned code is shown exactly once — only its hash survives server-side. */
export async function approveCliLoginRequest(id: string): Promise<string> {
  const data = await request<{ ok: boolean; code: string }>(
    `/api/cli/login-requests/${encodeURIComponent(id)}/approve`,
    { method: "POST" },
  )
  return data.code
}

export async function denyCliLoginRequest(id: string): Promise<void> {
  await request<{ ok: boolean }>(`/api/cli/login-requests/${encodeURIComponent(id)}/deny`, {
    method: "POST",
  })
}

// Model groups — virtual endpoints whose model names map to ordered
// provider/model targets. Session-cookie auth, same as the routes above.
// See docs/auth.md § Model groups.

export async function listModelGroups(): Promise<ModelGroup[]> {
  const data = await request<{ groups: ModelGroup[] }>("/api/model-groups")
  return data.groups
}

/**
 * `name` is the display label; `slug` is the endpoint's URL id; `models` are
 * the callable names, each with its targets. Targets go up as
 * `{model, account_id}` objects — a bare string is still accepted by the API
 * as shorthand for an unpinned target, but sending objects keeps the pin
 * explicit, including when it is deliberately null.
 */
export async function createModelGroup(body: {
  name: string
  slug: string
  models: ModelGroupModelInput[]
  strategy?: RoutingStrategy
}): Promise<ModelGroup> {
  return request<ModelGroup>("/api/model-groups", {
    method: "POST",
    body: JSON.stringify(body),
  })
}

/**
 * `models`, when sent, replaces the whole set — there is no per-entry
 * patching, because each model's target order *is* the routing priority and
 * the model set is saved as one unit. Changing `slug` moves the endpoint URL
 * immediately.
 */
export async function updateModelGroup(
  id: string,
  body: {
    name?: string
    slug?: string
    models?: ModelGroupModelInput[]
    strategy?: RoutingStrategy
  },
): Promise<ModelGroup> {
  return request<ModelGroup>(`/api/model-groups/${encodeURIComponent(id)}`, {
    method: "PUT",
    body: JSON.stringify(body),
  })
}

export async function deleteModelGroup(id: string): Promise<void> {
  await request<{ ok: boolean }>(`/api/model-groups/${encodeURIComponent(id)}`, {
    method: "DELETE",
  })
}

// Usage dashboard — session auth, same as the routes above. See docs/admin-ui.md.

/** Aggregates over `request_logs` for a date range or trailing `days` window. */
export async function getUsageSummary(
  params:
    | {
        from?: string
        to?: string
        grain?: "hour" | "day"
        /** Bucket calendar, minutes east of UTC — see docs/admin-ui.md. */
        offsetMinutes?: number
        days?: UsageDays
      }
    | UsageDays = 7,
): Promise<UsageSummary> {
  if (typeof params === "number") {
    return request<UsageSummary>(`/api/usage/summary?days=${params}`)
  }
  const q = new URLSearchParams()
  if (params.from) q.set("from", params.from)
  if (params.to) q.set("to", params.to)
  if (params.grain) q.set("grain", params.grain)
  if (params.offsetMinutes !== undefined) q.set("offset", String(params.offsetMinutes))
  if (params.days !== undefined && !params.from) q.set("days", String(params.days))
  const qs = q.toString()
  return request<UsageSummary>(`/api/usage/summary${qs ? `?${qs}` : ""}`)
}

// Request logs — session auth. See docs/admin-ui.md § Logs page.

/**
 * One page of the caller's own `request_logs` rows, newest first. `cursor` is
 * the previous page's `next_cursor` and is passed straight back — it is opaque
 * by contract, so nothing here reads inside it.
 */
export async function listLogs(params?: {
  limit?: number
  cursor?: string
  /** Exact provider slug; omitted = every provider, including deleted ones. */
  provider?: string
  /** Only rows that failed (`error_code` set or `status_code >= 400`). */
  errorsOnly?: boolean
}): Promise<LogsResponse> {
  const q = new URLSearchParams()
  if (params?.limit !== undefined) q.set("limit", String(params.limit))
  if (params?.cursor) q.set("cursor", params.cursor)
  if (params?.provider) q.set("provider", params.provider)
  if (params?.errorsOnly) q.set("errors", "1")
  const query = q.toString()
  return request<LogsResponse>(`/api/logs${query ? `?${query}` : ""}`)
}

// Changelog — session auth. See docs/changelog.md.

/** Running version + published GitHub Releases. `?refresh=true` bypasses the 1h server-side KV freshness window. */
export async function getChangelog(opts?: {
  refresh?: boolean
}): Promise<ChangelogResponse> {
  const q = opts?.refresh ? "?refresh=true" : ""
  return request<ChangelogResponse>(`/api/changelog${q}`)
}
