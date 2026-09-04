/**
 * Anthropic Messages ↔ xAI Responses for `/anthropic` → grok.
 *
 * Maps reasoning.encrypted_content ↔ thinking.signature; never invents
 * Claude-native signatures. Thinking/effort rules: docs/api.md "Grok reasoning".
 */

import { isValidGrokEncryptedContent } from "../providers/grok_encrypted_content"
import { mapReasoning, parseReasoningEffort, type ReasoningEffort } from "../utils/reasoning"
import { anthropicOutputFormat, stripCacheControl } from "./openai_anthropic"

export type GrokThinkingMode = "disabled" | "enabled" | "default"

export type GrokAnthropicConvertResult = {
  body: Record<string, unknown>
  thinkingMode: GrokThinkingMode
  /** Last assistant plaintext in the converted input (for replay-cache match). */
  lastAssistantText: string
}

/**
 * Resolve whether to expose encrypted reasoning and which effort to send.
 * `thinking.type=disabled` is authoritative — effort fields are ignored.
 * `budget_tokens` is intentionally ignored (effort-only).
 */
export function resolveGrokThinkingEffort(body: Record<string, unknown>): {
  thinkingMode: GrokThinkingMode
  effort: ReasoningEffort | undefined
} {
  const thinking = body.thinking as { type?: string } | undefined
  const thinkingType =
    thinking && typeof thinking === "object" && typeof thinking.type === "string"
      ? thinking.type.toLowerCase()
      : ""

  // Disabled wins over any effort field — do not map effort when off.
  if (thinkingType === "disabled") {
    return { thinkingMode: "disabled", effort: undefined }
  }

  const outputConfig = body.output_config as { effort?: unknown } | undefined
  const rawEffort =
    body.reasoning_effort != null
      ? body.reasoning_effort
      : typeof outputConfig?.effort === "string"
        ? outputConfig.effort
        : undefined
  const parsed = parseReasoningEffort(rawEffort)
  const effort = parsed === "invalid" || parsed === undefined ? undefined : parsed

  if (
    thinkingType === "enabled" ||
    thinkingType === "adaptive" ||
    thinkingType === "auto"
  ) {
    return { thinkingMode: "enabled", effort: effort ?? "medium" }
  }
  if (effort !== undefined) {
    return { thinkingMode: "enabled", effort }
  }
  // No thinking object and no effort: still ask for encrypted reasoning so
  // Claude Code multi-turn can continue when the client omits thinking config.
  return { thinkingMode: "default", effort: undefined }
}

/** Pure builder — unit-testable without network. */
export function anthropicToGrokResponses(
  body: Record<string, unknown>,
  opts: {
    upstreamModel: string
    /** Injected from KV when the client omitted signature but session continues. */
    replayEncryptedContent?: string | null
  },
): GrokAnthropicConvertResult {
  const cleaned = stripCacheControl(body) as Record<string, unknown>
  const { thinkingMode, effort } = resolveGrokThinkingEffort(cleaned)
  if (parseReasoningEffort(cleaned.reasoning_effort) === "invalid") {
    // Surface via a sentinel the adapter turns into 400 — keep convert pure.
    throw new InvalidGrokReasoningEffortError()
  }
  if (
    cleaned.output_config &&
    typeof (cleaned.output_config as { effort?: unknown }).effort === "string" &&
    cleaned.reasoning_effort == null &&
    parseReasoningEffort((cleaned.output_config as { effort: string }).effort) === "invalid"
  ) {
    throw new InvalidGrokReasoningEffortError()
  }

  const mapped = mapReasoning("grok", effort)
  const { input, instructions, lastAssistantText } = anthropicMessagesToGrokInput(
    cleaned,
    thinkingMode !== "disabled" ? opts.replayEncryptedContent : null,
  )

  const out: Record<string, unknown> = {
    model: opts.upstreamModel,
    input,
    stream: true,
    store: false,
  }
  if (instructions) out.instructions = instructions

  // Disabled: omit include and reasoning entirely — do not send effort "none"
  // (unverified on xAI) and do not honor a concurrent output_config.effort.
  if (thinkingMode !== "disabled") {
    out.include = ["reasoning.encrypted_content"]
    if (mapped.reasoning_effort && mapped.reasoning_effort !== "none") {
      out.reasoning = { effort: mapped.reasoning_effort }
    }
  }

  if (typeof cleaned.max_tokens === "number") {
    out.max_output_tokens = cleaned.max_tokens
  }
  // Pin the shared surface default (docs/providers.md).
  out.temperature = typeof cleaned.temperature === "number" ? cleaned.temperature : 1
  if (typeof cleaned.top_p === "number") out.top_p = cleaned.top_p

  // stop_sequences: Responses has no Chat Completions `stop` equivalent —
  // dropped (same as codex). See docs/api.md grok Anthropic row.
  const outputFormat = anthropicOutputFormat(cleaned)
  if (outputFormat) {
    const of = outputFormat
    if (of.type === "json_schema" && of.schema) {
      out.text = {
        format: {
          type: "json_schema",
          name: "response",
          schema: of.schema,
          strict: false,
        },
      }
    }
  }

  if (Array.isArray(cleaned.tools)) {
    const tools = mapAnthropicToolsToResponses(cleaned.tools as Array<Record<string, unknown>>)
    if (tools.length) {
      out.tools = tools
      out.tool_choice = mapAnthropicToolChoiceToResponses(cleaned.tool_choice, true)
    }
  }

  return { body: out, thinkingMode, lastAssistantText }
}

export class InvalidGrokReasoningEffortError extends Error {
  constructor() {
    super("invalid reasoning_effort")
    this.name = "InvalidGrokReasoningEffortError"
  }
}

function anthropicMessagesToGrokInput(
  body: Record<string, unknown>,
  replayEncryptedContent: string | null | undefined,
): {
  input: unknown[]
  instructions: string
  lastAssistantText: string
} {
  const instructions = systemToInstructions(body.system)
  const input: unknown[] = []
  const lastAssistantText = lastAssistantTextFromAnthropicMessages(body)
  let injectedReplay = false
  const replay =
    typeof replayEncryptedContent === "string" &&
    isValidGrokEncryptedContent(replayEncryptedContent)
      ? replayEncryptedContent
      : null

  const messages = (body.messages as Array<Record<string, unknown>>) ?? []
  for (let mi = 0; mi < messages.length; mi++) {
    const m = messages[mi]!
    const role = String(m.role ?? "")
    const isLastAssistant =
      role === "assistant" &&
      !messages.slice(mi + 1).some((x) => String(x.role ?? "") === "assistant")

    if (role === "user") {
      const blocks = Array.isArray(m.content)
        ? (m.content as Array<Record<string, unknown>>)
        : null
      if (blocks && blocks.some((b) => b.type === "tool_result")) {
        for (const b of blocks) {
          if (b.type === "tool_result") {
            input.push(...toolResultToResponses(b))
          }
        }
        const nonTool = blocks.filter((b) => b.type !== "tool_result")
        if (nonTool.length) {
          input.push({
            type: "message",
            role: "user",
            content: anthropicBlocksToInputContent(nonTool),
          })
        }
        continue
      }
      input.push({
        type: "message",
        role: "user",
        content: Array.isArray(m.content)
          ? anthropicBlocksToInputContent(m.content as Array<Record<string, unknown>>)
          : [{ type: "input_text", text: contentToText(m.content) }],
      })
      continue
    }

    if (role === "assistant") {
      const blocks = Array.isArray(m.content)
        ? (m.content as Array<Record<string, unknown>>)
        : null
      let text = ""
      const toolUses: Array<Record<string, unknown>> = []
      let signature: string | null = null

      if (blocks) {
        for (const b of blocks) {
          if (b.type === "text") text += String(b.text ?? "")
          if (b.type === "thinking") {
            // Only forward signatures that pass the Grok transport check —
            // Claude-native / GPT / Gemini envelopes are dropped, never
            // replayed as encrypted_content.
            const sig = b.signature
            if (typeof sig === "string" && isValidGrokEncryptedContent(sig)) {
              signature = sig
            }
          }
          if (b.type === "tool_use") toolUses.push(b)
        }
      } else {
        text = contentToText(m.content)
      }

      // Prefer a validated client signature; else inject session replay once
      // for the trailing assistant turn when the client stripped signatures.
      let encrypted: string | null = signature
      if (!encrypted && isLastAssistant && !injectedReplay && replay) {
        encrypted = replay
        injectedReplay = true
      }
      if (encrypted) {
        input.push({
          type: "reasoning",
          summary: [],
          content: null,
          encrypted_content: encrypted,
        })
      }

      if (text) {
        input.push({
          type: "message",
          role: "assistant",
          content: [{ type: "output_text", text }],
        })
      }
      for (const tu of toolUses) {
        input.push({
          type: "function_call",
          call_id: tu.id,
          name: tu.name,
          arguments: JSON.stringify(tu.input ?? {}),
        })
      }
      continue
    }

    // Unknown roles: best-effort user text.
    input.push({
      type: "message",
      role: "user",
      content: [{ type: "input_text", text: contentToText(m.content) }],
    })
  }

  return { input, instructions, lastAssistantText }
}

function systemToInstructions(system: unknown): string {
  if (typeof system === "string") return system
  if (!Array.isArray(system)) return ""
  return system
    .map((b) => {
      if (b && typeof b === "object" && (b as { type?: string }).type === "text") {
        return String((b as { text?: string }).text ?? "")
      }
      return ""
    })
    .filter((t) => t)
    .join("\n\n")
}

function anthropicBlocksToInputContent(
  blocks: Array<Record<string, unknown>>,
): Array<Record<string, unknown>> {
  const parts: Array<Record<string, unknown>> = []
  for (const b of blocks) {
    if (b.type === "text") {
      parts.push({ type: "input_text", text: String(b.text ?? "") })
    } else if (b.type === "image") {
      const src = b.source as
        | { type?: string; media_type?: string; data?: string; url?: string }
        | undefined
      if (src?.type === "base64" && src.data) {
        parts.push({
          type: "input_image",
          image_url: `data:${src.media_type || "image/png"};base64,${src.data}`,
        })
      } else if (src?.type === "url" && src.url) {
        parts.push({ type: "input_image", image_url: src.url })
      }
    }
  }
  return parts.length ? parts : [{ type: "input_text", text: "" }]
}

function toolResultToResponses(b: Record<string, unknown>): unknown[] {
  const callId = b.tool_use_id
  const content = b.content
  const blocks = Array.isArray(content) ? (content as Array<Record<string, unknown>>) : null
  const images = blocks ? blocks.filter((x) => x.type === "image") : []
  const out: unknown[] = []

  if (!blocks || images.length === 0) {
    out.push({
      type: "function_call_output",
      call_id: callId,
      output: contentToText(content),
    })
    return out
  }

  const text = blocks
    .filter((x) => x.type === "text")
    .map((x) => String(x.text ?? ""))
    .join("")
  const placeholder =
    images.length === 1 ? "[image attached below]" : `[${images.length} images attached below]`
  out.push({
    type: "function_call_output",
    call_id: callId,
    output: text ? `${text}\n${placeholder}` : placeholder,
  })
  out.push({
    type: "message",
    role: "user",
    content: [
      { type: "input_text", text: `[Image(s) from tool result ${callId}]` },
      ...anthropicBlocksToInputContent(images),
    ],
  })
  return out
}

function mapAnthropicToolsToResponses(
  tools: Array<Record<string, unknown>>,
): unknown[] {
  const out: unknown[] = []
  for (const t of tools) {
    if (!t || typeof t !== "object") continue
    if (t.function) {
      const fn = t.function as {
        name?: string
        description?: string
        parameters?: unknown
      }
      out.push({
        type: "function",
        name: fn.name,
        description: fn.description,
        parameters: fn.parameters,
      })
      continue
    }
    const hasInputSchema = "input_schema" in t
    const type = typeof t.type === "string" ? t.type : undefined
    if (type && type !== "custom" && !hasInputSchema) {
      // Server-side Anthropic tools — drop (same as Chat Completions convert).
      continue
    }
    if (t.name) {
      out.push({
        type: "function",
        name: t.name,
        description: t.description,
        parameters: t.input_schema ?? { type: "object", properties: {} },
      })
    }
  }
  return out
}

function mapAnthropicToolChoiceToResponses(
  tc: unknown,
  hasTools: boolean,
): unknown {
  if (!hasTools) return undefined
  if (tc == null) return "auto"
  if (!tc || typeof tc !== "object") return "auto"
  const t = tc as { type?: string; name?: string }
  if (t.type === "auto") return "auto"
  if (t.type === "none") return "none"
  if (t.type === "any") return "required"
  if (t.type === "tool" && t.name) return { type: "function", name: t.name }
  return "auto"
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

/** Trailing assistant plaintext — shared by convert + replay-cache match. */
export function lastAssistantTextFromAnthropicMessages(
  body: Record<string, unknown>,
): string {
  const messages = (body.messages as Array<Record<string, unknown>>) ?? []
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i]!
    if (String(m.role ?? "") !== "assistant") continue
    if (Array.isArray(m.content)) {
      let text = ""
      for (const b of m.content as Array<Record<string, unknown>>) {
        if (b.type === "text") text += String(b.text ?? "")
      }
      return text
    }
    return typeof m.content === "string" ? m.content : ""
  }
  return ""
}

// ── Response / stream conversion ───────────────────────────────────────────

type GrokResponsesEvent = {
  type?: string
  delta?: string
  item_id?: string
  item?: {
    type?: string
    id?: string
    call_id?: string
    name?: string
    arguments?: string
    encrypted_content?: string
    summary?: Array<{ type?: string; text?: string } | string>
    content?: unknown
  }
  response?: {
    id?: string
    output?: unknown[]
    usage?: {
      input_tokens?: number
      output_tokens?: number
      input_tokens_details?: { cached_tokens?: number }
      output_tokens_details?: { reasoning_tokens?: number }
    }
    error?: { message?: string }
  }
  error?: { message?: string }
  message?: string
}

export type GrokTurnOutcome =
  | { kind: "replayable"; encrypted_content: string; assistant_text: string }
  | { kind: "clear" }

/**
 * Responses SSE → Anthropic Messages SSE.
 * Emits signature_delta from reasoning.encrypted_content when present.
 * Upstream EOF without `response.completed` is an Anthropic `event: error`,
 * not a fabricated successful message_stop (mirrors codex mid-turn failure).
 */
export function grokResponsesSseToAnthropicStream(
  body: ReadableStream<Uint8Array>,
  model: string,
  opts?: {
    thinkingMode?: GrokThinkingMode
    onTurnOutcome?: (outcome: GrokTurnOutcome) => void
  },
): ReadableStream<Uint8Array> {
  const thinkingMode = opts?.thinkingMode ?? "default"
  const emitThinking = thinkingMode !== "disabled"
  const decoder = new TextDecoder()
  const encoder = new TextEncoder()
  let buffer = ""
  const msgId = `msg_${crypto.randomUUID().replace(/-/g, "").slice(0, 24)}`
  let started = false
  let stopped = false
  let sawCompleted = false
  let nextBlockIndex = 0
  let textBlockOpen = false
  let textBlockIndex = -1
  let thinkingBlockOpen = false
  let thinkingBlockIndex = -1
  let pendingThinking = ""
  let sawToolCall = false
  let stopReason: string | null = null
  let promptTokens: number | null = null
  let completionTokens: number | null = null
  let cacheReadInputTokens: number | null = null
  let assistantText = ""
  let encryptedContent = ""
  let thinkingSignaturePending = ""
  let upstreamReader: ReadableStreamDefaultReader<Uint8Array> | null = null
  // cli-chat-proxy often emits function_call / output_text before
  // reasoning.output_item.done. Closing the thinking block early would either
  // drop the final signature or emit a preliminary blob — both break the next
  // turn (and Claude Code fork/subagent inherits that assistant message).
  let reasoningOpen = false
  const deferredEvents: Array<() => void> = []

  let liveTool: {
    itemId: string
    blockIndex: number
    id: string
    sawArgs: boolean
  } | null = null
  const pendingArgs = new Map<string, string>()

  return new ReadableStream({
    async start(controller) {
      const reader = body.getReader()
      upstreamReader = reader
      const emitEvent = (event: string, data: unknown) => {
        controller.enqueue(
          encoder.encode(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`),
        )
      }
      const flushDeferred = () => {
        const queued = deferredEvents.splice(0)
        for (const run of queued) run()
      }
      /** Run now, or after the in-flight reasoning item finishes. */
      const afterReasoning = (run: () => void) => {
        if (reasoningOpen) deferredEvents.push(run)
        else run()
      }
      /**
       * Responses usage, from whichever event carries it. Called for *every*
       * event, not just the terminal one: `message_start.usage.input_tokens`
       * is the client's context indicator (docs/api.md), so an upstream that
       * reports the input side early is worth catching before the first
       * content block opens.
       */
      const harvestUsage = (u: NonNullable<GrokResponsesEvent["response"]>["usage"]) => {
        if (!u) return
        if (typeof u.input_tokens === "number") promptTokens = u.input_tokens
        // Responses output_tokens already includes reasoning for xAI.
        if (typeof u.output_tokens === "number") completionTokens = u.output_tokens
        if (typeof u.input_tokens_details?.cached_tokens === "number") {
          cacheReadInputTokens = u.input_tokens_details.cached_tokens
        }
      }
      const ensureStart = () => {
        if (started) return
        started = true
        const input =
          promptTokens != null ? promptTokens - (cacheReadInputTokens ?? 0) : 0
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
            usage: {
              input_tokens: Math.max(0, input),
              output_tokens: 0,
              ...(cacheReadInputTokens != null
                ? { cache_read_input_tokens: cacheReadInputTokens }
                : {}),
            },
          },
        })
      }
      const closeText = () => {
        if (!textBlockOpen) return
        emitEvent("content_block_stop", {
          type: "content_block_stop",
          index: textBlockIndex,
        })
        textBlockOpen = false
      }
      const closeTool = () => {
        if (!liveTool) return
        emitEvent("content_block_stop", {
          type: "content_block_stop",
          index: liveTool.blockIndex,
        })
        liveTool = null
      }
      const closeThinking = () => {
        if (!thinkingBlockOpen) return
        if (thinkingSignaturePending) {
          emitEvent("content_block_delta", {
            type: "content_block_delta",
            index: thinkingBlockIndex,
            delta: {
              type: "signature_delta",
              signature: thinkingSignaturePending,
            },
          })
          thinkingSignaturePending = ""
        }
        emitEvent("content_block_stop", {
          type: "content_block_stop",
          index: thinkingBlockIndex,
        })
        thinkingBlockOpen = false
      }
      const openThinking = () => {
        if (!emitThinking || thinkingBlockOpen) return
        if (textBlockOpen || liveTool) return
        ensureStart()
        thinkingBlockIndex = nextBlockIndex++
        thinkingBlockOpen = true
        emitEvent("content_block_start", {
          type: "content_block_start",
          index: thinkingBlockIndex,
          content_block: { type: "thinking", thinking: "" },
        })
      }
      const appendThinking = (text: string) => {
        if (!emitThinking || !text) return
        if (textBlockOpen || liveTool) {
          pendingThinking += text
          return
        }
        openThinking()
        if (!thinkingBlockOpen) return
        emitEvent("content_block_delta", {
          type: "content_block_delta",
          index: thinkingBlockIndex,
          delta: { type: "thinking_delta", thinking: text },
        })
      }
      const flushPendingThinking = () => {
        if (!pendingThinking || !emitThinking) {
          pendingThinking = ""
          return
        }
        const thinking = pendingThinking
        pendingThinking = ""
        const index = nextBlockIndex++
        emitEvent("content_block_start", {
          type: "content_block_start",
          index,
          content_block: {
            type: "thinking",
            thinking: "",
            ...(encryptedContent ? { signature: encryptedContent } : {}),
          },
        })
        emitEvent("content_block_delta", {
          type: "content_block_delta",
          index,
          delta: { type: "thinking_delta", thinking },
        })
        emitEvent("content_block_stop", { type: "content_block_stop", index })
      }
      const appendText = (text: string) => {
        if (!text) return
        ensureStart()
        closeThinking()
        if (liveTool) return
        assistantText += text
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
      const openTool = (itemId: string, callId: string, name: string) => {
        ensureStart()
        closeText()
        closeThinking()
        liveTool = {
          itemId,
          blockIndex: nextBlockIndex++,
          id: callId,
          sawArgs: false,
        }
        sawToolCall = true
        emitEvent("content_block_start", {
          type: "content_block_start",
          index: liveTool.blockIndex,
          content_block: {
            type: "tool_use",
            id: callId,
            name: name || "unknown",
            input: {},
          },
        })
        const stashed = pendingArgs.get(itemId)
        if (stashed) {
          pendingArgs.delete(itemId)
          liveTool.sawArgs = true
          emitEvent("content_block_delta", {
            type: "content_block_delta",
            index: liveTool.blockIndex,
            delta: { type: "input_json_delta", partial_json: stashed },
          })
        }
      }
      const emitUpstreamError = (message: string) => {
        if (stopped) return
        stopped = true
        emitEvent("error", {
          type: "error",
          error: { type: "api_error", message },
        })
      }
      const finish = (reason: string) => {
        if (stopped) return
        stopped = true
        ensureStart()
        // Settle any in-flight reasoning first so deferred tool/text blocks
        // still follow a signature_delta when upstream omitted .done.
        closeThinking()
        reasoningOpen = false
        flushDeferred()
        closeText()
        closeTool()
        flushPendingThinking()
        if (nextBlockIndex === 0) {
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
        const finalReason =
          reason === "end_turn" && sawToolCall ? "tool_use" : reason
        const inputTokens =
          promptTokens != null && cacheReadInputTokens != null
            ? promptTokens - cacheReadInputTokens
            : promptTokens
        emitEvent("message_delta", {
          type: "message_delta",
          delta: { stop_reason: finalReason, stop_sequence: null },
          usage: {
            ...(inputTokens != null ? { input_tokens: inputTokens } : {}),
            output_tokens: completionTokens ?? 0,
            ...(cacheReadInputTokens != null
              ? { cache_read_input_tokens: cacheReadInputTokens }
              : {}),
          },
        })
        emitEvent("message_stop", { type: "message_stop" })
        if (!opts?.onTurnOutcome) return
        if (
          thinkingMode !== "disabled" &&
          encryptedContent &&
          isValidGrokEncryptedContent(encryptedContent)
        ) {
          opts.onTurnOutcome({
            kind: "replayable",
            encrypted_content: encryptedContent,
            assistant_text: assistantText,
          })
        } else {
          // Completed turn with no replayable state (disabled / no ciphertext)
          // must not leave a prior turn's entry for a later inject.
          opts.onTurnOutcome({ kind: "clear" })
        }
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
            if (!data || data === "[DONE]") continue
            if (stopped) continue
            try {
              const ev = JSON.parse(data) as GrokResponsesEvent
              // Every event, not just the terminal one — see harvestUsage.
              harvestUsage(ev.response?.usage)
              if (ev.type === "response.failed" || ev.type === "error") {
                emitUpstreamError(
                  ev.response?.error?.message ||
                    ev.error?.message ||
                    ev.message ||
                    "upstream error",
                )
                continue
              }

              if (
                ev.type === "response.reasoning_summary_text.delta" &&
                ev.delta
              ) {
                appendThinking(ev.delta)
              } else if (ev.type === "response.output_text.delta" && ev.delta) {
                const text = ev.delta
                afterReasoning(() => appendText(text))
              } else if (
                ev.type === "response.output_item.added" &&
                ev.item?.type === "reasoning"
              ) {
                reasoningOpen = true
                // Pre-content encrypted snapshot — keep for fallback only;
                // final value arrives on output_item.done. Do not close the
                // thinking block until then (see afterReasoning).
                if (
                  typeof ev.item.encrypted_content === "string" &&
                  isValidGrokEncryptedContent(ev.item.encrypted_content)
                ) {
                  thinkingSignaturePending = ev.item.encrypted_content
                  if (!encryptedContent) {
                    encryptedContent = ev.item.encrypted_content
                  }
                }
                if (emitThinking) openThinking()
              } else if (
                ev.type === "response.output_item.done" &&
                ev.item?.type === "reasoning"
              ) {
                if (
                  typeof ev.item.encrypted_content === "string" &&
                  isValidGrokEncryptedContent(ev.item.encrypted_content)
                ) {
                  encryptedContent = ev.item.encrypted_content
                  thinkingSignaturePending = ev.item.encrypted_content
                }
                // Summary text may only appear on the done item.
                const summaryText = summaryTextFromItem(ev.item)
                if (summaryText) appendThinking(summaryText)
                // Signature-only reasoning (no summary text streamed): still
                // open a thinking block so signature_delta can be delivered.
                if (
                  emitThinking &&
                  !thinkingBlockOpen &&
                  !textBlockOpen &&
                  !liveTool &&
                  thinkingSignaturePending
                ) {
                  openThinking()
                }
                closeThinking()
                reasoningOpen = false
                flushDeferred()
              } else if (
                ev.type === "response.output_item.added" &&
                ev.item?.type === "function_call"
              ) {
                const itemId =
                  ev.item.id || ev.item.call_id || `item_${nextBlockIndex}`
                const callId = ev.item.call_id || itemId
                const name = ev.item.name || "unknown"
                afterReasoning(() => {
                  if (!liveTool || liveTool.itemId !== itemId) {
                    if (liveTool) closeTool()
                    openTool(itemId, callId, name)
                  }
                })
              } else if (ev.type === "response.function_call_arguments.delta") {
                const itemId = ev.item_id
                const delta = ev.delta ?? ""
                if (!itemId || !delta) continue
                afterReasoning(() => {
                  if (liveTool && liveTool.itemId === itemId) {
                    liveTool.sawArgs = true
                    emitEvent("content_block_delta", {
                      type: "content_block_delta",
                      index: liveTool.blockIndex,
                      delta: { type: "input_json_delta", partial_json: delta },
                    })
                  } else {
                    pendingArgs.set(
                      itemId,
                      (pendingArgs.get(itemId) ?? "") + delta,
                    )
                  }
                })
              } else if (
                ev.type === "response.output_item.done" &&
                ev.item?.type === "function_call"
              ) {
                const itemId = ev.item.id || ev.item.call_id || ""
                const callId =
                  ev.item.call_id || itemId || `call_${nextBlockIndex}`
                const name = ev.item.name || "unknown"
                const args = ev.item.arguments
                afterReasoning(() => {
                  if (!liveTool || liveTool.itemId !== itemId) {
                    if (liveTool) closeTool()
                    openTool(
                      itemId || `item_${nextBlockIndex}`,
                      callId,
                      name,
                    )
                  }
                  if (liveTool && !liveTool.sawArgs && args) {
                    liveTool.sawArgs = true
                    emitEvent("content_block_delta", {
                      type: "content_block_delta",
                      index: liveTool.blockIndex,
                      delta: {
                        type: "input_json_delta",
                        partial_json: args,
                      },
                    })
                  }
                  closeTool()
                })
              } else if (
                ev.type === "response.completed" ||
                ev.type === "response.done"
              ) {
                harvestUsage(ev.response?.usage)
                // Also harvest encrypted_content from completed output if stream
                // events omitted it.
                if (!encryptedContent && Array.isArray(ev.response?.output)) {
                  for (const item of ev.response!.output as Array<
                    Record<string, unknown>
                  >) {
                    if (
                      item.type === "reasoning" &&
                      typeof item.encrypted_content === "string" &&
                      isValidGrokEncryptedContent(item.encrypted_content)
                    ) {
                      encryptedContent = item.encrypted_content
                    }
                    if (item.type === "message" && Array.isArray(item.content)) {
                      for (const part of item.content as Array<
                        Record<string, unknown>
                      >) {
                        if (
                          part.type === "output_text" &&
                          typeof part.text === "string" &&
                          !assistantText
                        ) {
                          assistantText += part.text
                        }
                      }
                    }
                  }
                }
                sawCompleted = true
                stopReason = sawToolCall ? "tool_use" : "end_turn"
                finish(stopReason)
              }
            } catch {
              /* ignore parse */
            }
          }
        }
        // Truncated upstream: never fabricate a successful message_stop.
        if (!stopped) {
          if (sawCompleted) {
            finish(stopReason ?? (sawToolCall ? "tool_use" : "end_turn"))
          } else {
            emitUpstreamError(
              "upstream stalled: stream ended before response.completed",
            )
          }
        }
        controller.close()
      } catch (e) {
        controller.error(e)
      }
    },
    cancel() {
      void upstreamReader?.cancel()
    },
  })
}

function summaryTextFromItem(
  item: NonNullable<GrokResponsesEvent["item"]>,
): string {
  if (!Array.isArray(item.summary)) return ""
  const parts: string[] = []
  for (const part of item.summary) {
    if (typeof part === "string") parts.push(part)
    else if (part && typeof part === "object" && typeof part.text === "string") {
      parts.push(part.text)
    }
  }
  return parts.join("\n\n")
}

/** Collect Responses SSE into one Anthropic message object (non-stream clients). */
export async function collectGrokResponsesSseToAnthropic(
  body: ReadableStream<Uint8Array>,
  model: string,
  opts?: {
    thinkingMode?: GrokThinkingMode
    onTurnOutcome?: (outcome: GrokTurnOutcome) => void
  },
): Promise<Record<string, unknown> | { error: { message: string; type: string } }> {
  const stream = grokResponsesSseToAnthropicStream(body, model, opts)
  const decoder = new TextDecoder()
  const reader = stream.getReader()
  let buffer = ""
  let event = ""
  let error: { message: string; type: string } | null = null
  const content: Array<Record<string, unknown>> = []
  let stopReason = "end_turn"
  let usage: Record<string, unknown> = { input_tokens: 0, output_tokens: 0 }
  let msgId = `msg_${Date.now()}`
  /** Open block index → accumulating content */
  const open = new Map<
    number,
    { type: string; thinking?: string; text?: string; signature?: string; id?: string; name?: string; partial?: string }
  >()

  for (;;) {
    const { done, value } = await reader.read()
    if (done) break
    buffer += decoder.decode(value, { stream: true })
    const lines = buffer.split("\n")
    buffer = lines.pop() ?? ""
    for (const line of lines) {
      if (line.startsWith("event:")) event = line.slice(6).trim()
      else if (line.startsWith("data:")) {
        const data = line.slice(5).trim()
        if (!data) continue
        try {
          const json = JSON.parse(data) as Record<string, unknown>
          if (event === "error" || json.type === "error") {
            const err = json.error as { message?: string; type?: string } | undefined
            error = {
              message: err?.message || "upstream error",
              type: err?.type || "api_error",
            }
          } else if (event === "message_start") {
            const msg = json.message as { id?: string } | undefined
            if (msg?.id) msgId = msg.id
          } else if (event === "content_block_start") {
            const index = typeof json.index === "number" ? json.index : 0
            const block = json.content_block as Record<string, unknown>
            open.set(index, {
              type: String(block.type ?? "text"),
              thinking: block.type === "thinking" ? "" : undefined,
              text: block.type === "text" ? "" : undefined,
              signature:
                typeof block.signature === "string" ? block.signature : undefined,
              id: typeof block.id === "string" ? block.id : undefined,
              name: typeof block.name === "string" ? block.name : undefined,
              partial: block.type === "tool_use" ? "" : undefined,
            })
          } else if (event === "content_block_delta") {
            const index = typeof json.index === "number" ? json.index : 0
            const delta = json.delta as Record<string, unknown>
            const block = open.get(index)
            if (!block) continue
            if (delta.type === "thinking_delta") {
              block.thinking = (block.thinking ?? "") + String(delta.thinking ?? "")
            } else if (delta.type === "signature_delta") {
              block.signature = String(delta.signature ?? "")
            } else if (delta.type === "text_delta") {
              block.text = (block.text ?? "") + String(delta.text ?? "")
            } else if (delta.type === "input_json_delta") {
              block.partial = (block.partial ?? "") + String(delta.partial_json ?? "")
            }
          } else if (event === "content_block_stop") {
            const index = typeof json.index === "number" ? json.index : 0
            const block = open.get(index)
            if (!block) continue
            open.delete(index)
            if (block.type === "thinking") {
              const thinkingBlock: Record<string, unknown> = {
                type: "thinking",
                thinking: block.thinking ?? "",
              }
              if (block.signature) thinkingBlock.signature = block.signature
              content.push(thinkingBlock)
            } else if (block.type === "text") {
              content.push({ type: "text", text: block.text ?? "" })
            } else if (block.type === "tool_use") {
              let input: unknown = {}
              try {
                input = JSON.parse(block.partial || "{}")
              } catch {
                input = { raw: block.partial }
              }
              content.push({
                type: "tool_use",
                id: block.id,
                name: block.name,
                input,
              })
            }
          } else if (event === "message_delta") {
            const d = json.delta as { stop_reason?: string } | undefined
            if (d?.stop_reason) stopReason = d.stop_reason
            if (json.usage && typeof json.usage === "object") {
              usage = json.usage as Record<string, unknown>
            }
          }
        } catch {
          /* */
        }
        event = ""
      }
    }
  }

  if (error) return { error }
  return {
    id: msgId,
    type: "message",
    role: "assistant",
    model,
    content: content.length ? content : [{ type: "text", text: "" }],
    stop_reason: stopReason,
    stop_sequence: null,
    usage,
  }
}
