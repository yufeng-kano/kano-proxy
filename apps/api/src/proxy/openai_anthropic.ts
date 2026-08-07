/**
 * OpenAI Chat Completions ↔ Anthropic Messages conversion.
 * OpenAI→Claude: never invent cache_control.
 * Anthropic→OpenAI (non-Claude providers): strip all cache_control (no equivalent).
 */

/** Deep-drop every `cache_control` key. Used only on Anthropic→OpenAI convert. */
export function stripCacheControl(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(stripCacheControl)
  if (value && typeof value === "object") {
    const out: Record<string, unknown> = {}
    for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
      if (k === "cache_control") continue
      out[k] = stripCacheControl(v)
    }
    return out
  }
  return value
}

/**
 * Anthropic `metadata.user_id` → the internal request's `prompt_cache_key`
 * (codex-only effect; see docs/api.md "Prompt cache"). The Messages wire
 * format has no prompt_cache_key field, but Claude Code sends a stable
 * per-session id here — the per-conversation granularity the upstream cache
 * router needs. Its exact shape is not relied on anywhere: the production
 * 400 (`got 150`) proved it carries no `_session_<uuid>` tail, so downstream
 * code treats it as an opaque string and fits it by length. Guard
 * mirrors Anthropic's own metadata.user_id limit (≤ 256 chars); anything
 * else yields undefined — a client-supplied id is translated, never invented.
 */
export function promptCacheKeyFromAnthropicMetadata(
  body: Record<string, unknown>,
): string | undefined {
  const metadata = body.metadata
  if (!metadata || typeof metadata !== "object" || Array.isArray(metadata)) return undefined
  const userId = (metadata as { user_id?: unknown }).user_id
  if (typeof userId !== "string") return undefined
  const trimmed = userId.trim()
  if (!trimmed || trimmed.length > 256) return undefined
  return trimmed
}

/**
 * Anthropic Messages request → OpenAI Chat Completions fields.
 * Strips all cache_control. Does not invent affinity headers.
 */
export function anthropicToOpenAIChatRequest(body: Record<string, unknown>): {
  messages: unknown[]
  max_tokens?: number
  stream?: boolean
  tools?: unknown
  tool_choice?: unknown
  response_format?: unknown
  reasoning_effort?: unknown
  temperature?: number
  top_p?: number
  stop?: string[]
} {
  const cleaned = stripCacheControl(body) as Record<string, unknown>
  const messages: unknown[] = []

  const system = cleaned.system
  if (typeof system === "string" && system) {
    messages.push({ role: "system", content: system })
  } else if (Array.isArray(system)) {
    // Separate system blocks are distinct instructions upstream; joining them
    // with "" would run the last line of one into the first of the next.
    const text = system
      .map((b) => {
        if (b && typeof b === "object" && (b as { type?: string }).type === "text") {
          return String((b as { text?: string }).text ?? "")
        }
        return ""
      })
      .filter((t) => t)
      .join("\n\n")
    if (text) messages.push({ role: "system", content: text })
  }

  const inMessages = (cleaned.messages as Array<Record<string, unknown>>) ?? []
  for (const m of inMessages) {
    const role = String(m.role ?? "")
    if (role === "assistant") {
      const blocks = Array.isArray(m.content) ? (m.content as Array<Record<string, unknown>>) : null
      if (blocks) {
        let text = ""
        let reasoning = ""
        const tool_calls: Array<Record<string, unknown>> = []
        for (const b of blocks) {
          if (b.type === "text") text += String(b.text ?? "")
          // Round-trip a prior converted turn's thinking block back into the
          // de-facto reasoning_content field before replay upstream (grok
          // accepts this echoed back; codex's request builder ignores the
          // field; custom-openai forwards it verbatim). redacted_thinking has
          // no plaintext to round-trip and is dropped, not an error.
          if (b.type === "thinking") reasoning += String(b.thinking ?? "")
          if (b.type === "tool_use") {
            tool_calls.push({
              id: b.id,
              type: "function",
              function: {
                name: b.name,
                arguments: JSON.stringify(b.input ?? {}),
              },
            })
          }
        }
        const msg: Record<string, unknown> = {
          role: "assistant",
          content: text || null,
        }
        if (reasoning) msg.reasoning_content = reasoning
        if (tool_calls.length) msg.tool_calls = tool_calls
        messages.push(msg)
      } else {
        messages.push({ role: "assistant", content: contentToText(m.content) })
      }
      continue
    }
    if (role === "user") {
      const blocks = Array.isArray(m.content) ? (m.content as Array<Record<string, unknown>>) : null
      if (blocks && blocks.some((b) => b.type === "tool_result")) {
        for (const b of blocks) {
          if (b.type === "tool_result") {
            const { toolMessage, imageMessage } = toolResultToOpenAI(b.tool_use_id, b.content)
            messages.push(toolMessage)
            if (imageMessage) messages.push(imageMessage)
          }
        }
        const nonTool = blocks.filter((b) => b.type !== "tool_result")
        if (nonTool.length) {
          messages.push({
            role: "user",
            content: anthropicBlocksToOpenAIContent(nonTool),
          })
        }
        continue
      }
      if (blocks) {
        messages.push({
          role: "user",
          content: anthropicBlocksToOpenAIContent(blocks),
        })
      } else {
        messages.push({ role: "user", content: contentToText(m.content) })
      }
      continue
    }
    // pass through unknown roles best-effort
    messages.push({ role, content: contentToText(m.content) })
  }

  const out: {
    messages: unknown[]
    max_tokens?: number
    stream?: boolean
    tools?: unknown
    tool_choice?: unknown
    response_format?: unknown
    reasoning_effort?: unknown
    temperature?: number
    top_p?: number
    stop?: string[]
  } = {
    messages,
    stream: !!cleaned.stream,
  }
  if (typeof cleaned.max_tokens === "number") out.max_tokens = cleaned.max_tokens
  if (typeof cleaned.temperature === "number") out.temperature = cleaned.temperature
  if (typeof cleaned.top_p === "number") out.top_p = cleaned.top_p

  if (Array.isArray(cleaned.stop_sequences)) {
    const stop = cleaned.stop_sequences.filter(
      (s): s is string => typeof s === "string" && s.length > 0,
    )
    if (stop.length) out.stop = stop
  }

  if (Array.isArray(cleaned.tools)) {
    const tools: unknown[] = []
    for (const t of cleaned.tools as Array<Record<string, unknown>>) {
      if (!t || typeof t !== "object" || t.function) {
        // Not a plain object, or already OpenAI-shaped — pass through.
        tools.push(t)
        continue
      }
      const hasInputSchema = "input_schema" in t
      const type = typeof t.type === "string" ? t.type : undefined
      if (type && type !== "custom" && !hasInputSchema) {
        // Anthropic server-side tool (web_search_*, bash_*, text_editor_*,
        // computer_*, code_execution_*, ...). Grok/codex cannot execute
        // these; forwarding a fake empty-schema function would invite an
        // uncallable tool call, so drop it instead of converting.
        continue
      }
      if (t.name) {
        // Client-defined tool: has input_schema, or type is absent/"custom".
        tools.push({
          type: "function",
          function: {
            name: t.name,
            description: t.description,
            parameters: t.input_schema ?? { type: "object", properties: {} },
          },
        })
        continue
      }
      tools.push(t)
    }
    out.tools = tools
  }
  if (cleaned.tool_choice) {
    out.tool_choice = mapAnthropicToolChoice(cleaned.tool_choice)
  }
  if (cleaned.output_format && typeof cleaned.output_format === "object") {
    const of = cleaned.output_format as { type?: string; schema?: unknown }
    if (of.type === "json_schema" && of.schema) {
      out.response_format = {
        type: "json_schema",
        json_schema: { name: "response", schema: of.schema },
      }
    }
  }
  // Reasoning is effort-only: `thinking`/`budget_tokens` is never read here.
  // `reasoning_effort` is a nonstandard extension some Anthropic-shaped
  // clients send directly; `output_config.effort` (the native Anthropic
  // field) is the fallback when that is absent. Explicit reasoning_effort
  // always wins.
  const outputConfig = cleaned.output_config as { effort?: unknown } | undefined
  if (cleaned.reasoning_effort != null) {
    out.reasoning_effort = cleaned.reasoning_effort
  } else if (typeof outputConfig?.effort === "string") {
    out.reasoning_effort = outputConfig.effort
  }

  return out
}

function anthropicBlocksToOpenAIContent(
  blocks: Array<Record<string, unknown>>,
): string | Array<Record<string, unknown>> {
  const parts: Array<Record<string, unknown>> = []
  let onlyText = true
  let textJoined = ""
  for (const b of blocks) {
    if (b.type === "text") {
      const t = String(b.text ?? "")
      textJoined += t
      parts.push({ type: "text", text: t })
    } else if (b.type === "image") {
      onlyText = false
      const src = b.source as { type?: string; media_type?: string; data?: string; url?: string } | undefined
      if (src?.type === "base64" && src.data) {
        parts.push({
          type: "image_url",
          image_url: {
            url: `data:${src.media_type || "image/png"};base64,${src.data}`,
          },
        })
      } else if (src?.type === "url" && src.url) {
        parts.push({ type: "image_url", image_url: { url: src.url } })
      }
    } else {
      onlyText = false
      parts.push(b)
    }
  }
  if (onlyText) return textJoined
  return parts.length ? parts : textJoined
}

/**
 * Anthropic `tool_result` → OpenAI `role: "tool"` message. OpenAI's tool
 * message has no multi-part (text + image) shape, so when the result
 * contains `image` blocks: the tool message keeps the text plus a short
 * placeholder line, and a separate `role: "user"` message carries the
 * image(s) right after it — the same image conversion `anthropicBlocksToOpenAIContent`
 * uses for regular user content. A tool_result with no images is unchanged.
 */
function toolResultToOpenAI(
  toolUseId: unknown,
  content: unknown,
): { toolMessage: Record<string, unknown>; imageMessage: Record<string, unknown> | null } {
  const blocks = Array.isArray(content) ? (content as Array<Record<string, unknown>>) : null
  const images = blocks ? blocks.filter((b) => b.type === "image") : []
  if (!blocks || images.length === 0) {
    return {
      toolMessage: {
        role: "tool",
        tool_call_id: toolUseId,
        content: contentToText(content),
      },
      imageMessage: null,
    }
  }
  const text = blocks
    .filter((b) => b.type === "text")
    .map((b) => String(b.text ?? ""))
    .join("")
  const placeholder =
    images.length === 1 ? "[image attached below]" : `[${images.length} images attached below]`
  const imageParts = anthropicBlocksToOpenAIContent(images)
  return {
    toolMessage: {
      role: "tool",
      tool_call_id: toolUseId,
      content: text ? `${text}\n${placeholder}` : placeholder,
    },
    imageMessage: {
      role: "user",
      content: [
        { type: "text", text: `[Image(s) from tool result ${toolUseId}]` },
        ...(Array.isArray(imageParts) ? imageParts : []),
      ],
    },
  }
}

function mapAnthropicToolChoice(tc: unknown): unknown {
  if (!tc || typeof tc !== "object") return tc
  const t = tc as { type?: string; name?: string }
  if (t.type === "auto") return "auto"
  if (t.type === "none") return "none"
  if (t.type === "any") return "required"
  if (t.type === "tool" && t.name) {
    return { type: "function", function: { name: t.name } }
  }
  return tc
}

/** OpenAI chat.completion → Anthropic message object. */
export function openaiToAnthropicMessage(
  completion: Record<string, unknown>,
  model: string,
): Record<string, unknown> {
  const choice = (completion.choices as Array<Record<string, unknown>> | undefined)?.[0]
  const message = (choice?.message as Record<string, unknown> | undefined) ?? {}
  const content: Array<Record<string, unknown>> = []
  // The de-facto reasoning_content extension field (grok's include_reasoning
  // output, or codex's reasoning summary) becomes a leading, unsigned
  // thinking block — no `signature`, since this proxy has none to attach;
  // only a genuine Anthropic-origin thinking block carries one.
  const reasoning = message.reasoning_content
  if (typeof reasoning === "string" && reasoning) {
    content.push({ type: "thinking", thinking: reasoning })
  }
  const text = message.content
  if (typeof text === "string" && text) {
    content.push({ type: "text", text })
  }
  const toolCalls = message.tool_calls as Array<Record<string, unknown>> | undefined
  if (toolCalls) {
    for (const tc of toolCalls) {
      const fn = tc.function as { name?: string; arguments?: string } | undefined
      let input: unknown = {}
      try {
        input = JSON.parse(fn?.arguments || "{}")
      } catch {
        input = { raw: fn?.arguments }
      }
      content.push({
        type: "tool_use",
        id: tc.id,
        name: fn?.name,
        input,
      })
    }
  }
  const finish = choice?.finish_reason
  let stop_reason = "end_turn"
  if (finish === "tool_calls") stop_reason = "tool_use"
  else if (finish === "length") stop_reason = "max_tokens"

  const usage = completion.usage as
    | {
        prompt_tokens?: number
        completion_tokens?: number
        prompt_tokens_details?: { cached_tokens?: number; cache_write_tokens?: number }
        completion_tokens_details?: { reasoning_tokens?: number }
      }
    | undefined

  return {
    id: String(completion.id ?? `msg_${Date.now()}`),
    type: "message",
    role: "assistant",
    model,
    content: content.length ? content : [{ type: "text", text: "" }],
    stop_reason,
    stop_sequence: null,
    usage: usage ? anthropicUsageFromOpenAI(usage) : { input_tokens: 0, output_tokens: 0 },
  }
}

/**
 * OpenAI-shaped `usage` → Anthropic semantics: `prompt_tokens` is
 * cache-inclusive, Anthropic `input_tokens` is not — so when the upstream
 * reported `prompt_tokens_details.cached_tokens`, subtract it out and
 * report it as `cache_read_input_tokens` instead. Unchanged when no cache
 * details were reported (docs/api.md "Usage cache details on converted
 * responses"). `completion_tokens_details.reasoning_tokens`, when reported,
 * is added into `output_tokens` — Anthropic's own `output_tokens` already
 * includes thinking, so a converted response should match (docs/logging.md
 * "Token usage capture").
 */
function anthropicUsageFromOpenAI(usage: {
  prompt_tokens?: number
  completion_tokens?: number
  prompt_tokens_details?: { cached_tokens?: number; cache_write_tokens?: number }
  completion_tokens_details?: { reasoning_tokens?: number }
}): Record<string, unknown> {
  const prompt = usage.prompt_tokens ?? 0
  const reasoning = usage.completion_tokens_details?.reasoning_tokens
  const output = (usage.completion_tokens ?? 0) + (typeof reasoning === "number" ? reasoning : 0)
  const cached = usage.prompt_tokens_details?.cached_tokens
  const cacheWrite = usage.prompt_tokens_details?.cache_write_tokens
  const input = prompt - (typeof cached === "number" ? cached : 0) - (typeof cacheWrite === "number" ? cacheWrite : 0)
  return {
    input_tokens: input,
    output_tokens: output,
    ...(typeof cached === "number" ? { cache_read_input_tokens: cached } : {}),
    ...(typeof cacheWrite === "number" ? { cache_creation_input_tokens: cacheWrite } : {}),
  }
}

/**
 * OpenAI Chat Completions SSE → Anthropic Messages SSE.
 * Handles text deltas and streamed tool_calls (required for Claude Code on
 * /anthropic → grok|codex conversion).
 */
export function openaiSseToAnthropicStream(
  body: ReadableStream<Uint8Array>,
  model: string,
): ReadableStream<Uint8Array> {
  const decoder = new TextDecoder()
  const encoder = new TextEncoder()
  let buffer = ""
  const msgId = `msg_${crypto.randomUUID().replace(/-/g, "").slice(0, 24)}`
  let started = false
  let textBlockOpen = false
  let textBlockIndex = -1
  /**
   * Reasoning (grok's include_reasoning `reasoning_content`, or codex's
   * reasoning summary) normally arrives before anything else and streams
   * live into its own thinking block, closed as soon as real text/tool
   * content starts. Unsigned — this proxy has no signature to attach.
   */
  let thinkingBlockOpen = false
  let thinkingBlockIndex = -1
  /** Reasoning that arrives after a text/tool block is already open; flushed whole at finish(), mirrors pendingText. */
  let pendingThinking = ""
  let nextBlockIndex = 0
  let outputTokens = 0
  let stopped = false
  let sawToolCall = false
  /** finish_reason seen, mapped to an Anthropic stop_reason; null until then. */
  let stopReason: string | null = null
  /** Real upstream usage, when the provider reports it. */
  let promptTokens: number | null = null
  let completionTokens: number | null = null
  /** From `prompt_tokens_details.cached_tokens`, when the upstream reports it. */
  let cacheReadInputTokens: number | null = null
  /** From `prompt_tokens_details.cache_write_tokens`, when the upstream reports it. */
  let cacheCreationInputTokens: number | null = null
  /**
   * Anthropic content blocks are strictly sequential: a block must stop before
   * the next one starts, and a stopped block can never be reopened. An OpenAI
   * chunk stream has no such rule — it may interleave `content` text with a
   * tool call's `arguments`, and may alternate fragments between several
   * `tool_calls[].index` values. Nothing may therefore be closed early:
   *
   * - the first tool call streams live into an open block;
   * - text arriving while it is open is buffered (closing the block for text
   *   would split one call's JSON across two blocks sharing a tool id);
   * - every *other* tool call accumulates and is emitted complete at the end,
   *   which is what makes arbitrary fragment interleaving safe.
   *
   * Only the first of several parallel calls streams incrementally; providers
   * finish one call before starting the next, so in practice that costs
   * nothing, and a single tool call — the overwhelmingly common case — always
   * streams live.
   */
  let liveTool:
    | {
        openaiIndex: number
        blockIndex: number
        id: string
        idSynthesized: boolean
        name: string
      }
    | null = null
  /** Tool calls held back until finish, in arrival order. */
  const deferredTools: Array<{
    openaiIndex: number
    id: string | null
    name: string | null
    args: string
  }> = []
  /** Text that arrived while a tool block was open; emitted as its own block. */
  let pendingText = ""
  /** Tool names seen before their call could be opened (name-before-id). */
  const pendingToolNames = new Map<number, string>()
  /** Indices that already produced a block, so a re-sent name is not a new call. */
  const openedToolIndexes = new Set<number>()

  return new ReadableStream({
    async start(controller) {
      const reader = body.getReader()
      const emitEvent = (event: string, data: unknown) => {
        controller.enqueue(
          encoder.encode(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`),
        )
      }
      const ensureStart = () => {
        if (started) return
        started = true
        emitEvent("message_start", {
          type: "message_start",
          message: {
            id: msgId,
            type: "message",
            role: "assistant",
            model,
            content: [],
            stop_reason: null,
            stop_sequence: null,
            usage: { input_tokens: 0, output_tokens: 0 },
          },
        })
      }
      const closeTextBlock = () => {
        if (!textBlockOpen) return
        emitEvent("content_block_stop", {
          type: "content_block_stop",
          index: textBlockIndex,
        })
        textBlockOpen = false
      }
      const closeToolBlock = () => {
        if (!liveTool) return
        emitEvent("content_block_stop", {
          type: "content_block_stop",
          index: liveTool.blockIndex,
        })
        liveTool = null
      }
      const closeThinkingBlock = () => {
        if (!thinkingBlockOpen) return
        emitEvent("content_block_stop", {
          type: "content_block_stop",
          index: thinkingBlockIndex,
        })
        thinkingBlockOpen = false
      }
      /** Emit buffered reasoning as a complete thinking block of its own. */
      const flushPendingThinking = () => {
        if (!pendingThinking) return
        const thinking = pendingThinking
        pendingThinking = ""
        const index = nextBlockIndex++
        emitEvent("content_block_start", {
          type: "content_block_start",
          index,
          content_block: { type: "thinking", thinking: "" },
        })
        emitEvent("content_block_delta", {
          type: "content_block_delta",
          index,
          delta: { type: "thinking_delta", thinking },
        })
        emitEvent("content_block_stop", { type: "content_block_stop", index })
      }
      const appendThinking = (text: string) => {
        ensureStart()
        // Reasoning normally precedes everything else. If a text or tool
        // block is already open (a late/out-of-order fragment), do not
        // interleave — buffer it and flush as one complete block at the end,
        // same treatment as pendingText below.
        if (textBlockOpen || liveTool) {
          pendingThinking += text
          return
        }
        if (!thinkingBlockOpen) {
          thinkingBlockIndex = nextBlockIndex++
          thinkingBlockOpen = true
          emitEvent("content_block_start", {
            type: "content_block_start",
            index: thinkingBlockIndex,
            content_block: { type: "thinking", thinking: "" },
          })
        }
        emitEvent("content_block_delta", {
          type: "content_block_delta",
          index: thinkingBlockIndex,
          delta: { type: "thinking_delta", thinking: text },
        })
      }
      /** Emit buffered post-tool text as a complete block of its own. */
      const flushPendingText = () => {
        if (!pendingText) return
        const text = pendingText
        pendingText = ""
        const index = nextBlockIndex++
        emitEvent("content_block_start", {
          type: "content_block_start",
          index,
          content_block: { type: "text", text: "" },
        })
        emitEvent("content_block_delta", {
          type: "content_block_delta",
          index,
          delta: { type: "text_delta", text },
        })
        emitEvent("content_block_stop", { type: "content_block_stop", index })
      }
      const appendText = (text: string) => {
        ensureStart()
        // Reasoning (if any) always finishes before real content — close it
        // the moment text/tool content actually starts.
        closeThinkingBlock()
        // Never close an open tool block for text — that would split its
        // arguments JSON across two blocks sharing one tool_use id.
        if (liveTool) {
          pendingText += text
          return
        }
        if (!textBlockOpen) {
          textBlockIndex = nextBlockIndex++
          textBlockOpen = true
          emitEvent("content_block_start", {
            type: "content_block_start",
            index: textBlockIndex,
            content_block: { type: "text", text: "" },
          })
        }
        emitEvent("content_block_delta", {
          type: "content_block_delta",
          index: textBlockIndex,
          delta: { type: "text_delta", text },
        })
      }
      const emitArgs = (index: number, args: string) => {
        emitEvent("content_block_delta", {
          type: "content_block_delta",
          index,
          delta: { type: "input_json_delta", partial_json: args },
        })
      }
      /** A whole tool_use block at once, for calls that were held back. */
      const emitCompleteToolBlock = (
        openaiIndex: number,
        id: string | null,
        name: string | null,
        args: string,
      ) => {
        const index = nextBlockIndex++
        sawToolCall = true
        emitEvent("content_block_start", {
          type: "content_block_start",
          index,
          content_block: {
            type: "tool_use",
            id: id || `toolu_${openaiIndex}_${msgId.slice(4, 12)}`,
            name: name || "unknown",
            input: {},
          },
        })
        if (args) emitArgs(index, args)
        emitEvent("content_block_stop", { type: "content_block_stop", index })
      }
      const lastDeferred = (openaiIndex: number) => {
        for (let i = deferredTools.length - 1; i >= 0; i--) {
          const entry = deferredTools[i]!
          if (entry.openaiIndex === openaiIndex) return entry
        }
        return null
      }
      const routeToolFragment = (
        openaiIndex: number,
        id: string | undefined,
        name: string | undefined,
        args: string | undefined,
      ) => {
        ensureStart()
        const takeName = () => {
          const resolved = name || pendingToolNames.get(openaiIndex) || null
          pendingToolNames.delete(openaiIndex)
          return resolved
        }
        // Once an index is deferred its later fragments must follow it, or a
        // continuation would be appended to whichever call is live instead.
        const deferred = lastDeferred(openaiIndex)
        if (deferred) {
          // A new id at a deferred index is a distinct call, not a continuation.
          if (id && deferred.id && id !== deferred.id) {
            deferredTools.push({
              openaiIndex,
              id,
              name: takeName(),
              args: args ?? "",
            })
            openedToolIndexes.add(openaiIndex)
            return
          }
          if (id) deferred.id = id
          if (!deferred.name && name) deferred.name = name
          deferred.args += args ?? ""
          return
        }
        if (
          liveTool &&
          liveTool.openaiIndex === openaiIndex &&
          // A late id for a block opened on arguments alone belongs to that
          // call — its block_start has already gone out under the synthesized
          // id, so keep that and stop treating further ids as continuations.
          (!id || id === liveTool.id || liveTool.idSynthesized)
        ) {
          if (id) liveTool.idSynthesized = false
          if (args) emitArgs(liveTool.blockIndex, args)
          return
        }
        if (liveTool) {
          // Another call while one is streaming: hold it back so the live block
          // stays open and its arguments stay in one piece.
          deferredTools.push({
            openaiIndex,
            id: id ?? null,
            name: takeName(),
            args: args ?? "",
          })
          openedToolIndexes.add(openaiIndex)
          return
        }
        closeTextBlock()
        closeThinkingBlock()
        liveTool = {
          openaiIndex,
          blockIndex: nextBlockIndex++,
          id: id || `toolu_${openaiIndex}_${msgId.slice(4, 12)}`,
          idSynthesized: !id,
          name: takeName() || "unknown",
        }
        openedToolIndexes.add(openaiIndex)
        sawToolCall = true
        emitEvent("content_block_start", {
          type: "content_block_start",
          index: liveTool.blockIndex,
          content_block: {
            type: "tool_use",
            id: liveTool.id,
            name: liveTool.name,
            input: {},
          },
        })
        if (args) emitArgs(liveTool.blockIndex, args)
      }
      const finish = (stop_reason: string) => {
        if (stopped) return
        stopped = true
        ensureStart()
        closeTextBlock()
        closeToolBlock()
        closeThinkingBlock()
        flushPendingText()
        flushPendingThinking()
        for (const t of deferredTools) {
          emitCompleteToolBlock(t.openaiIndex, t.id, t.name, t.args)
        }
        deferredTools.length = 0
        // A call announced by name only, with no id and no arguments, still has
        // to reach the client — as a zero-argument call, which is what it is.
        // An index that already produced a block is a re-sent name, not a call.
        for (const [openaiIndex, name] of [...pendingToolNames.entries()].sort(
          (a, b) => a[0] - b[0],
        )) {
          if (openedToolIndexes.has(openaiIndex)) continue
          emitCompleteToolBlock(openaiIndex, null, name, "")
        }
        pendingToolNames.clear()
        if (nextBlockIndex === 0) {
          // Upstream sent no usable events. An SSE response carrying no content
          // block is a protocol error to Anthropic clients, which retry the
          // turn; emit a well-formed empty message instead.
          emitEvent("content_block_start", {
            type: "content_block_start",
            index: nextBlockIndex,
            content_block: { type: "text", text: "" },
          })
          emitEvent("content_block_stop", {
            type: "content_block_stop",
            index: nextBlockIndex++,
          })
        }
        // Prefer tool_use if we streamed any tools even when finish_reason was missing.
        const reason =
          stop_reason === "end_turn" && sawToolCall ? "tool_use" : stop_reason
        // Anthropic input_tokens excludes cached reads; OpenAI prompt_tokens
        // does not. Subtract out cached_tokens when the upstream reported it
        // (docs/api.md "Usage cache details on converted responses") —
        // unchanged otherwise.
        const inputTokens =
          promptTokens != null
            ? promptTokens - (cacheReadInputTokens ?? 0) - (cacheCreationInputTokens ?? 0)
            : promptTokens
        emitEvent("message_delta", {
          type: "message_delta",
          delta: { stop_reason: reason, stop_sequence: null },
          // Real upstream counts when the provider reports them; the character
          // estimate is only a floor for providers that report nothing.
          usage: {
            ...(inputTokens != null ? { input_tokens: inputTokens } : {}),
            output_tokens: completionTokens ?? outputTokens,
            ...(cacheReadInputTokens != null
              ? { cache_read_input_tokens: cacheReadInputTokens }
              : {}),
            ...(cacheCreationInputTokens != null
              ? { cache_creation_input_tokens: cacheCreationInputTokens }
              : {}),
          },
        })
        emitEvent("message_stop", { type: "message_stop" })
      }
      try {
        for (;;) {
          const { done, value } = await reader.read()
          if (done) break
          buffer += decoder.decode(value, { stream: true })
          const parts = buffer.split("\n")
          buffer = parts.pop() ?? ""
          for (const line of parts) {
            if (!line.startsWith("data:")) continue
            const data = line.slice(5).trim()
            if (!data) continue
            if (data === "[DONE]") {
              finish(stopReason ?? (sawToolCall ? "tool_use" : "end_turn"))
              continue
            }
            try {
              const json = JSON.parse(data) as Record<string, unknown>
              if (stopped) continue
              // A top-level `error` object with no `choices` (codex mid-turn
              // failure, relayed as an OpenAI-shaped error line) has no
              // Anthropic chunk equivalent — surface it as its own event and
              // end the message here; no message_delta/message_stop after it.
              const errorObj = json.error as { message?: string } | undefined
              if (errorObj && !("choices" in json)) {
                emitEvent("error", {
                  type: "error",
                  error: { type: "api_error", message: errorObj.message || "upstream error" },
                })
                stopped = true
                continue
              }
              // Usage rides on a final chunk that carries no choices, so read
              // it before the choice guard below discards that chunk.
              const usage = json.usage as
                | {
                    prompt_tokens?: number
                    completion_tokens?: number
                    prompt_tokens_details?: { cached_tokens?: number; cache_write_tokens?: number }
                    completion_tokens_details?: { reasoning_tokens?: number }
                  }
                | null
                | undefined
              if (usage) {
                if (typeof usage.prompt_tokens === "number") {
                  promptTokens = usage.prompt_tokens
                }
                if (typeof usage.completion_tokens === "number") {
                  // Anthropic's own output_tokens already includes thinking —
                  // fold reasoning_tokens in so a converted response matches
                  // (docs/logging.md "Token usage capture").
                  const reasoningTokens = usage.completion_tokens_details?.reasoning_tokens
                  completionTokens =
                    usage.completion_tokens +
                    (typeof reasoningTokens === "number" ? reasoningTokens : 0)
                }
                if (typeof usage.prompt_tokens_details?.cached_tokens === "number") {
                  cacheReadInputTokens = usage.prompt_tokens_details.cached_tokens
                }
                if (typeof usage.prompt_tokens_details?.cache_write_tokens === "number") {
                  cacheCreationInputTokens = usage.prompt_tokens_details.cache_write_tokens
                }
              }
              const choice = (json.choices as Array<Record<string, unknown>> | undefined)?.[0]
              if (!choice) continue
              // The turn is over once finish_reason lands; anything after it
              // would have to be emitted past message_stop.
              if (stopReason != null || stopped) continue
              const delta = choice.delta as Record<string, unknown> | undefined
              // Reasoning (grok include_reasoning / codex reasoning summary)
              // arrives before real content — see appendThinking above.
              const reasoningContent = delta?.reasoning_content
              if (typeof reasoningContent === "string" && reasoningContent) {
                outputTokens += Math.max(1, Math.ceil(reasoningContent.length / 4))
                appendThinking(reasoningContent)
              }
              const content = delta?.content
              if (typeof content === "string" && content) {
                outputTokens += Math.max(1, Math.ceil(content.length / 4))
                appendText(content)
              }

              const toolCalls = delta?.tool_calls as
                | Array<Record<string, unknown>>
                | undefined
              if (Array.isArray(toolCalls)) {
                for (const tc of toolCalls) {
                  const openaiIndex =
                    typeof tc.index === "number" ? tc.index : 0
                  const fn = tc.function as
                    | { name?: string; arguments?: string }
                    | undefined
                  const id = typeof tc.id === "string" ? tc.id : undefined
                  const name =
                    typeof fn?.name === "string" ? fn.name : undefined
                  const args =
                    typeof fn?.arguments === "string" ? fn.arguments : undefined
                  // A name with no id yet is not enough to open a block: doing
                  // so burns a fallback id that the real id would then have to
                  // supersede with a second, empty block.
                  if (id || args != null) {
                    if (args) {
                      outputTokens += Math.max(1, Math.ceil(args.length / 4))
                    }
                    routeToolFragment(openaiIndex, id, name, args)
                  } else if (name) {
                    // Name alone cannot open a block yet; remember it for the
                    // fragment that can. Routed fragments carry `name` directly.
                    pendingToolNames.set(openaiIndex, name)
                  }
                }
              }

              const fr = choice.finish_reason
              if (fr) {
                // Record it, but do not close the message yet: the usage chunk
                // rides after this one, before [DONE].
                if (fr === "tool_calls") stopReason = "tool_use"
                else if (fr === "length") stopReason = "max_tokens"
                else stopReason = "end_turn"
              }
            } catch {
              /* ignore parse */
            }
          }
        }
        if (!stopped) {
          finish(stopReason ?? (sawToolCall ? "tool_use" : "end_turn"))
        }
        controller.close()
      } catch (e) {
        controller.error(e)
      }
    },
  })
}


export function openaiToAnthropicMessages(input: {
  model: string
  messages: unknown[]
  max_tokens: number
  stream?: boolean
  tools?: unknown
  tool_choice?: unknown
  response_format?: unknown
  thinking?: { type: string }
  output_config?: { effort: string }
  stop?: string[]
  /** Clamped to Anthropic's [0, 1] — OpenAI's client-facing range is 0–2. */
  temperature?: number
  top_p?: number
}): Record<string, unknown> {
  const systemParts: Array<Record<string, unknown>> = []
  const messages: Array<Record<string, unknown>> = []

  for (const m of input.messages as Array<Record<string, unknown>>) {
    const role = String(m.role ?? "")
    if (role === "system") {
      systemParts.push(...contentToAnthropicBlocks(m.content))
      continue
    }
    if (role === "tool") {
      messages.push({
        role: "user",
        content: [
          {
            type: "tool_result",
            tool_use_id: m.tool_call_id,
            content: contentToText(m.content),
          },
        ],
      })
      continue
    }
    if (role === "assistant") {
      const blocks: Array<Record<string, unknown>> = []
      const text = contentToText(m.content)
      if (text) blocks.push({ type: "text", text })
      const toolCalls = m.tool_calls as Array<Record<string, unknown>> | undefined
      if (toolCalls) {
        for (const tc of toolCalls) {
          const fn = tc.function as { name?: string; arguments?: string } | undefined
          let args: unknown = {}
          try {
            args = JSON.parse(fn?.arguments || "{}")
          } catch {
            args = { raw: fn?.arguments }
          }
          blocks.push({
            type: "tool_use",
            id: tc.id,
            name: fn?.name,
            input: args,
          })
        }
      }
      messages.push({ role: "assistant", content: blocks.length ? blocks : text })
      continue
    }
    // user
    messages.push({
      role: "user",
      content: contentToAnthropicBlocks(m.content),
    })
  }

  const body: Record<string, unknown> = {
    model: input.model,
    max_tokens: input.max_tokens,
    messages,
    stream: !!input.stream,
  }
  // Sampling is constrained once thinking is on, and this surface always turns
  // a client effort into `output_config` (see `mapReasoning`) — so forwarding a
  // plain `temperature` + effort request would 400 on a combination the client
  // never asked for. Anthropic's rule: `temperature`/`top_k` are incompatible
  // with thinking, `top_p` only within [0.95, 1]. Thinking counts as on unless
  // explicitly disabled — an absent `thinking` alongside an `output_config`
  // effort still means thinking upstream. Dropped rather than clamped: quietly
  // retuning someone's sampling is worse than honoring the real constraint.
  const thinking =
    input.thinking?.type !== "disabled" && (!!input.thinking || !!input.output_config)
  // OpenAI's client-facing temperature range is 0–2; Anthropic caps at 1.
  if (typeof input.temperature === "number" && !thinking) {
    body.temperature = Math.max(0, Math.min(1, input.temperature))
  }
  if (typeof input.top_p === "number" && (!thinking || (input.top_p >= 0.95 && input.top_p <= 1))) {
    body.top_p = input.top_p
  }
  if (systemParts.length === 1 && systemParts[0]!.type === "text") {
    body.system = systemParts[0]!.text
  } else if (systemParts.length) {
    body.system = systemParts
  }
  if (input.tools && Array.isArray(input.tools)) {
    body.tools = (input.tools as Array<Record<string, unknown>>).map((t) => {
      const fn = t.function as { name?: string; description?: string; parameters?: unknown }
      if (fn) {
        return {
          name: fn.name,
          description: fn.description,
          input_schema: fn.parameters ?? { type: "object", properties: {} },
        }
      }
      return t
    })
  }
  if (input.tool_choice) {
    body.tool_choice = mapToolChoice(input.tool_choice)
  }
  if (input.thinking) body.thinking = input.thinking
  if (input.output_config) body.output_config = input.output_config
  if (input.stop?.length) body.stop_sequences = input.stop
  if (input.response_format) {
    const rf = input.response_format as { type?: string; json_schema?: { schema?: unknown } }
    if (rf.type === "json_schema" && rf.json_schema?.schema) {
      body.output_format = {
        type: "json_schema",
        schema: rf.json_schema.schema,
      }
    } else if (rf.type === "json_object") {
      // best-effort: not always supported; leave as instruction-free skip
    }
  }
  // Explicitly no cache_control
  return body
}

function contentToText(content: unknown): string {
  if (typeof content === "string") return content
  if (Array.isArray(content)) {
    return content
      .map((p) => {
        if (typeof p === "string") return p
        if (p && typeof p === "object" && (p as { type?: string }).type === "text") {
          return String((p as { text?: string }).text ?? "")
        }
        return ""
      })
      .join("")
  }
  return content == null ? "" : String(content)
}

function contentToAnthropicBlocks(content: unknown): Array<Record<string, unknown>> {
  if (typeof content === "string") return [{ type: "text", text: content }]
  if (!Array.isArray(content)) return [{ type: "text", text: String(content ?? "") }]
  const blocks: Array<Record<string, unknown>> = []
  for (const part of content as Array<Record<string, unknown>>) {
    if (part.type === "text") {
      blocks.push({ type: "text", text: part.text })
    } else if (part.type === "image_url") {
      const url = (part.image_url as { url?: string })?.url ?? ""
      const m = url.match(/^data:([^;]+);base64,(.+)$/)
      if (m) {
        blocks.push({
          type: "image",
          source: { type: "base64", media_type: m[1], data: m[2] },
        })
      } else {
        blocks.push({
          type: "image",
          source: { type: "url", url },
        })
      }
    } else {
      blocks.push(part)
    }
  }
  return blocks.length ? blocks : [{ type: "text", text: "" }]
}

function mapToolChoice(tc: unknown): unknown {
  if (tc === "auto" || tc === "none" || tc === "required") {
    if (tc === "required") return { type: "any" }
    if (tc === "none") return { type: "none" }
    return { type: "auto" }
  }
  if (tc && typeof tc === "object" && (tc as { type?: string }).type === "function") {
    const name = (tc as { function?: { name?: string } }).function?.name
    return { type: "tool", name }
  }
  return tc
}

export function anthropicToOpenAIResponse(msg: Record<string, unknown>, model: string): Record<string, unknown> {
  const content = msg.content as Array<Record<string, unknown>> | undefined
  let text = ""
  const tool_calls: Array<Record<string, unknown>> = []
  if (Array.isArray(content)) {
    for (const b of content) {
      if (b.type === "text") text += String(b.text ?? "")
      if (b.type === "tool_use") {
        tool_calls.push({
          id: b.id,
          type: "function",
          function: {
            name: b.name,
            arguments: JSON.stringify(b.input ?? {}),
          },
        })
      }
    }
  }
  const usage = msg.usage as Record<string, number> | undefined
  const stop = msg.stop_reason
  let finish: string | null = "stop"
  if (stop === "tool_use") finish = "tool_calls"
  else if (stop === "max_tokens") finish = "length"

  return {
    id: msg.id ?? `chatcmpl_${Date.now()}`,
    object: "chat.completion",
    created: Math.floor(Date.now() / 1000),
    model,
    choices: [
      {
        index: 0,
        message: {
          role: "assistant",
          content: text || null,
          tool_calls: tool_calls.length ? tool_calls : undefined,
        },
        finish_reason: finish,
      },
    ],
    usage: usage
      ? {
          prompt_tokens:
            (usage.input_tokens ?? 0) +
            (usage.cache_read_input_tokens ?? 0) +
            (usage.cache_creation_input_tokens ?? 0),
          completion_tokens: usage.output_tokens ?? 0,
          total_tokens:
            (usage.input_tokens ?? 0) +
            (usage.cache_read_input_tokens ?? 0) +
            (usage.cache_creation_input_tokens ?? 0) +
            (usage.output_tokens ?? 0),
          // Attach the upstream cache numbers instead of discarding them
          // (docs/api.md "Usage cache details on converted responses").
          // 0 is a valid, meaningful value here — Anthropic usage always
          // defines these fields, so they are attached whenever `usage`
          // itself was present, not only when they are non-zero.
          prompt_tokens_details: { cached_tokens: usage.cache_read_input_tokens ?? 0 },
          cache_creation_input_tokens: usage.cache_creation_input_tokens ?? 0,
        }
      : undefined,
  }
}

/**
 * Anthropic Messages SSE → OpenAI Chat Completions SSE.
 * Handles text_delta and tool_use (input_json_delta) streaming.
 */
export function anthropicSseToOpenAIStream(
  body: ReadableStream<Uint8Array>,
  model: string,
): ReadableStream<Uint8Array> {
  const decoder = new TextDecoder()
  const encoder = new TextEncoder()
  let buffer = ""
  const id = `chatcmpl_${crypto.randomUUID().replace(/-/g, "").slice(0, 24)}`
  let sentRole = false
  let finishReason: string | null = null
  // Anthropic content block index → OpenAI tool_calls index (only tool_use blocks)
  const toolIndexByBlock = new Map<number, number>()
  let nextToolIndex = 0
  // SSE event name must survive chunk boundaries: the `event:` line and its
  // `data:` line routinely arrive in different reads.
  let event = ""
  // Usage rides on message_start (input side) and message_delta (output
  // side), the same fields anthropicToOpenAIResponse sums for the non-stream
  // response. null until an actual usage object is seen.
  let promptTokens: number | null = null
  let completionTokens: number | null = null
  let sawUsage = false
  // Cache numbers ride only on message_start's usage (the input side);
  // null until that event is seen, then always a number (Anthropic usage
  // always defines these fields — see fromAnthropicUsage).
  let cacheReadInputTokens: number | null = null
  let cacheCreationInputTokens: number | null = null
  // Set once the message has concluded — normally or via a mid-stream
  // `event: error` — so nothing more is emitted after.
  let stopped = false

  return new ReadableStream({
    async start(controller) {
      const reader = body.getReader()
      const emit = (obj: unknown) => {
        controller.enqueue(encoder.encode(`data: ${JSON.stringify(obj)}\n\n`))
      }
      const ensureRole = () => {
        if (sentRole) return
        emit({
          id,
          object: "chat.completion.chunk",
          created: Math.floor(Date.now() / 1000),
          model,
          choices: [
            {
              index: 0,
              delta: { role: "assistant", content: "" },
              finish_reason: null,
            },
          ],
        })
        sentRole = true
      }
      try {
        for (;;) {
          const { done, value } = await reader.read()
          if (done) break
          buffer += decoder.decode(value, { stream: true })
          const parts = buffer.split("\n")
          buffer = parts.pop() ?? ""
          for (const line of parts) {
            if (line.startsWith("event:")) event = line.slice(6).trim()
            else if (line.startsWith("data:")) {
              const data = line.slice(5).trim()
              if (!data) continue
              try {
                const json = JSON.parse(data) as Record<string, unknown>
                if (stopped) {
                  event = ""
                  continue
                }
                if (event === "message_start") {
                  const msg = json.message as
                    | { usage?: Record<string, number> }
                    | undefined
                  const usage = msg?.usage
                  if (usage) {
                    // Same summation anthropicToOpenAIResponse uses for the
                    // non-stream response: cache tokens count as prompt tokens.
                    promptTokens =
                      (usage.input_tokens ?? 0) +
                      (usage.cache_read_input_tokens ?? 0) +
                      (usage.cache_creation_input_tokens ?? 0)
                    cacheReadInputTokens = usage.cache_read_input_tokens ?? 0
                    cacheCreationInputTokens = usage.cache_creation_input_tokens ?? 0
                    sawUsage = true
                  }
                } else if (event === "content_block_start") {
                  const block = json.content_block as
                    | { type?: string; id?: string; name?: string }
                    | undefined
                  const blockIndex =
                    typeof json.index === "number" ? json.index : 0
                  if (block?.type === "tool_use") {
                    ensureRole()
                    const toolIndex = nextToolIndex++
                    toolIndexByBlock.set(blockIndex, toolIndex)
                    emit({
                      id,
                      object: "chat.completion.chunk",
                      created: Math.floor(Date.now() / 1000),
                      model,
                      choices: [
                        {
                          index: 0,
                          delta: {
                            tool_calls: [
                              {
                                index: toolIndex,
                                id: block.id,
                                type: "function",
                                function: {
                                  name: block.name,
                                  arguments: "",
                                },
                              },
                            ],
                          },
                          finish_reason: null,
                        },
                      ],
                    })
                  }
                } else if (event === "content_block_delta") {
                  const delta = json.delta as
                    | { type?: string; text?: string; partial_json?: string }
                    | undefined
                  const blockIndex =
                    typeof json.index === "number" ? json.index : 0
                  if (delta?.type === "text_delta" && delta.text) {
                    ensureRole()
                    emit({
                      id,
                      object: "chat.completion.chunk",
                      created: Math.floor(Date.now() / 1000),
                      model,
                      choices: [
                        {
                          index: 0,
                          delta: { content: delta.text },
                          finish_reason: null,
                        },
                      ],
                    })
                  } else if (
                    delta?.type === "input_json_delta" &&
                    typeof delta.partial_json === "string"
                  ) {
                    const toolIndex = toolIndexByBlock.get(blockIndex) ?? 0
                    ensureRole()
                    emit({
                      id,
                      object: "chat.completion.chunk",
                      created: Math.floor(Date.now() / 1000),
                      model,
                      choices: [
                        {
                          index: 0,
                          delta: {
                            tool_calls: [
                              {
                                index: toolIndex,
                                function: {
                                  arguments: delta.partial_json,
                                },
                              },
                            ],
                          },
                          finish_reason: null,
                        },
                      ],
                    })
                  }
                } else if (event === "message_delta") {
                  const d = json.delta as { stop_reason?: string } | undefined
                  if (d?.stop_reason === "tool_use") finishReason = "tool_calls"
                  else if (d?.stop_reason === "max_tokens") finishReason = "length"
                  else if (d?.stop_reason) finishReason = "stop"
                  const u = json.usage as
                    | { input_tokens?: number; output_tokens?: number }
                    | undefined
                  if (u) {
                    if (typeof u.output_tokens === "number") {
                      completionTokens = u.output_tokens
                      sawUsage = true
                    }
                    if (typeof u.input_tokens === "number") {
                      promptTokens = u.input_tokens
                      sawUsage = true
                    }
                  }
                } else if (event === "message_stop") {
                  const payload: Record<string, unknown> = {
                    id,
                    object: "chat.completion.chunk",
                    created: Math.floor(Date.now() / 1000),
                    model,
                    choices: [
                      {
                        index: 0,
                        delta: {},
                        finish_reason: finishReason ?? "stop",
                      },
                    ],
                  }
                  // Same pattern codexSseToOpenAIStream uses: usage rides on
                  // the finish chunk, only when some count was actually seen.
                  if (sawUsage) {
                    const prompt = promptTokens ?? 0
                    const completion = completionTokens ?? 0
                    const usagePayload: Record<string, unknown> = {
                      prompt_tokens: prompt,
                      completion_tokens: completion,
                      total_tokens: prompt + completion,
                    }
                    // Only known once message_start's usage was actually
                    // seen — mirrors anthropicToOpenAIResponse's non-stream
                    // enrichment (docs/api.md cache details section).
                    if (cacheReadInputTokens != null) {
                      usagePayload.prompt_tokens_details = { cached_tokens: cacheReadInputTokens }
                      usagePayload.cache_creation_input_tokens = cacheCreationInputTokens ?? 0
                    }
                    payload.usage = usagePayload
                  }
                  emit(payload)
                  controller.enqueue(encoder.encode("data: [DONE]\n\n"))
                  stopped = true
                } else if (event === "error") {
                  const errObj = json.error as
                    | { type?: string; message?: string }
                    | undefined
                  emit({
                    error: {
                      message: errObj?.message || "upstream error",
                      type: errObj?.type || "api_error",
                    },
                  })
                  stopped = true
                }
              } catch {
                /* ignore parse */
              }
              event = ""
            }
          }
        }
        controller.close()
      } catch (e) {
        controller.error(e)
      }
    },
  })
}
