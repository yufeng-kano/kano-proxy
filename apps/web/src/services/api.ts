import type {
  AccountsResponse,
  ApiKey,
  CatalogModel,
  CreatedKey,
  LoginStart,
  ModelsResponse,
  ProviderId,
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

export async function createKey(name?: string): Promise<CreatedKey> {
  return request<CreatedKey>("/api/keys", {
    method: "POST",
    body: JSON.stringify({ name: name?.trim() || "default" }),
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

export async function promoteAccount(
  provider: ProviderId,
  id: string,
): Promise<void> {
  await request<{ ok: boolean }>(
    `/api/providers/${provider}/accounts/${encodeURIComponent(id)}/promote`,
    { method: "POST" },
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
