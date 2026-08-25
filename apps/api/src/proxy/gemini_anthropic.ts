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
  schemaDialectFor,
  type SchemaDialect,
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
      case "thinking": {
        const thinkingText = typeof block.thinking === "string" ? block.thinking : ""
        // The signature is Gemini's own opaque `thoughtSignature` coming
        // back; echoing it is what keeps multi-turn thinking valid. Gemini
        // itself emits signature-only thought parts (no text), so a replayed
        // block whose text is empty but whose signature is set still counts.
        const signature = typeof block.signature === "string" ? block.signature : ""
        if (thinkingText || signature) {
          parts.push({
            text: thinkingText,
            thought: true,
            ...(signature ? { thoughtSignature: signature } : {}),
          })
        }
        break
      }
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
        // A tool result can carry base64 images (screenshots, renders).
        // Gemini's functionResponse has no image field, but the same user
        // turn can carry inlineData parts alongside it — append them rather
        // than silently sending the model only the textual fragment.
        if (Array.isArray(block.content)) {
          for (const inner of block.content) {
            if (!inner || typeof inner !== "object") continue
            const innerBlock = inner as AnthropicBlock
            if (innerBlock.type !== "image") continue
            const source = innerBlock.source
            if (source?.type === "base64" && source.data) {
              parts.push({
                inlineData: { mimeType: source.media_type || "image/png", data: source.data },
              })
            }
          }
        }
        break
      }
      default:
        break
    }
  }
  return parts
}

function mapAnthropicTools(tools: unknown, dialect: SchemaDialect): GeminiRequest["tools"] {
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
      parameters: sanitizeJsonSchema(tool.input_schema, dialect),
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
    const rawParts = blocksToParts(message.content)
    const parts: GeminiPart[] = []
    for (let index = 0; index < rawParts.length; index++) {
      const part = rawParts[index]!
      if (part.functionCall?.name) {
        // Gemini requires a function-call signature to remain on the
        // functionCall part. Anthropic has no corresponding field on tool_use,
        // so the response emits it on the adjacent thinking block — the textual
        // one when thinking text was streaming, a signature-only one otherwise.
        // Move it back: a signature-only block is pure transport and is
        // dropped, a textual one stays as an unsigned thought part. Replaying
        // an unsigned functionCall makes Google reject the turn as missing
        // thought_signature in the functionCall part.
        const preceding = rawParts[index - 1]
        if (!part.thoughtSignature && preceding?.thought && preceding.thoughtSignature) {
          part.thoughtSignature = preceding.thoughtSignature
          // `preceding` is the same object already pushed into `parts`.
          if (preceding.text === "") {
            if (parts.at(-1) === preceding) parts.pop()
          } else {
            delete preceding.thoughtSignature
          }
        }
        if (part.functionCall.id) callNames.set(part.functionCall.id, part.functionCall.name)
      }
      if (part.functionResponse && !part.functionResponse.name) {
        part.functionResponse.name = callNames.get(part.functionResponse.id ?? "") ?? "tool"
      }
      parts.push(part)
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
  // Claude behind Antigravity rejects a union its Gemini sibling accepts, so
  // the schema dialect follows the model family (gemini_wire.ts).
  const tools = mapAnthropicTools(
    body.tools,
    schemaDialectFor(typeof body.model === "string" ? body.model : ""),
  )
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

/**
 * Only the fields this frame actually reported. Gemini repeats
 * `promptTokenCount` on later frames without always repeating the output
 * counts, so a caller merging frame-by-frame must not receive a defaulted
 * `output_tokens: 0` that would overwrite a real number it already had.
 */
function usageToAnthropic(resp: GeminiResponse | null): Record<string, number> {
  const usage = normalizeGeminiUsage(resp?.usageMetadata)
  if (!usage) return {}
  const cacheRead = usage.cachedTokens ?? 0
  const out: Record<string, number> = {}
  if (usage.promptTokens !== null) {
    // Anthropic's `input_tokens` excludes cache reads; Gemini's
    // `promptTokenCount` includes them, so the cached half is subtracted out.
    // The two are reported as a **pair**, `cache_read_input_tokens` included
    // when it is zero: a merging caller that took a later frame's
    // `promptTokenCount` while keeping an earlier frame's cache number would
    // count the cached tokens twice.
    out.input_tokens = Math.max(0, usage.promptTokens - cacheRead)
    out.cache_read_input_tokens = cacheRead
  }
  if (usage.completionTokens !== null || usage.reasoningTokens !== null) {
    out.output_tokens = (usage.completionTokens ?? 0) + (usage.reasoningTokens ?? 0)
  }
  return out
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
    // A signature with no accumulated text still makes a block — dropping it
    // would strip `thinking.signature` and break the client's next replay.
    if (!thinking && !thinkingSignature) return
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
      // Gemini can sign the functionCall part itself. Anthropic tool_use has
      // no signature field, so it rides on the adjacent thinking block (a
      // signature-only one when nothing was accumulated) — the replay path
      // already restores signed thinking blocks as signed thought parts.
      if (emitThinking && part.thoughtSignature) thinkingSignature = part.thoughtSignature
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
    if (part.thought) {
      if (!emitThinking) continue
      // The signature can ride on a thought part with no visible text — it
      // must be captured before the text guard or it never reaches the block.
      const hasText = typeof part.text === "string" && !!part.text
      if (!hasText && !part.thoughtSignature) continue
      flushText()
      if (hasText) thinking += part.text
      if (part.thoughtSignature) thinkingSignature = part.thoughtSignature
      continue
    }
    if (typeof part.text !== "string" || !part.text) continue
    // Gemini signs plain text parts too (think-then-answer, no tool call).
    // The capture must precede the flush or the signature never reaches a
    // block: it rides the adjacent thinking block, a signature-only one when
    // nothing was accumulated, exactly like a functionCall signature.
    if (emitThinking && part.thoughtSignature) thinkingSignature = part.thoughtSignature
    flushThinking()
    text += part.text
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
    usage: { input_tokens: 0, output_tokens: 0, ...usageToAnthropic(resp) },
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
  const lines = sseDataLines(reader)
  let clientCancelled = false
  let finished = false
  let emitted = 0

  let nextIndex = 0
  let open: { kind: "text" | "thinking" | "tool"; index: number } | null = null
  let pendingSignature = ""
  let sawToolCall = false
  let finishReason: string | undefined
  /** A candidate reported a `finishReason` — Gemini's terminal frame. A
   *  clean EOF without one is a truncated stream, not a completion. */
  let sawTerminal = false
  let blockReason = ""
  /** Merged field-wise across frames — see `usageToAnthropic`. */
  const usage: Record<string, number> = { input_tokens: 0, output_tokens: 0 }
  /** A frame has reported `promptTokenCount`, so `input_tokens` is real. */
  let sawInputTokens = false
  let messageStarted = false

  // Pull-driven pump, same shape as the OpenAI converter: each pull()
  // consumes upstream frames only until it has enqueued something, so a slow
  // or paused client applies backpressure to the paid upstream generation
  // instead of the whole remainder buffering in Worker memory.
  return new ReadableStream({
    // No `start()`: `message_start` is emitted from `pull()` instead, once a
    // frame has carried `usageMetadata`. Anthropic clients read the context
    // size off its `usage.input_tokens`, and Gemini reports counts on its
    // stream frames, not before them — emitting the event up front can only
    // put a zero there, which is what left Claude Code's ctx blank
    // (docs/api.md § Streaming).
    async pull(controller) {
      if (finished || clientCancelled) return
      const emitRaw = (event: string, data: unknown) => {
        if (clientCancelled) return
        controller.enqueue(encoder.encode(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`))
        emitted++
      }
      /**
       * Every event except `error` is preceded by `message_start`, which is
       * held back until a frame reported `promptTokenCount`. If content is
       * ready and no count ever arrived, the turn fails instead of shipping a
       * zero: a wrong context size is worse than a visible error, and there is
       * no honest number to substitute (docs/api.md § Streaming).
       */
      const emit = (event: string, data: unknown) => {
        // Not gated on `finished`: the terminal branch sets it *before*
        // emitting message_delta / message_stop, and swallowing those
        // truncates every stream.
        if (clientCancelled) return
        if (event !== "error" && !messageStarted) {
          if (!sawInputTokens) {
            finished = true
            emitRaw("error", {
              type: "error",
              error: {
                type: "api_error",
                message: "upstream reported no prompt token count before its first content frame",
              },
            })
            controller.close()
            return
          }
          messageStarted = true
          emitRaw("message_start", {
            type: "message_start",
            message: {
              id: msgId,
              type: "message",
              role: "assistant",
              model,
              content: [],
              stop_reason: null,
              stop_sequence: null,
              usage: { ...usage, output_tokens: 0 },
            },
          })
        }
        emitRaw(event, data)
      }
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

      const before = emitted
      try {
        for (;;) {
          const next = await lines.next()
          if (clientCancelled) {
            finished = true
            return
          }
          if (next.done || next.value === "[DONE]") {
            finished = true
            if (!next.done) await lines.return(undefined)

            closeBlock()
            // A clean EOF is not a completion: without a terminal Gemini
            // frame the stream was truncated (no error is thrown for a quiet
            // network close), so end the turn with the documented error
            // event, never a fabricated `end_turn`. A blocked prompt is the
            // one no-terminal shape that *is* a real answer — Gemini reports
            // it via promptFeedback, no candidate.
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
              // The whole usage object, not just `output_tokens`: Gemini
              // reports input counts on the same frames, and `message_start`
              // went out before any of them arrived, so this is the client's
              // only chance to learn them (the repo's Anthropic usage sniffer
              // merges field-wise).
              usage,
            })
            emit("message_stop", { type: "message_stop" })
            controller.close()
            return
          }

          let json: unknown
          try {
            json = JSON.parse(next.value)
          } catch {
            continue
          }
          const resp = unwrapAntigravityResponse(json)
          // Before anything is emitted from this frame: `message_start` is
          // gated on a real `input_tokens`, and Gemini carries the count on
          // the same frame as the first content part, so reading it after the
          // parts loop would fail the turn on a frame that did report it.
          if (resp?.usageMetadata) {
            const reported = usageToAnthropic(resp)
            Object.assign(usage, reported)
            // `usage` is seeded with a zero, so the flag has to key off what
            // *this frame* carried, not off the merged object.
            if (reported.input_tokens !== undefined) sawInputTokens = true
          }
          if (resp?.promptFeedback?.blockReason) blockReason = resp.promptFeedback.blockReason

          for (const part of geminiParts(resp)) {
            if (part.functionCall) {
              // A signature on the functionCall part itself rides on the
              // adjacent thinking block, whose closing signature_delta goes
              // out right before the tool_use block opens.
              if (emitThinking && part.thoughtSignature) {
                openBlock("thinking", { type: "thinking", thinking: "" })
                pendingSignature = part.thoughtSignature
              }
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
            if (part.thought) {
              if (!emitThinking) continue
              // A signature-only thought part (no text) is a valid shape —
              // the block still opens so closeBlock emits its signature_delta.
              const hasText = typeof part.text === "string" && !!part.text
              if (!hasText && !part.thoughtSignature) continue
              openBlock("thinking", { type: "thinking", thinking: "" })
              if (hasText) {
                emit("content_block_delta", {
                  type: "content_block_delta",
                  index: open!.index,
                  delta: { type: "thinking_delta", thinking: part.text },
                })
              }
              if (part.thoughtSignature) pendingSignature = part.thoughtSignature
              continue
            }
            if (typeof part.text !== "string" || !part.text) continue
            // A signature on a plain text part rides on the adjacent thinking
            // block (opened empty when nothing was streaming), whose closing
            // signature_delta goes out right before the text block opens.
            if (emitThinking && part.thoughtSignature) {
              openBlock("thinking", { type: "thinking", thinking: "" })
              pendingSignature = part.thoughtSignature
            }
            openBlock("text", { type: "text", text: "" })
            emit("content_block_delta", {
              type: "content_block_delta",
              index: open!.index,
              delta: { type: "text_delta", text: part.text },
            })
          }

          if (resp?.candidates?.[0]?.finishReason) {
            sawTerminal = true
            finishReason = resp.candidates[0].finishReason
          }

          // Something went out — yield to the client until it pulls again.
          if (emitted > before) return
        }
      } catch (e) {
        finished = true
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
      finished = true
      reader.cancel(reason).catch(() => {})
    },
  })
}
