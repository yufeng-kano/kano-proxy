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
 * cache-inclusive, so it is stored as-is. `completion_tokens` is likewise
 * already total output inclusive of reasoning tokens (`completion_tokens_details.reasoning_tokens`
 * is reported as a breakdown detail). A missing detail field means
 * unreported: NULL, never 0. See docs/logging.md "Token usage capture".
 */
export function fromOpenAIUsage(u: Record<string, unknown> | null | undefined): NormalizedUsage {
  if (!u) return { ...NULL_USAGE }
  const details = u.prompt_tokens_details as Record<string, unknown> | undefined
  // Responses API shape (native `/openai/v1/responses` path): the cached
  // count sits under `input_tokens_details` instead.
  const inputDetails = u.input_tokens_details as Record<string, unknown> | undefined
  const prompt = num(u.prompt_tokens) ?? num(u.input_tokens)
  const completion = num(u.completion_tokens) ?? num(u.output_tokens)
  return {
    promptTokens: prompt ?? null,
    completionTokens: completion ?? null,
    cacheReadInputTokens: num(details?.cached_tokens) ?? num(inputDetails?.cached_tokens) ?? null,
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

/**
 * Longest partial line kept in memory. A line that outgrows it is skipped,
 * never buffered — capture resumes on the next line (docs/logging.md
 * "Token usage capture").
 */
const MAX_CARRY = 256 * 1024
/** Longest `usage` object scanned out of an over-long Responses terminal line. */
const MAX_USAGE_SCAN = 4 * 1024
/** A Responses SSE terminal event, as it starts a `data:` line. */
const RESPONSES_TERMINAL_PREFIX = /^data:\s*\{\s*"type"\s*:\s*"response\.(completed|incomplete)"/

export type UsageSniffer = {
  /** Never throws — a bad chunk degrades capture, it never fails the request. */
  feed(chunk: Uint8Array): void
  /** null when nothing usable was captured (no usage seen, or carry overflow). */
  finish(): NormalizedUsage | null
  /**
   * Whether the stream reached its documented completion signal (Anthropic
   * `message_stop`; OpenAI `[DONE]` or a chunk carrying a non-null
   * `finish_reason`; Responses `response.completed` / `response.incomplete`)
   * at any point before this was called. False when capture was abandoned
   * (parse failure) — an abandoned sniffer cannot vouch for completeness
   * either way, so it degrades to "not complete" rather than a false
   * positive. See docs/logging.md "Streaming rows".
   */
  complete(): boolean
}

/**
 * Index just past the JSON object starting at `start` (after optional
 * whitespace): -1 when the object has not fully arrived yet, -2 when the
 * value is not an object at all (`"usage":null`) or runs past
 * `MAX_USAGE_SCAN`.
 */
function jsonObjectEnd(s: string, start: number): number {
  let i = start
  while (i < s.length && (s[i] === " " || s[i] === "\n" || s[i] === "\r" || s[i] === "\t")) i++
  if (i >= s.length) return -1
  if (s[i] !== "{") return -2
  let depth = 0
  let inString = false
  for (; i < s.length; i++) {
    if (i - start > MAX_USAGE_SCAN) return -2
    const c = s[i]
    if (inString) {
      if (c === "\\") i++
      else if (c === '"') inString = false
      continue
    }
    if (c === '"') inString = true
    else if (c === "{") depth++
    else if (c === "}") {
      depth--
      if (depth === 0) return i + 1
    }
  }
  return -1
}

/**
 * `chat.completion.chunk` SSE, and the Responses SSE relayed by the native
 * `/openai/v1/responses` path. Usage rides on whichever chunk carries a
 * non-null top-level `usage` — normally only the final one, but the whole
 * object is replaced last-wins if more than one ever does — or on the
 * terminal `response.completed` / `response.incomplete` event.
 */
export function createOpenAISseUsageSniffer(): UsageSniffer {
  const decoder = new TextDecoder()
  let carry = ""
  let abandoned = false
  let usage: Record<string, unknown> | null = null
  /** [DONE], a chunk whose choices[].finish_reason was a real (non-null) string, or a Responses terminal event. */
  let seenCompletion = false
  /** Name from the last `event:` line — the Responses SSE labels its terminal event there too. */
  let eventName = ""
  /**
   * An over-long line in progress. Skipped, unless it is a Responses
   * terminal event: the ChatGPT codex backend echoes enough of the request
   * into `response.completed` that a real Codex CLI turn passes MAX_CARRY,
   * so its small `usage` object is scanned out of the passing bytes rather
   * than the line being buffered.
   */
  let longLine: { terminal: boolean; window: string; found: Record<string, unknown> | null } | null = null

  function processLine(line: string): void {
    if (line.startsWith("event:")) {
      eventName = line.slice(6).trim()
      return
    }
    if (!line.startsWith("data:")) return
    eventName = ""
    const data = line.slice(5).trim()
    if (!data) return
    if (data === "[DONE]") {
      seenCompletion = true
      return
    }
    // Cheap pre-filter before JSON.parse — every other SSE line (a plain
    // content/tool_calls delta) is skipped without ever being parsed.
    if (
      !data.includes('"usage"') &&
      !data.includes('"finish_reason"') &&
      !data.includes('"response.completed"') &&
      !data.includes('"response.incomplete"')
    ) {
      return
    }
    try {
      const json = JSON.parse(data) as {
        type?: unknown
        usage?: unknown
        response?: { usage?: unknown }
        choices?: Array<{ finish_reason?: unknown }>
      }
      if (!json || typeof json !== "object") return
      // Responses SSE relayed by the native `/openai/v1/responses` path: the
      // terminal event carries the usage and is the completion signal.
      if (json.type === "response.completed" || json.type === "response.incomplete") {
        seenCompletion = true
        if (json.response?.usage && typeof json.response.usage === "object") {
          usage = json.response.usage as Record<string, unknown>
        }
        return
      }
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

  function isTerminalLine(lineStart: string): boolean {
    return (
      RESPONSES_TERMINAL_PREFIX.test(lineStart.slice(0, 128)) ||
      eventName === "response.completed" ||
      eventName === "response.incomplete"
    )
  }

  /** Scan a slice of an over-long terminal line for its `usage` object; the last valid one wins. */
  function scanLong(text: string): void {
    const ll = longLine!
    if (!ll.terminal) return
    let buf = ll.window + text
    for (;;) {
      const idx = buf.indexOf('"usage":')
      if (idx === -1) {
        // Keep just enough tail to complete a marker split across chunks.
        ll.window = buf.slice(-7)
        return
      }
      const start = idx + 8
      const end = jsonObjectEnd(buf, start)
      if (end === -1) {
        // Object still arriving — resume from the marker on the next chunk.
        ll.window = buf.slice(idx)
        return
      }
      if (end === -2) {
        buf = buf.slice(start)
        continue
      }
      try {
        const obj = JSON.parse(buf.slice(start, end)) as Record<string, unknown>
        // A tool schema may have a property named `usage` — only an object
        // carrying a numeric token count is the real one.
        if (obj && typeof obj === "object" && (num(obj.input_tokens) !== undefined || num(obj.prompt_tokens) !== undefined)) {
          ll.found = obj
        }
      } catch {
        // Not JSON after all — keep scanning past it.
      }
      buf = buf.slice(end)
    }
  }

  function endLong(): void {
    const ll = longLine!
    longLine = null
    eventName = ""
    if (!ll.terminal) return
    seenCompletion = true
    if (ll.found) usage = ll.found
  }

  function feed(chunk: Uint8Array): void {
    if (abandoned) return
    try {
      let text = decoder.decode(chunk, { stream: true })
      while (text) {
        if (longLine) {
          const nl = text.indexOf("\n")
          if (nl === -1) {
            scanLong(text)
            return
          }
          scanLong(text.slice(0, nl))
          endLong()
          text = text.slice(nl + 1)
          continue
        }
        if (text.indexOf("\n") === -1) {
          carry += text
        } else {
          const lines = (carry + text).split("\n")
          carry = lines.pop() ?? ""
          for (const line of lines) processLine(line)
        }
        text = ""
        if (carry.length > MAX_CARRY) {
          longLine = { terminal: isTerminalLine(carry), window: "", found: null }
          scanLong(carry)
          carry = ""
        }
      }
    } catch {
      abandoned = true
      carry = ""
      longLine = null
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
  /** An over-long line is being skipped up to its newline. */
  let skipping = false
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
      // Malformed line — skip it, keep listening for the next event.
    }
  }

  function feed(chunk: Uint8Array): void {
    if (abandoned) return
    try {
      let text = decoder.decode(chunk, { stream: true })
      while (text) {
        if (skipping) {
          const nl = text.indexOf("\n")
          if (nl === -1) return
          skipping = false
          event = ""
          text = text.slice(nl + 1)
          continue
        }
        carry += text
        text = ""
        const lines = carry.split("\n")
        carry = lines.pop() ?? ""
        for (const line of lines) processLine(line)
        if (carry.length > MAX_CARRY) {
          carry = ""
          skipping = true
        }
      }
    } catch {
      abandoned = true
      carry = ""
      skipping = false
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
