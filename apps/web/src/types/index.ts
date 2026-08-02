export type ProviderId = "claude-code" | "codex" | "grok"

export type AccountStatus = "active" | "standby" | "benched" | "unusable"

export type User = {
  id: string
  email: string
  name: string | null
  picture_url: string | null
}

export type UsageWindow = {
  label: string
  utilization: number | null
  resets_at: string | null
}

export type ProviderAccount = {
  id: string
  priority: number
  status: AccountStatus
  label: string | null
  account: Record<string, unknown> | null
  usage: { windows: UsageWindow[] } | null
  error: string | null
  stale: boolean
}

export type AccountsResponse = {
  available: boolean
  accounts: ProviderAccount[]
  models: string[]
  error: string | null
}

export type ApiKey = {
  id: string
  name: string
  key_prefix: string
  created_at: string
  last_used_at: string | null
}

export type CreatedKey = ApiKey & {
  key: string
}

export type LoginStart = {
  login_id: string
  authorization_url?: string
  user_code?: string
  verification_uri?: string
  verification_uri_complete?: string
  interval?: number
}

export const PROVIDERS: { id: ProviderId; name: string; blurb: string }[] = [
  { id: "claude-code", name: "Claude Code", blurb: "Anthropic subscription OAuth" },
  { id: "codex", name: "Codex", blurb: "ChatGPT / Codex OAuth" },
  { id: "grok", name: "Grok", blurb: "xAI SuperGrok device code" },
]

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

export type ModelsResponse = {
  object: "list"
  data: CatalogModel[]
  providers?: Array<{
    provider: string
    count: number
    error: string | null
    cached: boolean
  }>
  openai_base?: string
  anthropic_base?: string
}

/** Wire format a custom endpoint speaks. Immutable after creation. */
export type CustomProviderFormat = "openai" | "anthropic"

export type CustomProviderModelsMode = "auto" | "manual"

/** Only two states surfaced for custom cards — no standby/unusable nuance. */
export type CustomProviderStatus = "active" | "benched"

/** User-defined BYO OpenAI-/Anthropic-compatible upstream. `GET /api/custom-providers` item shape. */
export type CustomProvider = {
  id: string
  slug: string
  name: string
  format: CustomProviderFormat
  base_url: string
  models_mode: CustomProviderModelsMode
  manual_models: string[]
  /** Non-secret display mask, e.g. "sk-abc…f3a2". Never the plaintext key. */
  key_mask: string | null
  status: CustomProviderStatus
  created_at: string
  updated_at: string
}

/** `POST /api/custom-providers/test` result — always HTTP 200. */
export type CustomProviderTestResult = {
  ok: boolean
  models_count?: number | null
  sample?: string[]
  note?: string
  error?: string
}

// Usage dashboard — GET /api/usage/summary?days=1|7|30. See docs/admin-ui.md
// (Dashboard page) and docs/database.md (request_logs NULL token semantics).

/** Range picker options: 24h / 7d / 30d. */
export type UsageDays = 1 | 7 | 30

export type UsageTotals = {
  requests: number
  errors: number
  avg_latency_ms: number | null
  prompt_tokens: number
  completion_tokens: number
  cache_read_input_tokens: number
  cache_creation_input_tokens: number
  /** Σcache_read_input_tokens / Σprompt_tokens over cache-known rows; null when none are cache-known. */
  cache_rate: number | null
  /** Requests with non-null token fields (used in the token aggregates). */
  usage_known_requests: number
  /** Requests with non-null cache_read_input_tokens (used in cache_rate). */
  cache_known_requests: number
}

/** One row of the per-model breakdown. `model` is the full "provider/upstream" id. */
export type ModelUsageRow = {
  provider: string
  model: string
  requests: number
  errors: number
  prompt_tokens: number
  completion_tokens: number
  cache_read_input_tokens: number
  cache_creation_input_tokens: number
  cache_rate: number | null
  usage_known_requests: number
  cache_known_requests: number
}

/** One time-series bucket. `bucket` is an hour key ("YYYY-MM-DDTHH") when days=1, else a day key ("YYYY-MM-DD"); both UTC. Sparse — missing buckets are zero-filled client-side. */
export type UsageSeriesPoint = {
  bucket: string
  requests: number
  prompt_tokens: number
  completion_tokens: number
  cache_read_input_tokens: number
}

export type UsageSummary = {
  days: UsageDays
  /** ISO UTC inclusive lower bound of the queried range. */
  from: string
  totals: UsageTotals
  /** Sorted by total tokens desc (server-side). */
  models: ModelUsageRow[]
  series: UsageSeriesPoint[]
}
