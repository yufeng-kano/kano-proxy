/**
 * Normalizes provider-shaped `usage` objects into `request_logs` columns,
 * and incrementally captures usage from an SSE body without buffering the
 * stream itself — only a small bounded partial-line carry.
 *
 * NULL means "unreported", not zero — see the token semantics in
 * docs/database.md. capture matrix: docs/logging.md.
 */

export type NormalizedUsage = {
  promptTokens: number | null
  completionTokens: number | null
  cacheReadInputTokens: number | null
  cacheCreationInputTokens: number | null
}

export const NULL_USAGE: Readonly<NormalizedUsage> = {
  promptTokens: null,
  completionTokens: null,
  cacheReadInputTokens: null,
  cacheCreationInputTokens: null,
}

function num(v: unknown): number | undefined {
  return typeof v === "number" ? v : undefined
}

/**
 * OpenAI Chat Completions-shaped `usage` — also the shape this proxy's own
 * converters build for claude-code / custom-anthropic / codex on
 * `/openai/v1` (see openai_anthropic.ts / codex_openai.ts). OpenAI-compatible
 * upstreams can report cache writes in
 * `prompt_tokens_details.cache_write_tokens`; converted responses retain the
 * proxy's `cache_creation_input_tokens` extension. `prompt_tokens` is already
 * cache-inclusive, so it is stored as-is. A missing detail field means
 * unreported: NULL, never 0. `completion_tokens_details.reasoning_tokens`
 * (grok's `include_reasoning`, or any upstream that reports it) is added
 * into `completionTokens` when both it and `completion_tokens` itself are
 * present — see docs/logging.md "Token usage capture".
 */
export function fromOpenAIUsage(u: Record<string, unknown> | null | undefined): NormalizedUsage {
  if (!u) return { ...NULL_USAGE }
  const details = u.prompt_tokens_details as Record<string, unknown> | undefined
  const completionDetails = u.completion_tokens_details as Record<string, unknown> | undefined
  const completionBase = num(u.completion_tokens)
  const reasoningTokens = num(completionDetails?.reasoning_tokens)
  return {
    promptTokens: num(u.prompt_tokens) ?? null,
    completionTokens: completionBase != null ? completionBase + (reasoningTokens ?? 0) : null,
    cacheReadInputTokens: num(details?.cached_tokens) ?? null,
    cacheCreationInputTokens:
      num(details?.cache_write_tokens) ?? num(u.cache_creation_input_tokens) ?? null,
  }
}

/**
 * Anthropic Messages-shaped `usage`. `input_tokens` excludes cache reads and
 * writes, so `promptTokens` sums all three into the normalized *total* this
 * proxy stores (docs/database.md). Anthropic always defines the cache
 * fields on a real usage object, so a missing one there defaults to 0 —
 * only a wholly absent usage object means unreported (NULL).
 */
export function fromAnthropicUsage(u: Record<string, unknown> | null | undefined): NormalizedUsage {
  if (!u) return { ...NULL_USAGE }
  const cacheRead = num(u.cache_read_input_tokens) ?? 0
  const cacheCreation = num(u.cache_creation_input_tokens) ?? 0
  return {
    promptTokens: (num(u.input_tokens) ?? 0) + cacheRead + cacheCreation,
    completionTokens: num(u.output_tokens) ?? null,
    cacheReadInputTokens: cacheRead,
    cacheCreationInputTokens: cacheCreation,
  }
}

/** Safety valve for a pathological upstream line with no newline in sight. */
const MAX_CARRY = 256 * 1024

export type UsageSniffer = {
  /** Never throws — a bad chunk degrades capture, it never fails the request. */
  feed(chunk: Uint8Array): void
  /** null when nothing usable was captured (no usage seen, or carry overflow). */
  finish(): NormalizedUsage | null
  /**
   * Whether the stream reached its documented completion signal (Anthropic
   * `message_stop`; OpenAI `[DONE]` or a chunk carrying a non-null
   * `finish_reason`) at any point before this was called. False when
   * capture was abandoned (carry overflow / parse failure) — an abandoned
   * sniffer cannot vouch for completeness either way, so it degrades to
   * "not complete" rather than a false positive. See docs/logging.md
   * "Streaming rows".
   */
  complete(): boolean
}

/**
 * `chat.completion.chunk` SSE. Usage rides on whichever chunk carries a
 * non-null top-level `usage` — normally only the final one, but the whole
 * object is replaced last-wins if more than one ever does.
 */
export function createOpenAISseUsageSniffer(): UsageSniffer {
  const decoder = new TextDecoder()
  let carry = ""
  let abandoned = false
  let usage: Record<string, unknown> | null = null
  /** [DONE], or a chunk whose choices[].finish_reason was a real (non-null) string. */
  let seenCompletion = false

  function processLine(line: string): void {
    if (!line.startsWith("data:")) return
    const data = line.slice(5).trim()
    if (!data) return
    if (data === "[DONE]") {
      seenCompletion = true
      return
    }
    // Cheap pre-filter before JSON.parse — every other SSE line (a plain
    // content/tool_calls delta) is skipped without ever being parsed.
    if (!data.includes('"usage"') && !data.includes('"finish_reason"')) return
    try {
      const json = JSON.parse(data) as {
        usage?: unknown
        choices?: Array<{ finish_reason?: unknown }>
      }
      if (!json || typeof json !== "object") return
      if (json.usage && typeof json.usage === "object") {
        usage = json.usage as Record<string, unknown>
      }
      if (Array.isArray(json.choices)) {
        for (const choice of json.choices) {
          if (typeof choice?.finish_reason === "string") {
            seenCompletion = true
            break
          }
        }
      }
    } catch {
      // Malformed line — skip it, keep listening for the next one.
    }
  }

  function feed(chunk: Uint8Array): void {
    if (abandoned) return
    try {
      carry += decoder.decode(chunk, { stream: true })
      const lines = carry.split("\n")
      carry = lines.pop() ?? ""
      if (carry.length > MAX_CARRY) {
        abandoned = true
        carry = ""
        return
      }
      for (const line of lines) processLine(line)
    } catch {
      abandoned = true
      carry = ""
    }
  }

  function finish(): NormalizedUsage | null {
    if (abandoned || !usage) return null
    return fromOpenAIUsage(usage)
  }

  function complete(): boolean {
    return !abandoned && seenCompletion
  }

  return { feed, finish, complete }
}

/**
 * Anthropic Messages SSE. `message_start` seeds the input-side counts (+
 * cache fields), `message_delta` carries the output-side count — and, on
 * newer API revisions, may repeat cumulative input/cache fields too. Merged
 * field-wise: the last non-undefined value seen for each field wins.
 */
export function createAnthropicSseUsageSniffer(): UsageSniffer {
  const decoder = new TextDecoder()
  let carry = ""
  let abandoned = false
  let event = ""
  let seen = false
  let seenMessageStop = false
  let inputTokens: number | undefined
  let outputTokens: number | undefined
  let cacheReadInputTokens: number | undefined
  let cacheCreationInputTokens: number | undefined

  function merge(partial: Record<string, unknown> | undefined): void {
    if (!partial) return
    seen = true
    const i = num(partial.input_tokens)
    if (i !== undefined) inputTokens = i
    const o = num(partial.output_tokens)
    if (o !== undefined) outputTokens = o
    const r = num(partial.cache_read_input_tokens)
    if (r !== undefined) cacheReadInputTokens = r
    const c = num(partial.cache_creation_input_tokens)
    if (c !== undefined) cacheCreationInputTokens = c
  }

  function processLine(line: string): void {
    if (line.startsWith("event:")) {
      event = line.slice(6).trim()
      return
    }
    if (!line.startsWith("data:")) return
    const data = line.slice(5).trim()
    const currentEvent = event
    event = ""
    if (!data) return
    // message_stop's payload (`{"type":"message_stop"}`) never carries a
    // "usage" substring, so this check must happen before that fast filter.
    if (currentEvent === "message_stop") {
      seenMessageStop = true
      return
    }
    if (!data.includes('"usage"')) return
    try {
      const json = JSON.parse(data) as Record<string, unknown>
      if (currentEvent === "message_start") {
        const message = json.message as Record<string, unknown> | undefined
        merge(message?.usage as Record<string, unknown> | undefined)
      } else if (currentEvent === "message_delta") {
        merge(json.usage as Record<string, unknown> | undefined)
      }
    } catch {
      // Malformed line — skip it, keep listening for the next one.
    }
  }

  function feed(chunk: Uint8Array): void {
    if (abandoned) return
    try {
      carry += decoder.decode(chunk, { stream: true })
      const lines = carry.split("\n")
      carry = lines.pop() ?? ""
      if (carry.length > MAX_CARRY) {
        abandoned = true
        carry = ""
        event = ""
        return
      }
      for (const line of lines) processLine(line)
    } catch {
      abandoned = true
      carry = ""
    }
  }

  function finish(): NormalizedUsage | null {
    if (abandoned || !seen) return null
    return fromAnthropicUsage({
      input_tokens: inputTokens,
      output_tokens: outputTokens,
      cache_read_input_tokens: cacheReadInputTokens,
      cache_creation_input_tokens: cacheCreationInputTokens,
    })
  }

  function complete(): boolean {
    return !abandoned && seenMessageStop
  }

  return { feed, finish, complete }
}
