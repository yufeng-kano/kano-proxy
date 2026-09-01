export type ProviderId = "claude-code" | "codex" | "grok" | "antigravity"

/**
 * Where a request goes right now, not which row is first: `limited` is a
 * usage window at 100% waiting on its reset (docs/admin-ui.md § Providers
 * page). The `Primary` badge reads priority order instead.
 */
export type AccountStatus = "active" | "standby" | "limited" | "benched" | "unusable"

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
  /** Display name — already the custom one when the user set it. */
  label: string | null
  /** The user's own name for this account; null = falls back to upstream identity. */
  custom_label: string | null
  account: Record<string, unknown> | null
  usage: { windows: UsageWindow[] } | null
  error: string | null
  stale: boolean
}

/**
 * How a pool or a group orders the candidates a request may run on
 * (docs/providers.md § Routing module). `ordered` — priority order, first
 * usable candidate — is the only value today; future ones (usage balancing)
 * plug into the same field, which is why the UI writes it through a select
 * rather than stating it as text.
 */
export type RoutingStrategy = "ordered"

/** What the server falls back to, and what a payload without the field means. */
export const DEFAULT_ROUTING_STRATEGY: RoutingStrategy = "ordered"

/** Every strategy the UI offers, in display order. */
export const ROUTING_STRATEGIES: RoutingStrategy[] = ["ordered"]

export type AccountsResponse = {
  available: boolean
  accounts: ProviderAccount[]
  models: string[]
  error: string | null
  /**
   * The pool's routing strategy. Optional because a cache entry written before
   * the field existed has none — absent reads as `ordered`, which is what the
   * server would have answered anyway.
   */
  strategy?: RoutingStrategy
}

/** Reset window for a key's spend limit (docs/pricing.md). */
export type SpendLimitInterval = "daily" | "weekly" | "monthly" | "total"

export const SPEND_LIMIT_INTERVALS: SpendLimitInterval[] = [
  "daily",
  "weekly",
  "monthly",
  "total",
]

export type ApiKey = {
  id: string
  name: string
  key_prefix: string
  created_at: string
  last_used_at: string | null
  /** USD ceiling per window; null = unlimited. */
  spend_limit: number | null
  spend_limit_interval: SpendLimitInterval
  /** Whether subscription (OAuth) traffic counts toward the limit. */
  spend_limit_include_oauth: boolean
  /** Estimated USD spent in the current window — GET /api/keys only; null when unreadable. */
  window_spend?: number | null
}

export type CreatedKey = ApiKey & {
  key: string
}

/**
 * Two disjoint shapes behind one response: `authorization_url` for the browser
 * redirect flow (Claude Code), the device-code fields for the poll flows
 * (Codex, Grok).
 */
export type LoginStart = {
  login_id: string
  authorization_url?: string
  user_code?: string
  verification_uri?: string
  verification_uri_complete?: string
  interval?: number
}

/**
 * The builtin provider pools, in display order — the single source of truth
 * for Providers, Models, and the login brand panel.
 *
 * Names and descriptions are **not** here: they are user-facing copy and live
 * in the message catalog under `provider.<id>.name` / `provider.<id>.blurb`
 * (see docs/i18n.md). This list carries only the wire ids and their order.
 */
export const PROVIDERS: { id: ProviderId }[] = [
  { id: "claude-code" },
  { id: "codex" },
  { id: "grok" },
  { id: "antigravity" },
]

export const PROVIDER_IDS: ProviderId[] = PROVIDERS.map((p) => p.id)

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
/** One machine signed in with `kano-proxy init` (docs/cli.md). */
export type CliDevice = {
  id: string
  name: string
  last_seen_at: string | null
  created_at: string
  revoked_at: string | null
}

/** One local endpoint registered by `kano-proxy add` (docs/cli.md). */
export type CliProvider = {
  id: string
  slug: string
  name: string
  format: CustomProviderFormat
  /** Live tunnel state, read through the AgentTunnel DO at request time. */
  connected: boolean
  /** The exposed model ids — the agent report with the whitelist applied. */
  models: string[]
  models_reported: number
  model_filter: string[]
  models_updated_at: string | null
  device_id: string | null
  device_name: string | null
  created_at: string
  updated_at: string
}

/** A pending `kano-proxy init` login, as the authorize view reads it. */
export type CliLoginRequest = {
  id: string
  device_name: string
  expires_at: string
  approved: boolean
  used: boolean
}

export type CustomProvider = {
  id: string
  slug: string
  name: string
  format: CustomProviderFormat
  base_url: string
  /**
   * Complete URL of an Anthropic-shaped `/v1/messages/count_tokens` to post to
   * verbatim — nothing is appended, unlike `base_url` (docs/providers.md
   * § Custom endpoints). Only ever set on `openai`-format rows; `null` means
   * `count_tokens` keeps failing for them, which is the default. Not a secret,
   * so it is returned on read and pre-filled on edit.
   */
  count_tokens_url: string | null
  models_mode: CustomProviderModelsMode
  manual_models: string[]
  /** Non-secret display mask, e.g. "sk-abc…f3a2". Never the plaintext key. */
  key_mask: string | null
  /**
   * The `upstream_accounts` row holding this endpoint's key — what a model
   * group target pins to (docs/admin-ui.md § Groups page). `null` when the
   * account row is missing, which is the one case the picker cannot offer.
   */
  account_id: string | null
  status: CustomProviderStatus
  /** Display-only position in the user's list. The server returns items pre-sorted by it. */
  sort_order: number
  created_at: string
  updated_at: string
}

/**
 * One target of a model group.
 *
 * `account_id` null means the provider's whole pool, with its own priority and
 * failover. A pinned target dispatches on exactly that account and is skipped
 * when it is paused or gone (docs/providers.md § Model groups).
 *
 * `account_label` is resolved at read time and never stored — so an entry with
 * an `account_id` but a **null** label is a pin whose account no longer
 * exists. That combination is a state the UI has to show, not drop.
 */
export type ModelGroupTarget = {
  /** `provider/model` id this target dispatches. */
  model: string
  account_id: string | null
  account_label: string | null
}

/** Why a target cannot take a request right now; `null` while it can. */
export type ModelGroupTargetReason = "benched" | "limit" | "unresolved" | "no_account"

/**
 * One target's live routing state, index-aligned with `targets`.
 *
 * `unusable_until` is an ISO timestamp, or `null` when the recovery time is
 * unknown (and always for `unresolved` / `no_account`, which no clock fixes).
 */
export type ModelGroupTargetRouting = {
  usable: boolean
  reason: ModelGroupTargetReason | null
  unusable_until: string | null
}

/**
 * The current-route indicator (docs/admin-ui.md § Groups page): what the
 * ordered walk would dispatch right now, computed server-side from the same
 * stored facts dispatch uses. `current_target_index` is `null` when no target
 * is usable.
 */
export type ModelGroupRouting = {
  current_target_index: number | null
  targets: ModelGroupTargetRouting[]
}

/**
 * One callable model of a group: the name a client sends as `model` on the
 * group's endpoint, over its own ordered target list.
 */
export type ModelGroupModel = {
  /** The callable id on this group's endpoint — 1-128 chars, no whitespace, `/` allowed. */
  name: string
  /** Ordered targets — array order **is** priority. */
  targets: ModelGroupTarget[]
  /**
   * Live routing state per target. Optional because a cache entry written
   * before the field existed has none — the rows then render without the
   * current/unusable markers rather than breaking.
   */
  routing?: ModelGroupRouting
}

/**
 * A user-defined model group — since v4 a **virtual endpoint**: a URL slug
 * under `/g/` plus the models callable on it. `GET /api/model-groups` item
 * shape — see docs/providers.md § Model groups.
 */
export type ModelGroup = {
  id: string
  /** Display label. Free text, never part of the URL. */
  name: string
  /** The endpoint's URL id: `/g/<slug>/openai/v1` and `/g/<slug>/anthropic`. Mutable. */
  slug: string
  /** The models callable on this endpoint, each with its own target list. */
  models: ModelGroupModel[]
  /** How the group orders its targets. Absent (old cache entry) reads as `ordered`. */
  strategy?: RoutingStrategy
  created_at: string
  updated_at: string
}

/** What a write sends per target: the label is read-only, so it never goes back up. */
export type ModelGroupTargetInput = {
  model: string
  account_id: string | null
}

/** What a write sends per model: name + targets, replacing the whole set. */
export type ModelGroupModelInput = {
  name: string
  targets: ModelGroupTargetInput[]
}

/** Server-side limits, mirrored client-side so a violation is caught before the request (docs/auth.md § Model groups). */
export const MODEL_GROUP_NAME_MAX = 64
export const MODEL_GROUP_MODEL_NAME_MAX = 128
export const MODEL_GROUP_MODELS_MAX = 20
export const MODEL_GROUP_TARGETS_MAX = 20
/** Same shape as a custom-provider slug; no reserved list — `/g/` is its own namespace. */
export const MODEL_GROUP_SLUG_RE = /^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$/

/** `POST /api/custom-providers/test` result — always HTTP 200. */
export type CustomProviderTestResult = {
  ok: boolean
  models_count?: number | null
  sample?: string[]
  note?: string
  error?: string
}

// Usage dashboard — GET /api/usage/summary. See docs/admin-ui.md
// (Dashboard page) and docs/database.md (request_logs NULL token semantics).

/** Range picker granularity: Day (hourly) / Week (daily) / Month (daily). */
export type UsageRangeKind = "day" | "week" | "month"

export type UsageRange = {
  kind: UsageRangeKind
  /** Formatted anchor string for cache key, e.g. "2026-08-28" for day/week, "2026-08" for month */
  anchor: string
  from: string
  to: string
  grain: "hour" | "day"
  /**
   * Minutes east of UTC of the calendar this range was picked in. Sent to the
   * API so it buckets rows in the same calendar (see docs/admin-ui.md).
   */
  offsetMinutes: number
}

/** Legacy range picker options: 24h / 7d / 30d. */
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
  /** Estimated USD over priced rows; null when none is priced (docs/pricing.md). */
  cost: number | null
  /** Rows contributing to `cost` — lets the UI annotate partial coverage. */
  cost_known_requests: number
}

/**
 * One row of the per-model breakdown. `model` is the full "provider/upstream"
 * id.
 *
 * Same **(provider, model)** grain as the series, summed over the whole range:
 * requests served by different accounts, and requests addressed through a
 * group alias, fold into one row (docs/admin-ui.md § Series shape). The
 * per-request account and alias detail lives on the Logs page. Every field
 * below is a sum or a count, so any surface that wants per-model totals
 * re-aggregates these rows exactly.
 */
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
  cost: number | null
  cost_known_requests: number
}

/**
 * One (bucket, provider, model) group. `bucket` is an hour key
 * ("YYYY-MM-DDTHH") when days=1, else a day key ("YYYY-MM-DD"); both UTC.
 * Sparse in both dimensions — a bucket carries one point per model that had
 * traffic, and the client zero-fills the bucket grid. A bucket's totals are
 * the sum over its own model points (see docs/admin-ui.md § Series shape).
 */
export type UsageSeriesPoint = {
  bucket: string
  provider: string
  model: string
  requests: number
  prompt_tokens: number
  completion_tokens: number
  cache_read_input_tokens: number
  /** Requests here with non-null cache_read_input_tokens — separates "0% cached" from "not reported". */
  cache_known_requests: number
  /** Estimated USD over this group's priced rows; null when none is. */
  cost: number | null
}

export type UsageSummary = {
  days: number
  /** ISO UTC inclusive lower bound of the queried range. */
  from: string
  /** ISO UTC inclusive upper bound of the queried range. */
  to?: string
  /** Aggregation grain: "hour" for single days, "day" for multi-day ranges. */
  grain?: "hour" | "day"
  /** Minutes east of UTC the server bucketed in; absent or 0 means UTC keys. */
  offset?: number
  totals: UsageTotals
  /** Sorted by total tokens desc (server-side). */
  models: ModelUsageRow[]
  series: UsageSeriesPoint[]
}

// Request log explorer — GET /api/logs. See docs/admin-ui.md § Logs page.

/**
 * Which kind of upstream served the request: a builtin subscription pool
 * (`oauth`) or a custom endpoint (`api`). Derived server-side from the
 * provider, never stored — so the client keeps no builtin list of its own.
 */
export type LogUsageType = "oauth" | "api"

/**
 * One `request_logs` row, as the log explorer reads it.
 *
 * Nullable fields are *unreported*, not zero (docs/database.md): a token count
 * or a cost of `null` renders as an em dash, never as 0. `account_label` is
 * resolved at read time and never stored, so an id set with a **null** label
 * is an account deleted since the request ran — a state the UI has to show,
 * not drop. The API key follows a different convention: the `api_keys` id is
 * resolved server-side and never returned, so `api_key_name` pairs with the
 * explicit `api_key_removed` boolean instead (docs/admin-ui.md § Logs page).
 */
export type RequestLogRow = {
  id: string
  created_at: string
  provider: string
  /** Canonical `provider/model` id — the expanded target, even when a group alias was called. */
  model: string
  /** The group alias the client addressed; `null` for a direct call. */
  group_name: string | null
  account_id: string | null
  account_label: string | null
  api_key_name: string | null
  /** True when the row points at an `api_keys` id that no longer resolves — the key was deleted since the request ran. */
  api_key_removed: boolean
  usage_type: LogUsageType
  status_code: number
  /** Last upstream status seen; `null` when no upstream answered (docs/logging.md). */
  upstream_status: number | null
  error_code: string | null
  latency_ms: number
  prompt_tokens: number | null
  completion_tokens: number | null
  cache_read_input_tokens: number | null
  cache_creation_input_tokens: number | null
  /** Estimated USD, filled at read time; `null` = unpriced (docs/pricing.md). */
  cost: number | null
}

export type LogsResponse = {
  /** Newest first. */
  rows: RequestLogRow[]
  /** Opaque keyset token for the next page; `null` = end of the list. Never parsed client-side. */
  next_cursor: string | null
}

// Changelog — GET /api/changelog. See docs/changelog.md.

/** One published GitHub Release. */
export type ChangelogRelease = {
  /** Release tag, e.g. "v1.11.0" — carries the leading "v". */
  tag: string
  name: string
  published_at: string
  /** GitHub release page. */
  url: string
  /** Sanitized server-side (twice — GitHub's renderer, then ours). Rendered with `v-html`. */
  body_html: string
}

export type ChangelogResponse = {
  /** Running Worker version, bare SemVer with no "v" prefix, e.g. "1.11.0". */
  current: string
  /** Newest published tag, e.g. "v1.11.0" — MAY carry a leading "v". */
  latest: string | null
  /** Computed server-side by numeric SemVer compare; never recompute client-side. */
  updateAvailable: boolean
  /** Newest first. */
  releases: ChangelogRelease[]
  /** False when GITHUB_REPO is unset/misconfigured — `current` is still valid. */
  available: boolean
  cached: boolean
  /** Last good data served after a failed refetch (deliberate stale-serve). */
  stale: boolean
  error: string | null
}
