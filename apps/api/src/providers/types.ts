import type { Env } from "../env"
import type { AcquiredAccount } from "../pool/acquire"
import type { ReasoningEffort } from "../utils/reasoning"

export type UsageWindow = {
  label: string
  /** Percent used, 0–100 (not a 0–1 fraction). Adapters normalize to this. */
  utilization: number | null
  resets_at: string | null
}

/**
 * How an account reads on the admin Providers page — computed at read time
 * from priority order plus the router's own facts, never stored
 * (docs/admin-ui.md § Providers page): `limited` is a usage window at
 * `utilization >= 100` waiting on its reset, which is not a bench.
 * `active_no_fable` / `active_fable` exist only on Claude Code pools: the
 * first usable account when its seat cannot serve Fable, and the first
 * usable Fable-eligible account below it (docs/providers.md § Claude Code
 * "Fable seat eligibility").
 */
export type AccountStatus =
  | "active"
  | "active_no_fable"
  | "active_fable"
  | "standby"
  | "limited"
  | "benched"
  | "unusable"

export type AccountUsageView = {
  id: string
  priority: number
  status: AccountStatus
  label: string | null
  custom_label: string | null
  account: Record<string, unknown> | null
  usage: { windows: UsageWindow[] } | null
  error: string | null
  stale: boolean
}

/**
 * Opaque client-supplied affinity ids, forwarded verbatim upstream so the
 * provider can keep a conversation on one shard and hit its prompt cache.
 * Never generated here: a wrong-but-stable id is worse than none.
 */
export type AffinityIds = {
  convId?: string
  sessionId?: string
  turnIdx?: string
}

export type ChatCompletionRequest = {
  model: string
  rawModel: string
  upstreamModel: string
  messages: unknown[]
  stream?: boolean
  max_tokens?: number
  tools?: unknown
  tool_choice?: unknown
  response_format?: unknown
  reasoning_effort?: ReasoningEffort
  /** Forwarded to `grok` (defaults to 1 when unset) and `claude-code` (clamped to [0, 1]); ignored by `codex`. */
  temperature?: number
  /** Forwarded to `grok` and `claude-code` only when present — no default. Ignored by `codex`. */
  top_p?: number
  /** OpenAI `stop` / Anthropic `stop_sequences`, forwarded verbatim. */
  stop?: string[]
  /** OpenAI Chat Completions field. Forwarded to `codex` only; ignored elsewhere. */
  prompt_cache_key?: string
  affinity?: AffinityIds
  /**
   * Set only by `POST /openai/v1/responses` when every resolved candidate is
   * codex (docs/api.md § `POST /openai/v1/responses`): the client's own
   * Responses body. The codex adapter forwards it upstream after its usual
   * fix-ups and returns the upstream Responses SSE unconverted; no other
   * adapter ever sees it, because the route never sets it for them.
   */
  responsesBody?: Record<string, unknown>
  /**
   * The OpenAI Chat Completions-shaped body this request came from — the raw
   * client JSON on `/openai/v1`, or its Anthropic→OpenAI conversion on
   * `/anthropic`. Built-in adapters build their upstream body from the named
   * fields above and ignore this; the custom-openai adapter forwards it
   * near-verbatim (only rewriting `model`) so unmodeled fields like
   * `temperature` still reach the upstream, unlike built-ins which strip it.
   */
  rawBody: Record<string, unknown>
}

export type UpstreamModel = {
  id: string
  display_name: string | null
}

export type ProviderAdapter = {
  /** Builtin `ProviderId`, or a custom provider's slug for BYO adapters. */
  id: string
  /**
   * How an OpenAI `input_audio` part reaches this upstream (docs/api.md
   * § Audio input). `"convert"` — the adapter builds a native audio part out
   * of it; `"passthrough"` — the client's own part rides along in the
   * forwarded body and the upstream judges it. **Absent** means the wire this
   * adapter builds has nowhere to put audio, and the OpenAI route answers
   * `400 unsupported_modality` rather than dropping the part silently.
   */
  audioInput?: "convert" | "passthrough"
  /**
   * Forward OpenAI-shaped chat completion using acquired credential.
   * `extras` mirrors `messages()`: `apiKeyId` scopes per-caller KV state and
   * `waitUntil` keeps those writes alive past the Response on Workers. Both
   * are optional — an adapter that keeps no state ignores them.
   */
  chatCompletions(
    env: Env,
    account: AcquiredAccount,
    req: ChatCompletionRequest,
    extras?: {
      apiKeyId?: string | null
      waitUntil?: (promise: Promise<unknown>) => void
      /** Dispatch-scoped deadline for waiting on upstream response headers. */
      signal?: AbortSignal
    },
  ): Promise<Response>
  /**
   * Optional Anthropic Messages entry (claude-code / custom-anthropic native
   * passthrough, or grok Responses conversion). `extras.waitUntil` keeps KV
   * writes alive past the Response on Workers.
   */
  messages?(
    env: Env,
    account: AcquiredAccount,
    body: unknown,
    headers: Headers,
    extras?: { waitUntil?: (promise: Promise<unknown>) => void; signal?: AbortSignal },
  ): Promise<Response>
  /** Optional OpenAI-compatible audio transcription (STT) entry (custom-openai). */
  audioTranscriptions?(
    env: Env,
    account: AcquiredAccount,
    formData: FormData,
    rawModel: string,
    upstreamModel: string,
    extras?: { signal?: AbortSignal },
  ): Promise<Response>
  /** Optional native Anthropic count_tokens (same providers as `messages`). Never streams. */
  countTokens?(
    env: Env,
    account: AcquiredAccount,
    body: unknown,
    headers: Headers,
    extras?: { signal?: AbortSignal },
  ): Promise<Response>
  /**
   * Whether an account with these stored profile facts can serve
   * `upstreamModel` at all (docs/providers.md § Routing module
   * "Candidates"). Absent means every account is eligible. `meta` is
   * `account_meta_json` with the usage snapshot's `account` merged over it,
   * or `null` when neither exists. Must fail open on missing facts — a
   * wrong `false` silences an account that would have worked.
   */
  supportsModel?(meta: Record<string, unknown> | null, upstreamModel: string): boolean
  /** Live model list from upstream when the provider has one. Empty if none. */
  listModels?(
    env: Env,
    account: AcquiredAccount,
  ): Promise<{ models: UpstreamModel[]; error?: string | null }>
  fetchUsage?(env: Env, account: AcquiredAccount): Promise<{
    windows: UsageWindow[]
    account: Record<string, unknown>
    stale?: boolean
    error?: string | null
    /** Usage endpoint blocked (e.g. chatgpt bot wall); account may still be usable */
    edgeBlocked?: boolean
  }>
  refreshIfNeeded?(
    env: Env,
    account: AcquiredAccount,
  ): Promise<AcquiredAccount>
}
