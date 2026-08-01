import type { Env, ProviderId } from "../env"
import type { AcquiredAccount } from "../pool/acquire"
import type { ReasoningEffort } from "../utils/reasoning"

export type UsageWindow = {
  label: string
  utilization: number | null
  resets_at: string | null
}

export type AccountUsageView = {
  id: string
  priority: number
  status: "active" | "standby" | "benched" | "unusable"
  label: string | null
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
  /** OpenAI `stop` / Anthropic `stop_sequences`, forwarded verbatim. */
  stop?: string[]
  /** OpenAI Chat Completions field. Forwarded to `codex` only; ignored elsewhere. */
  prompt_cache_key?: string
  affinity?: AffinityIds
}

export type UpstreamModel = {
  id: string
  display_name: string | null
}

export type ProviderAdapter = {
  id: ProviderId
  /** Forward OpenAI-shaped chat completion using acquired credential. */
  chatCompletions(
    env: Env,
    account: AcquiredAccount,
    req: ChatCompletionRequest,
  ): Promise<Response>
  /** Optional native Anthropic Messages (claude-code only). */
  messages?(
    env: Env,
    account: AcquiredAccount,
    body: unknown,
    headers: Headers,
  ): Promise<Response>
  /** Optional native Anthropic count_tokens (claude-code only). Never streams. */
  countTokens?(
    env: Env,
    account: AcquiredAccount,
    body: unknown,
    headers: Headers,
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
