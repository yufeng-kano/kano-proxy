/**
 * Anthropic Messages ↔ Gemini `GenerateContent`, for the antigravity adapter's
 * `/anthropic` surface (docs/providers.md § Antigravity).
 *
 * Mirrors CLIProxyAPI `internal/translator/antigravity/claude/*`: the
 * Anthropic `system` field becomes `systemInstruction`, `tool_use` /
 * `tool_result` blocks become `functionCall` / `functionResponse` parts,
 * `thinking` blocks round-trip through `thought` parts carrying
 * `thoughtSignature`, and `thinking.budget_tokens` / `output_config.effort`
 * become `thinkingConfig`.
 *
 * This is a **conversion**, not the Claude-native passthrough claude-code
 * gets: `cache_control` has no Gemini equivalent and is dropped, and the tool
 * loop guard therefore applies on this ingress like it does for grok.
 */

import {
  anthropicStopReason,
  geminiParts,
  inlineDataOf,
  normalizeGeminiUsage,
  sanitizeJsonSchema,
  sseDataLines,
  unwrapAntigravityResponse,
  type GeminiContent,
  type GeminiPart,
  type GeminiRequest,
  type GeminiResponse,
} from "./gemini_wire"
import { mapReasoning, parseReasoningEffort, type ReasoningEffort } from "../utils/reasoning"

export class InvalidGeminiReasoningEffortError extends Error {}

export type GeminiThinkingMode = "disabled" | "enabled" | "default"

// ── Request: Anthropic → Gemini ────────────────────────────────────────────

type AnthropicBlock = {
  type?: string
  text?: unknown
  thinking?: unknown
  signature?: unknown
  id?: unknown
  name?: unknown
  input?: unknown
  tool_use_id?: unknown
  content?: unknown
  is_error?: unknown
  source?: { type?: string; media_type?: string; data?: string }
}

function systemToParts(system: unknown): GeminiPart[] {
  if (typeof system === "string") return system ? [{ text: system }] : []
  if (!Array.isArray(system)) return []
  const parts: GeminiPart[] = []
  for (const raw of system) {
    if (typeof raw === "string") {
      if (raw) parts.push({ text: raw })
      continue
    }
    if (!raw || typeof raw !== "object") continue
    const block = raw as AnthropicBlock
    if (block.type === "text" && typeof block.text === "string" && block.text) {
      parts.push({ text: block.text })
    }
  }
  return parts
}

/** An Anthropic `tool_result` body is free-form; keep JSON as JSON, text as text. */
function toolResultValue(content: unknown): unknown {
  if (typeof content === "string") return content
  if (!Array.isArray(content)) return content ?? {}
  const text = content
    .filter((b): b is AnthropicBlock => !!b && typeof b === "object")
    .filter((b) => b.type === "text" && typeof b.text === "string")
    .map((b) => b.text as string)
    .join("")
  return text
}

function blocksToParts(content: unknown): GeminiPart[] {
  if (typeof content === "string") return content ? [{ text: content }] : []
  if (!Array.isArray(content)) return []
  const parts: GeminiPart[] = []
  for (const raw of content) {
    if (!raw || typeof raw !== "object") continue
    const block = raw as AnthropicBlock
    switch (block.type) {
      case "text":
        if (typeof block.text === "string" && block.text) parts.push({ text: block.text })
        break
      case "thinking":
        if (typeof block.thinking === "string" && block.thinking) {
          parts.push({
            text: block.thinking,
            thought: true,
            // The signature is Gemini's own opaque `thoughtSignature` coming
            // back; echoing it is what keeps multi-turn thinking valid.
            ...(typeof block.signature === "string" && block.signature
              ? { thoughtSignature: block.signature }
              : {}),
          })
        }
        break
      case "image": {
        const source = block.source
        if (source?.type === "base64" && source.data) {
          parts.push({
            inlineData: { mimeType: source.media_type || "image/png", data: source.data },
          })
        }
        break
      }
      case "tool_use": {
        const name = typeof block.name === "string" ? block.name : ""
        if (!name) break
        const id = typeof block.id === "string" ? block.id : ""
        parts.push({ functionCall: { ...(id ? { id } : {}), name, args: block.input ?? {} } })
        break
      }
      case "tool_result": {
        const id = typeof block.tool_use_id === "string" ? block.tool_use_id : ""
        parts.push({
          functionResponse: {
            ...(id ? { id } : {}),
            // Anthropic identifies a result only by tool_use_id; the name is
            // filled in by the caller, which tracks id → name across the turn.
            name: "",
            response: block.is_error
              ? { error: toolResultValue(block.content) }
              : { result: toolResultValue(block.content) },
          },
        })
        break
      }
      default:
        break
    }
  }
  return parts
}

function mapAnthropicTools(tools: unknown): GeminiRequest["tools"] {
  if (!Array.isArray(tools)) return undefined
  const declarations: unknown[] = []
  for (const raw of tools) {
    if (!raw || typeof raw !== "object") continue
    const tool = raw as { name?: unknown; description?: unknown; input_schema?: unknown; type?: unknown }
    const name = typeof tool.name === "string" ? tool.name : ""
    // Anthropic server-side tools (`type: "web_search_20250305"` and friends)
    // have no function schema and no Gemini equivalent — skip, never forge one.
    if (!name || !tool.input_schema) continue
    declarations.push({
      name,
      ...(typeof tool.description === "string" ? { description: tool.description } : {}),
      parameters: sanitizeJsonSchema(tool.input_schema),
    })
  }
  return declarations.length ? [{ functionDeclarations: declarations }] : undefined
}

function mapAnthropicToolChoice(toolChoice: unknown): GeminiRequest["toolConfig"] {
  if (!toolChoice || typeof toolChoice !== "object") return undefined
  const choice = toolChoice as { type?: unknown; name?: unknown }
  switch (choice.type) {
    case "auto":
      return { functionCallingConfig: { mode: "AUTO" } }
    case "none":
      return { functionCallingConfig: { mode: "NONE" } }
    case "any":
      return { functionCallingConfig: { mode: "ANY" } }
    case "tool":
      return typeof choice.name === "string" && choice.name
        ? { functionCallingConfig: { mode: "ANY", allowedFunctionNames: [choice.name] } }
        : { functionCallingConfig: { mode: "ANY" } }
    default:
      return undefined
  }
}

/**
 * Anthropic states thinking two ways and this proxy accepts both: a `thinking`
 * object (`disabled` / `enabled` + `budget_tokens` / `adaptive`) and the
 * effort ladder (`output_config.effort`, or the `reasoning_effort` extension).
 * Budget wins when explicitly given — it is the more specific instruction.
 */
export function resolveGeminiThinking(body: Record<string, unknown>): {
  mode: GeminiThinkingMode
  thinkingConfig: Record<string, unknown>
} {
  const thinking = body.thinking as { type?: unknown; budget_tokens?: unknown } | undefined
  const type =
    thinking && typeof thinking === "object" ? String(thinking.type ?? "").toLowerCase() : ""

  if (type === "disabled") {
    return { mode: "disabled", thinkingConfig: { thinkingBudget: 0, includeThoughts: false } }
  }

  if (type === "enabled" && typeof thinking?.budget_tokens === "number") {
    return {
      mode: "enabled",
      thinkingConfig: { thinkingBudget: thinking.budget_tokens, includeThoughts: true },
    }
  }

  const outputConfig = body.output_config as { effort?: unknown } | undefined
  const rawEffort = outputConfig?.effort ?? body.reasoning_effort
  const effort = parseReasoningEffort(rawEffort)
  if (effort === "invalid") throw new InvalidGeminiReasoningEffortError("invalid reasoning_effort")
  if (effort) {
    const mapped = mapReasoning("antigravity", effort as ReasoningEffort)
    const disabled = !!mapped.thinkingConfig && "thinkingBudget" in mapped.thinkingConfig
    return {
      mode: disabled ? "disabled" : "enabled",
      thinkingConfig: { ...mapped.thinkingConfig, includeThoughts: !disabled },
    }
  }

  // Nothing asked for: let the model decide, but ask to see the thoughts so a
  // client that renders thinking blocks gets them.
  return { mode: "default", thinkingConfig: { includeThoughts: true } }
}

export type AnthropicToGeminiResult = {
  request: GeminiRequest
  thinkingMode: GeminiThinkingMode
}

/**
 * Anthropic Messages body → the inner Gemini `request`. The adapter wraps it
 * in the antigravity envelope.
 */
export function anthropicToGeminiRequest(
  body: Record<string, unknown>,
): AnthropicToGeminiResult {
  const contents: GeminiContent[] = []
  /** tool_use id → name, so a later `tool_result` can be named for Gemini. */
  const callNames = new Map<string, string>()

  for (const raw of (body.messages as unknown[]) ?? []) {
    if (!raw || typeof raw !== "object") continue
    const message = raw as { role?: unknown; content?: unknown }
    const role = message.role === "assistant" ? "model" : "user"
    const parts = blocksToParts(message.content)
    for (const part of parts) {
      if (part.functionCall?.name && part.functionCall.id) {
        callNames.set(part.functionCall.id, part.functionCall.name)
      }
      if (part.functionResponse && !part.functionResponse.name) {
        part.functionResponse.name = callNames.get(part.functionResponse.id ?? "") ?? "tool"
      }
    }
    if (!parts.length) continue
    const last = contents[contents.length - 1]
    if (last && last.role === role && Array.isArray(last.parts)) last.parts.push(...parts)
    else contents.push({ role, parts })
  }

  const generationConfig: Record<string, unknown> = {}
  if (typeof body.temperature === "number") generationConfig.temperature = body.temperature
  if (typeof body.top_p === "number") generationConfig.topP = body.top_p
  if (typeof body.max_tokens === "number") generationConfig.maxOutputTokens = body.max_tokens
  const stop = body.stop_sequences
  if (Array.isArray(stop) && stop.length) {
    generationConfig.stopSequences = stop.filter((s) => typeof s === "string")
  }
  const thinking = resolveGeminiThinking(body)
  generationConfig.thinkingConfig = thinking.thinkingConfig

  const systemParts = systemToParts(body.system)
  const tools = mapAnthropicTools(body.tools)
  const toolConfig = tools ? mapAnthropicToolChoice(body.tool_choice) : undefined

  return {
    request: {
      contents,
      ...(systemParts.length ? { systemInstruction: { role: "user", parts: systemParts } } : {}),
      ...(tools ? { tools } : {}),
      ...(toolConfig ? { toolConfig } : {}),
      generationConfig,
    },
    thinkingMode: thinking.mode,
  }
}

// ── Response: Gemini → Anthropic ───────────────────────────────────────────

function usageToAnthropic(resp: GeminiResponse | null): Record<string, number> {
  const usage = normalizeGeminiUsage(resp?.usageMetadata)
  if (!usage) return { input_tokens: 0, output_tokens: 0 }
  const cacheRead = usage.cachedTokens ?? 0
  return {
    // Anthropic's `input_tokens` excludes cache reads; Gemini's
    // `promptTokenCount` includes them, so the cached half is subtracted out.
    input_tokens: Math.max(0, (usage.promptTokens ?? 0) - cacheRead),
    ...(cacheRead ? { cache_read_input_tokens: cacheRead } : {}),
    output_tokens: (usage.completionTokens ?? 0) + (usage.reasoningTokens ?? 0),
  }
}

function toolUseBlock(part: GeminiPart, index: number): Record<string, unknown> | null {
  const call = part.functionCall
  if (!call?.name) return null
  return {
    type: "tool_use",
    id: call.id || `toolu_${index}_${crypto.randomUUID().replace(/-/g, "").slice(0, 16)}`,
    name: call.name,
    input: call.args ?? {},
  }
}

/** Non-stream `generateContent` body → an Anthropic `message`. */
export function geminiResponseToAnthropic(
  json: unknown,
  model: string,
  opts?: { thinkingMode?: GeminiThinkingMode },
): Record<string, unknown> {
  const resp = unwrapAntigravityResponse(json)
  const emitThinking = (opts?.thinkingMode ?? "default") !== "disabled"
  const content: Array<Record<string, unknown>> = []
  let sawToolCall = false
  let text = ""
  let thinking = ""
  let thinkingSignature = ""

  const flushThinking = () => {
    if (!thinking) return
    content.push({
      type: "thinking",
      thinking,
      ...(thinkingSignature ? { signature: thinkingSignature } : {}),
    })
    thinking = ""
    thinkingSignature = ""
  }
  const flushText = () => {
    if (!text) return
    content.push({ type: "text", text })
    text = ""
  }

  for (const part of geminiParts(resp)) {
    if (part.functionCall) {
      flushThinking()
      flushText()
      const block = toolUseBlock(part, content.length)
      if (block) {
        content.push(block)
        sawToolCall = true
      }
      continue
    }
    const inline = inlineDataOf(part)
    if (inline) {
      flushThinking()
      flushText()
      content.push({
        type: "image",
        source: { type: "base64", media_type: inline.mimeType, data: inline.data },
      })
      continue
    }
    if (typeof part.text !== "string" || !part.text) continue
    if (part.thought) {
      if (!emitThinking) continue
      flushText()
      thinking += part.text
      if (part.thoughtSignature) thinkingSignature = part.thoughtSignature
    } else {
      flushThinking()
      text += part.text
    }
  }
  flushThinking()
  flushText()

  // A safety-blocked prompt is a valid response with no candidates and a
  // `promptFeedback.blockReason` — surface it as a refusal, never as a
  // successful empty `end_turn` the client cannot tell apart from real output.
  const blocked = !resp?.candidates?.length && !!resp?.promptFeedback?.blockReason
  return {
    id: `msg_${crypto.randomUUID().replace(/-/g, "").slice(0, 24)}`,
    type: "message",
    role: "assistant",
    model,
    content,
    stop_reason: blocked
      ? "refusal"
      : anthropicStopReason(resp?.candidates?.[0]?.finishReason, sawToolCall),
    stop_sequence: null,
    usage: usageToAnthropic(resp),
  }
}

/**
 * `streamGenerateContent?alt=sse` → Anthropic Messages SSE. Content blocks
 * open and close as the part type changes; a thinking block emits its
 * `signature_delta` right before it closes, so a client that echoes the
 * signature keeps the next turn's thinking valid.
 */
export function geminiSseToAnthropicStream(
  body: ReadableStream<Uint8Array>,
  model: string,
  opts?: { thinkingMode?: GeminiThinkingMode },
): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder()
  const emitThinking = (opts?.thinkingMode ?? "default") !== "disabled"
  const msgId = `msg_${crypto.randomUUID().replace(/-/g, "").slice(0, 24)}`
  const reader = body.getReader()
  let clientCancelled = false

  return new ReadableStream({
    async start(controller) {
      let nextIndex = 0
      let open: { kind: "text" | "thinking" | "tool"; index: number } | null = null
      let pendingSignature = ""
      let sawToolCall = false
      let finishReason: string | undefined
      /** A candidate reported a `finishReason` — Gemini's terminal frame. A
       *  clean EOF without one is a truncated stream, not a completion. */
      let sawTerminal = false
      let blockReason = ""
      let usage: Record<string, number> = { input_tokens: 0, output_tokens: 0 }

      const emit = (event: string, data: unknown) => {
        if (clientCancelled) return
        controller.enqueue(encoder.encode(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`))
      }
      emit("message_start", {
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

      const closeBlock = () => {
        if (!open) return
        if (open.kind === "thinking" && pendingSignature) {
          emit("content_block_delta", {
            type: "content_block_delta",
            index: open.index,
            delta: { type: "signature_delta", signature: pendingSignature },
          })
          pendingSignature = ""
        }
        emit("content_block_stop", { type: "content_block_stop", index: open.index })
        open = null
      }
      const openBlock = (kind: "text" | "thinking", block: Record<string, unknown>) => {
        if (open?.kind === kind) return
        closeBlock()
        open = { kind, index: nextIndex++ }
        emit("content_block_start", {
          type: "content_block_start",
          index: open.index,
          content_block: block,
        })
      }

      try {
        for await (const data of sseDataLines(reader)) {
          if (data === "[DONE]") break
          let json: unknown
          try {
            json = JSON.parse(data)
          } catch {
            continue
          }
          const resp = unwrapAntigravityResponse(json)
          if (resp?.promptFeedback?.blockReason) blockReason = resp.promptFeedback.blockReason

          for (const part of geminiParts(resp)) {
            if (part.functionCall) {
              closeBlock()
              const block = toolUseBlock(part, nextIndex)
              if (!block) continue
              sawToolCall = true
              const index = nextIndex++
              emit("content_block_start", {
                type: "content_block_start",
                index,
                content_block: { ...block, input: {} },
              })
              // Gemini delivers a complete `args` object in one frame, so the
              // whole JSON goes out as a single partial_json delta.
              emit("content_block_delta", {
                type: "content_block_delta",
                index,
                delta: {
                  type: "input_json_delta",
                  partial_json: JSON.stringify(block.input ?? {}),
                },
              })
              emit("content_block_stop", { type: "content_block_stop", index })
              continue
            }
            const inline = inlineDataOf(part)
            if (inline) {
              closeBlock()
              const index = nextIndex++
              emit("content_block_start", {
                type: "content_block_start",
                index,
                content_block: {
                  type: "image",
                  source: { type: "base64", media_type: inline.mimeType, data: inline.data },
                },
              })
              emit("content_block_stop", { type: "content_block_stop", index })
              continue
            }
            if (typeof part.text !== "string" || !part.text) continue
            if (part.thought) {
              if (!emitThinking) continue
              openBlock("thinking", { type: "thinking", thinking: "" })
              emit("content_block_delta", {
                type: "content_block_delta",
                index: open!.index,
                delta: { type: "thinking_delta", thinking: part.text },
              })
              if (part.thoughtSignature) pendingSignature = part.thoughtSignature
            } else {
              openBlock("text", { type: "text", text: "" })
              emit("content_block_delta", {
                type: "content_block_delta",
                index: open!.index,
                delta: { type: "text_delta", text: part.text },
              })
            }
          }

          if (resp?.candidates?.[0]?.finishReason) {
            sawTerminal = true
            finishReason = resp.candidates[0].finishReason
          }
          if (resp?.usageMetadata) usage = usageToAnthropic(resp)
        }
        if (clientCancelled) return

        closeBlock()
        // A clean EOF is not a completion: without a terminal Gemini frame the
        // stream was truncated (no error is thrown for a quiet network close),
        // so end the turn with the documented error event, never a fabricated
        // `end_turn`. A blocked prompt is the one no-terminal shape that *is*
        // a real answer — Gemini reports it via promptFeedback, no candidate.
        if (!sawTerminal && !blockReason) {
          emit("error", {
            type: "error",
            error: { type: "api_error", message: "upstream stream ended before completion" },
          })
          controller.close()
          return
        }
        emit("message_delta", {
          type: "message_delta",
          delta: {
            stop_reason:
              !sawTerminal && blockReason
                ? "refusal"
                : anthropicStopReason(finishReason, sawToolCall),
            stop_sequence: null,
          },
          // The whole usage object, not just `output_tokens`: Gemini reports
          // input counts on the same frames, and `message_start` went out
          // before any of them arrived, so this is the client's only chance to
          // learn them (the repo's Anthropic usage sniffer merges field-wise).
          usage,
        })
        emit("message_stop", { type: "message_stop" })
        controller.close()
      } catch (e) {
        if (clientCancelled) return
        // Mid-stream failure ends the turn as an Anthropic `error` event, not
        // a fabricated message_stop (docs/api.md § Errors).
        try {
          emit("error", {
            type: "error",
            error: {
              type: "api_error",
              message: e instanceof Error ? e.message : "upstream stream error",
            },
          })
          controller.close()
        } catch {
          controller.error(e)
        }
      }
    },
    cancel(reason) {
      // Dispatch cancels the wrapper on client disconnect / idle timeout;
      // without this hook the conversion loop would keep consuming the
      // abandoned paid upstream generation until it ended on its own.
      clientCancelled = true
      reader.cancel(reason).catch(() => {})
    },
  })
}
