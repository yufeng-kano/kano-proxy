/**
 * OpenAI Chat Completions ↔ Gemini `GenerateContent`, for the antigravity
 * adapter's `/openai/v1` surface (docs/providers.md § Antigravity).
 *
 * Mirrors CLIProxyAPI
 * `internal/translator/antigravity/openai/chat-completions/*` for the wire
 * details: system messages become `systemInstruction`, assistant `tool_calls`
 * become `functionCall` parts and the matching `role: "tool"` messages become
 * `functionResponse` parts in a following `user` turn, `response_format`
 * becomes `responseMimeType` + `responseSchema`, and `reasoning_effort`
 * becomes `thinkingConfig`.
 */

import type { ChatCompletionRequest } from "../providers/types"
import { mapReasoning } from "../utils/reasoning"
import {
  geminiParts,
  inlineDataOf,
  normalizeGeminiUsage,
  openaiFinishReason,
  sanitizeJsonSchema,
  sseDataLines,
  unwrapAntigravityResponse,
  type GeminiContent,
  type GeminiPart,
  type GeminiRequest,
  type GeminiResponse,
} from "./gemini_wire"

// ── Request: OpenAI → Gemini ───────────────────────────────────────────────

type OpenAIMessage = {
  role?: string
  content?: unknown
  reasoning_content?: unknown
  /** Proxy extension: Gemini's opaque `thoughtSignature`, echoed back verbatim
   *  alongside `reasoning_content` to keep multi-turn thinking valid
   *  (docs/providers.md § Antigravity "Thinking"). */
  reasoning_signature?: unknown
  tool_calls?: Array<{
    id?: string
    type?: string
    function?: { name?: string; arguments?: string }
  }>
  tool_call_id?: string
  name?: string
}

function textPart(text: string): GeminiPart {
  return { text }
}

/**
 * `data:<mime>;base64,<payload>` → an inline part. A remote `https://` image
 * URL is dropped rather than forwarded: Gemini has no URL image part on this
 * endpoint, and fetching it server-side would make the proxy an open fetcher.
 */
function imageUrlPart(url: unknown): GeminiPart | null {
  if (typeof url !== "string") return null
  const match = /^data:([^;,]+);base64,(.+)$/s.exec(url.trim())
  if (!match) return null
  return { inlineData: { mimeType: match[1], data: match[2] } }
}

function contentToParts(content: unknown): GeminiPart[] {
  if (typeof content === "string") return content ? [textPart(content)] : []
  if (!Array.isArray(content)) return []
  const parts: GeminiPart[] = []
  for (const raw of content) {
    if (!raw || typeof raw !== "object") continue
    const item = raw as { type?: string; text?: unknown; image_url?: { url?: unknown } }
    if (item.type === "text" && typeof item.text === "string" && item.text) {
      parts.push(textPart(item.text))
    } else if (item.type === "image_url") {
      const part = imageUrlPart(item.image_url?.url)
      if (part) parts.push(part)
    }
  }
  return parts
}

/** Tool results arrive as their own OpenAI messages; Gemini wants them inside a user turn. */
function toolResponsePart(name: string, callId: string, content: unknown): GeminiPart {
  let result: unknown = content
  if (typeof content === "string") {
    try {
      result = JSON.parse(content)
    } catch {
      result = content
    }
  }
  return {
    functionResponse: {
      ...(callId ? { id: callId } : {}),
      name,
      response: { result: result ?? {} },
    },
  }
}

function pushContent(contents: GeminiContent[], role: "user" | "model", parts: GeminiPart[]): void {
  if (!parts.length) return
  const last = contents[contents.length - 1]
  // Gemini rejects two consecutive turns with the same role, and a tool result
  // following a user message is exactly that shape — merge instead.
  if (last && last.role === role && Array.isArray(last.parts)) {
    last.parts.push(...parts)
    return
  }
  contents.push({ role, parts })
}

function messagesToGemini(messages: unknown[]): {
  contents: GeminiContent[]
  systemInstruction?: GeminiContent
} {
  const systemParts: GeminiPart[] = []
  const contents: GeminiContent[] = []
  /** tool_call_id → function name, so a later `role: "tool"` can be named. */
  const callNames = new Map<string, string>()

  for (const raw of messages) {
    if (!raw || typeof raw !== "object") continue
    const message = raw as OpenAIMessage
    const role = message.role

    if (role === "system" || role === "developer") {
      systemParts.push(...contentToParts(message.content))
      continue
    }

    if (role === "user") {
      pushContent(contents, "user", contentToParts(message.content))
      continue
    }

    if (role === "tool") {
      const callId = typeof message.tool_call_id === "string" ? message.tool_call_id : ""
      const name = callNames.get(callId) || (typeof message.name === "string" ? message.name : "")
      if (!name) continue
      pushContent(contents, "user", [toolResponsePart(name, callId, message.content)])
      continue
    }

    if (role === "assistant") {
      const parts: GeminiPart[] = []
      if (typeof message.reasoning_content === "string" && message.reasoning_content) {
        parts.push({
          text: message.reasoning_content,
          thought: true,
          // The signature is Gemini's own `thoughtSignature` coming back;
          // echoing it is what keeps multi-turn thinking valid upstream.
          ...(typeof message.reasoning_signature === "string" && message.reasoning_signature
            ? { thoughtSignature: message.reasoning_signature }
            : {}),
        })
      }
      parts.push(...contentToParts(message.content))
      for (const call of message.tool_calls ?? []) {
        const name = call.function?.name
        if (!name) continue
        const id = typeof call.id === "string" ? call.id : ""
        if (id) callNames.set(id, name)
        let args: unknown = {}
        const rawArgs = call.function?.arguments
        if (typeof rawArgs === "string" && rawArgs.trim()) {
          try {
            args = JSON.parse(rawArgs)
          } catch {
            // A tool call the model never finished emitting is worse than
            // useless as a string here — send an empty object so the turn
            // still validates.
            args = {}
          }
        }
        parts.push({ functionCall: { ...(id ? { id } : {}), name, args } })
      }
      pushContent(contents, "model", parts)
    }
  }

  return {
    contents,
    ...(systemParts.length ? { systemInstruction: { role: "user", parts: systemParts } } : {}),
  }
}

function mapTools(tools: unknown): GeminiRequest["tools"] {
  if (!Array.isArray(tools)) return undefined
  const declarations: unknown[] = []
  for (const raw of tools) {
    if (!raw || typeof raw !== "object") continue
    const tool = raw as { type?: string; function?: Record<string, unknown> }
    if (tool.type !== "function" || !tool.function) continue
    const fn = tool.function
    const name = typeof fn.name === "string" ? fn.name : ""
    if (!name) continue
    const parameters = sanitizeJsonSchema(fn.parameters) ?? { type: "object", properties: {} }
    declarations.push({
      name,
      ...(typeof fn.description === "string" ? { description: fn.description } : {}),
      parameters,
    })
  }
  return declarations.length ? [{ functionDeclarations: declarations }] : undefined
}

function mapToolChoice(toolChoice: unknown): GeminiRequest["toolConfig"] {
  if (toolChoice === "auto") return { functionCallingConfig: { mode: "AUTO" } }
  if (toolChoice === "none") return { functionCallingConfig: { mode: "NONE" } }
  if (toolChoice === "required") return { functionCallingConfig: { mode: "ANY" } }
  if (toolChoice && typeof toolChoice === "object") {
    const name = (toolChoice as { function?: { name?: unknown } }).function?.name
    if (typeof name === "string" && name) {
      return { functionCallingConfig: { mode: "ANY", allowedFunctionNames: [name] } }
    }
  }
  return undefined
}

function mapResponseFormat(responseFormat: unknown): Record<string, unknown> {
  if (!responseFormat || typeof responseFormat !== "object") return {}
  const format = responseFormat as { type?: string; json_schema?: { schema?: unknown } }
  if (format.type === "json_object") return { responseMimeType: "application/json" }
  if (format.type !== "json_schema") return {}
  const schema = format.json_schema?.schema
  return {
    responseMimeType: "application/json",
    ...(schema ? { responseSchema: sanitizeJsonSchema(schema) } : {}),
  }
}

/**
 * Builds the inner `request` object. The adapter wraps it in the antigravity
 * envelope (`model` / `project` / `requestId` / `sessionId`).
 */
export function openaiToGeminiRequest(req: ChatCompletionRequest): GeminiRequest {
  const { contents, systemInstruction } = messagesToGemini(req.messages)
  const generationConfig: Record<string, unknown> = {}
  if (req.temperature != null) generationConfig.temperature = req.temperature
  if (req.top_p != null) generationConfig.topP = req.top_p
  if (req.max_tokens != null) generationConfig.maxOutputTokens = req.max_tokens
  if (req.stop?.length) generationConfig.stopSequences = req.stop
  Object.assign(generationConfig, mapResponseFormat(req.response_format))

  const mapped = mapReasoning("antigravity", req.reasoning_effort)
  if (mapped.thinkingConfig) {
    const disabled = "thinkingBudget" in mapped.thinkingConfig
    generationConfig.thinkingConfig = {
      ...mapped.thinkingConfig,
      // Without this the model still thinks but returns no thought parts, so
      // `reasoning_content` would silently vanish for every caller.
      includeThoughts: !disabled,
    }
  } else {
    generationConfig.thinkingConfig = { includeThoughts: true }
  }

  const tools = mapTools(req.tools)
  const toolConfig = tools ? mapToolChoice(req.tool_choice) : undefined

  return {
    contents,
    ...(systemInstruction ? { systemInstruction } : {}),
    ...(tools ? { tools } : {}),
    ...(toolConfig ? { toolConfig } : {}),
    ...(Object.keys(generationConfig).length ? { generationConfig } : {}),
  }
}

// ── Response: Gemini → OpenAI ──────────────────────────────────────────────

export type OpenAIToolCall = {
  id: string
  index: number
  type: "function"
  function: { name: string; arguments: string }
}

function toolCallFromPart(part: GeminiPart, index: number): OpenAIToolCall | null {
  const call = part.functionCall
  if (!call?.name) return null
  return {
    // Gemini only sometimes returns an id; a stable synthetic one is required
    // because the client has to echo it back on the tool result.
    id: call.id || `call_${index}_${crypto.randomUUID().replace(/-/g, "").slice(0, 16)}`,
    index,
    type: "function",
    function: { name: call.name, arguments: JSON.stringify(call.args ?? {}) },
  }
}

function usageToOpenAI(resp: GeminiResponse | null): Record<string, unknown> | null {
  const usage = normalizeGeminiUsage(resp?.usageMetadata)
  if (!usage) return null
  const completion =
    usage.completionTokens === null && usage.reasoningTokens === null
      ? null
      : (usage.completionTokens ?? 0) + (usage.reasoningTokens ?? 0)
  const out: Record<string, unknown> = {
    prompt_tokens: usage.promptTokens ?? 0,
    completion_tokens: completion ?? 0,
    total_tokens: usage.totalTokens ?? (usage.promptTokens ?? 0) + (completion ?? 0),
  }
  if (usage.cachedTokens !== null) {
    out.prompt_tokens_details = { cached_tokens: usage.cachedTokens }
  }
  if (usage.reasoningTokens !== null) {
    out.completion_tokens_details = { reasoning_tokens: usage.reasoningTokens }
  }
  return out
}

/** Non-stream `v1internal:generateContent` body → an OpenAI `chat.completion`. */
export function geminiResponseToOpenAI(json: unknown, model: string): Record<string, unknown> {
  const resp = unwrapAntigravityResponse(json)
  const parts = geminiParts(resp)
  let content = ""
  let reasoning = ""
  let reasoningSignature = ""
  const toolCalls: OpenAIToolCall[] = []
  const images: Array<{ type: "image_url"; index: number; image_url: { url: string } }> = []

  for (const part of parts) {
    if (part.functionCall) {
      const call = toolCallFromPart(part, toolCalls.length)
      if (call) toolCalls.push(call)
      continue
    }
    const inline = inlineDataOf(part)
    if (inline) {
      images.push({
        type: "image_url",
        index: images.length,
        image_url: { url: `data:${inline.mimeType};base64,${inline.data}` },
      })
      continue
    }
    // A signature can ride on a thought part with no visible text.
    if (part.thought && part.thoughtSignature) reasoningSignature = part.thoughtSignature
    if (typeof part.text !== "string" || !part.text) continue
    if (part.thought) reasoning += part.text
    else content += part.text
  }

  // A safety-blocked prompt is a valid response with no candidates and a
  // `promptFeedback.blockReason` — surface it as a content filter, never as a
  // successful blank answer the client cannot tell apart from real output.
  const blocked = !resp?.candidates?.length && !!resp?.promptFeedback?.blockReason
  const finishReason = toolCalls.length
    ? "tool_calls"
    : blocked
      ? "content_filter"
      : openaiFinishReason(resp?.candidates?.[0]?.finishReason)
  const usage = usageToOpenAI(resp)

  return {
    id: resp?.responseId || `chatcmpl_${crypto.randomUUID().replace(/-/g, "").slice(0, 24)}`,
    object: "chat.completion",
    created: Math.floor(Date.now() / 1000),
    model,
    choices: [
      {
        index: 0,
        message: {
          role: "assistant",
          content: content || null,
          ...(reasoning ? { reasoning_content: reasoning } : {}),
          ...(reasoningSignature ? { reasoning_signature: reasoningSignature } : {}),
          ...(toolCalls.length ? { tool_calls: toolCalls } : {}),
          ...(images.length ? { images } : {}),
        },
        finish_reason: finishReason,
      },
    ],
    ...(usage ? { usage } : {}),
  }
}

/**
 * `v1internal:streamGenerateContent?alt=sse` → OpenAI `chat.completion.chunk`
 * SSE. Each upstream frame carries whole parts (Gemini streams by part, not by
 * token boundary within a part), so one frame becomes one chunk; the terminal
 * chunk carries `finish_reason` and `usage`.
 */
export function geminiSseToOpenAIStream(
  body: ReadableStream<Uint8Array>,
  model: string,
): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder()
  const id = `chatcmpl_${crypto.randomUUID().replace(/-/g, "").slice(0, 24)}`
  const created = Math.floor(Date.now() / 1000)
  const reader = body.getReader()
  let clientCancelled = false

  return new ReadableStream({
    async start(controller) {
      let toolIndex = 0
      let sawToolCall = false
      let finishReason: string | null = null
      /** A candidate reported a `finishReason` — Gemini's terminal frame. A
       *  clean EOF without one is a truncated stream, not a completion. */
      let sawTerminal = false
      let blockReason = ""
      let usage: Record<string, unknown> | null = null
      let roleSent = false

      const emit = (payload: Record<string, unknown>) => {
        if (clientCancelled) return
        controller.enqueue(encoder.encode(`data: ${JSON.stringify(payload)}\n\n`))
      }
      const chunk = (delta: Record<string, unknown>) => {
        if (!roleSent) {
          delta = { role: "assistant", ...delta }
          roleSent = true
        }
        emit({
          id,
          object: "chat.completion.chunk",
          created,
          model,
          choices: [{ index: 0, delta, finish_reason: null }],
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
              const call = toolCallFromPart(part, toolIndex++)
              if (!call) continue
              sawToolCall = true
              chunk({ tool_calls: [call] })
              continue
            }
            const inline = inlineDataOf(part)
            if (inline) {
              chunk({
                images: [
                  {
                    type: "image_url",
                    index: 0,
                    image_url: { url: `data:${inline.mimeType};base64,${inline.data}` },
                  },
                ],
              })
              continue
            }
            if (part.thought) {
              const delta: Record<string, unknown> = {}
              if (typeof part.text === "string" && part.text) delta.reasoning_content = part.text
              // The signature can ride on a thought part with no visible text.
              if (part.thoughtSignature) delta.reasoning_signature = part.thoughtSignature
              if (Object.keys(delta).length) chunk(delta)
              continue
            }
            if (typeof part.text !== "string" || !part.text) continue
            chunk({ content: part.text })
          }
          const upstreamFinish = resp?.candidates?.[0]?.finishReason
          if (upstreamFinish) {
            sawTerminal = true
            finishReason = openaiFinishReason(upstreamFinish)
          }
          const chunkUsage = usageToOpenAI(resp)
          if (chunkUsage) usage = chunkUsage
        }
        if (clientCancelled) return

        // A clean EOF is not a completion: without a terminal Gemini frame the
        // stream was truncated (no error is thrown for a quiet network close),
        // so end the turn with the documented stream error, never a fabricated
        // `stop`. A blocked prompt is the one no-terminal shape that *is* a
        // real answer — Gemini reports it via promptFeedback with no candidate.
        if (!sawTerminal && !blockReason) {
          emit({
            error: { message: "upstream stream ended before completion", type: "upstream_error" },
          })
          controller.close()
          return
        }

        const terminalFinish = sawToolCall
          ? "tool_calls"
          : !sawTerminal && blockReason
            ? "content_filter"
            : (finishReason ?? "stop")
        emit({
          id,
          object: "chat.completion.chunk",
          created,
          model,
          choices: [{ index: 0, delta: {}, finish_reason: terminalFinish }],
          ...(usage ? { usage } : {}),
        })
        controller.enqueue(encoder.encode("data: [DONE]\n\n"))
        controller.close()
      } catch (e) {
        if (clientCancelled) return
        // Mid-stream upstream failure: an OpenAI-shaped error line ends the
        // turn, never a fabricated successful finish (same rule as codex).
        try {
          emit({
            error: {
              message: e instanceof Error ? e.message : "upstream stream error",
              type: "upstream_error",
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
