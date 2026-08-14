import type { Env } from "../env"
import type { AcquiredAccount } from "../pool/acquire"
import type { ReasoningEffort } from "../utils/reasoning"

export type UsageWindow = {
  label: string
  /** Percent used, 0–100 (not a 0–1 fraction). Adapters normalize to this. */
  utilization: number | null
  resets_at: string | null
}

export type AccountUsageView = {
  id: string
  priority: number
  status: "active" | "standby" | "benched" | "unusable"
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
  /** Optional native Anthropic count_tokens (same providers as `messages`). Never streams. */
  countTokens?(
    env: Env,
    account: AcquiredAccount,
    body: unknown,
    headers: Headers,
    extras?: { signal?: AbortSignal },
  ): Promise<Response>
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
