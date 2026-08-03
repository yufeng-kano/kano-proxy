/** Codex Responses SSE → OpenAI Chat Completions. */

import type { CodexReasoningReplayItem } from "../providers/codex_reasoning_cache"

type CodexEvent = {
  type?: string
  delta?: string
  item_id?: string
  item?: {
    type?: string
    id?: string
    call_id?: string
    name?: string
    arguments?: string
  }
  response?: {
    output?: unknown[]
    usage?: {
      input_tokens?: number
      output_tokens?: number
      input_tokens_details?: { cached_tokens?: number }
    }
    error?: { message?: string }
  }
  /** Present on a top-level `response.failed` / `error` event. */
  error?: { message?: string }
  message?: string
}

export type CodexReplayItemsCallback = (
  items: CodexReasoningReplayItem[],
  assistantText: string,
) => void

export type CodexSseOptions = {
  /** Called once for a successful completed turn, without buffering the stream. */
  onReplayItems?: CodexReplayItemsCallback
}

/** Pick the opaque replayable Responses output items, preserving upstream order. */
export function extractCodexReplayItems(output: unknown): CodexReasoningReplayItem[] {
  if (!Array.isArray(output)) return []
  const items: CodexReasoningReplayItem[] = []
  for (const item of output) {
    if (item === null || typeof item !== "object" || Array.isArray(item)) continue
    const type = (item as { type?: unknown }).type
    if (
      type === "reasoning" ||
      type === "function_call" ||
      type === "custom_tool_call"
    ) {
      items.push(item as CodexReasoningReplayItem)
    }
  }
  return items
}

function assistantTextFromOutput(output: unknown): string {
  if (!Array.isArray(output)) return ""
  let trailing = ""
  for (const item of output) {
    if (item === null || typeof item !== "object" || Array.isArray(item)) continue
    const record = item as {
      type?: unknown
      role?: unknown
      content?: unknown
    }
    if (record.type !== "message" || record.role !== "assistant") continue
    if (typeof record.content === "string") {
      trailing = record.content
      continue
    }
    if (!Array.isArray(record.content)) continue
    const text: string[] = []
    for (const part of record.content) {
      if (part === null || typeof part !== "object" || Array.isArray(part)) continue
      const value = part as { type?: unknown; text?: unknown }
      if (value.type === "output_text" && typeof value.text === "string") {
        text.push(value.text)
      }
    }
    if (text.length) trailing = text.join("")
  }
  return trailing
}

function replayItemsFromEvent(
  ev: CodexEvent,
  assistantText: string,
  opts: CodexSseOptions | undefined,
): void {
  if (!opts?.onReplayItems) return
  const text = assistantText || assistantTextFromOutput(ev.response?.output)
  try {
    opts.onReplayItems(extractCodexReplayItems(ev.response?.output), text)
  } catch {
    /* A replay tap must never break the upstream response. */
  }
}

/** `ev.response?.error?.message`, `ev.error?.message`, `ev.message`, then a fallback. */
function codexErrorMessage(ev: CodexEvent): string {
  return (
    ev.response?.error?.message || ev.error?.message || ev.message || "codex upstream failure"
  )
}

export function codexSseToOpenAIStream(
  body: ReadableStream<Uint8Array>,
  model: string,
  opts?: CodexSseOptions,
): ReadableStream<Uint8Array> {
  const decoder = new TextDecoder()
  const encoder = new TextEncoder()
  let buffer = ""
  const id = `chatcmpl_${crypto.randomUUID().replace(/-/g, "").slice(0, 24)}`
  let sentRole = false
  let finished = false
  let completed = false
  let assistantText = ""
  let sawToolCall = false
  let nextToolIndex = 0
  /** Responses item id → chat tool_calls index + whether any args streamed. */
  const tools = new Map<string, { toolIndex: number; sawArgs: boolean }>()
  /** Argument deltas that arrived before their item's `added` event. */
  const pendingArgs = new Map<string, string>()

  const captureCompleted = (ev: CodexEvent) => {
    if (completed) return
    completed = true
    replayItemsFromEvent(ev, assistantText, opts)
  }
  return new ReadableStream({
    async start(controller) {
      const reader = body.getReader()
      const chunk = (choice: Record<string, unknown>, usage?: Record<string, unknown>) => {
        controller.enqueue(
          encoder.encode(
            `data: ${JSON.stringify({
              id,
              object: "chat.completion.chunk",
              created: Math.floor(Date.now() / 1000),
              model,
              choices: [{ index: 0, finish_reason: null, ...choice }],
              ...(usage ? { usage } : {}),
            })}\n\n`,
          ),
        )
      }
      const ensureRole = () => {
        if (sentRole) return
        sentRole = true
        chunk({ delta: { role: "assistant", content: "" } })
      }
      const emitToolHeader = (
        itemId: string,
        callId: string,
        name: string,
      ): { toolIndex: number; sawArgs: boolean } => {
        ensureRole()
        const entry = { toolIndex: nextToolIndex++, sawArgs: false }
        tools.set(itemId, entry)
        sawToolCall = true
        chunk({
          delta: {
            tool_calls: [
              {
                index: entry.toolIndex,
                id: callId,
                type: "function",
                function: { name, arguments: "" },
              },
            ],
          },
        })
        const stashed = pendingArgs.get(itemId)
        if (stashed) {
          pendingArgs.delete(itemId)
          entry.sawArgs = true
          emitToolArgs(entry.toolIndex, stashed)
        }
        return entry
      }
      const emitToolArgs = (toolIndex: number, args: string) => {
        chunk({
          delta: {
            tool_calls: [{ index: toolIndex, function: { arguments: args } }],
          },
        })
      }
      const finish = (usage?: Record<string, unknown>) => {
        if (finished) return
        finished = true
        ensureRole()
        chunk(
          { delta: {}, finish_reason: sawToolCall ? "tool_calls" : "stop" },
          usage,
        )
        controller.enqueue(encoder.encode("data: [DONE]\n\n"))
      }
      /**
       * `response.failed` / `error`: a single OpenAI-shaped error line, no
       * finish chunk, no [DONE] — marks the stream finished so the trailing
       * `finish()` at read-loop end is a no-op and no further events process.
       */
      const emitError = (message: string) => {
        if (finished) return
        finished = true
        controller.enqueue(
          encoder.encode(
            `data: ${JSON.stringify({ error: { message, type: "upstream_error" } })}\n\n`,
          ),
        )
      }
      try {
        for (;;) {
          const { done, value } = await reader.read()
          if (done) break
          buffer += decoder.decode(value, { stream: true })
          const chunks = buffer.split("\n")
          buffer = chunks.pop() ?? ""
          for (const line of chunks) {
            if (!line.startsWith("data:")) continue
            const data = line.slice(5).trim()
            if (!data || data === "[DONE]") continue
            if (finished) continue
            try {
              const ev = JSON.parse(data) as CodexEvent
              if (
                ev.type === "response.output_text.delta" ||
                ev.type === "response.reasoning_summary_text.delta"
              ) {
                const text = ev.delta ?? ""
                if (!text) continue
                ensureRole()
                if (ev.type === "response.output_text.delta") {
                  assistantText += text
                  chunk({ delta: { content: text } })
                } else {
                  // De-facto extension field (DeepSeek/OpenRouter convention);
                  // OpenAI Chat Completions has no first-party reasoning field.
                  chunk({ delta: { reasoning_content: text } })
                }
              } else if (
                ev.type === "response.output_item.added" &&
                ev.item?.type === "function_call"
              ) {
                // Key symmetric with the done handler so an id-less item still
                // matches its own done event via call_id.
                const itemId =
                  ev.item.id || ev.item.call_id || `item_${nextToolIndex}`
                if (!tools.has(itemId)) {
                  emitToolHeader(
                    itemId,
                    ev.item.call_id || itemId,
                    ev.item.name || "unknown",
                  )
                }
              } else if (ev.type === "response.function_call_arguments.delta") {
                const itemId = ev.item_id
                const delta = ev.delta ?? ""
                if (!itemId || !delta) continue
                const entry = tools.get(itemId)
                if (entry) {
                  entry.sawArgs = true
                  emitToolArgs(entry.toolIndex, delta)
                } else {
                  // `added` not seen yet; hold until the header can go out.
                  pendingArgs.set(itemId, (pendingArgs.get(itemId) ?? "") + delta)
                }
              } else if (
                ev.type === "response.output_item.done" &&
                ev.item?.type === "function_call"
              ) {
                const itemId = ev.item.id || ev.item.call_id || ""
                const entry = itemId ? tools.get(itemId) : undefined
                if (!entry) {
                  // `added` never arrived. The done item's arguments are the
                  // backend's complete copy — a stash built from deltas may be
                  // missing pieces, so it yields to them.
                  if (itemId && ev.item.arguments) pendingArgs.delete(itemId)
                  const made = emitToolHeader(
                    itemId || `item_${nextToolIndex}`,
                    ev.item.call_id || itemId || `call_${nextToolIndex}`,
                    ev.item.name || "unknown",
                  )
                  if (!made.sawArgs && ev.item.arguments) {
                    made.sawArgs = true
                    emitToolArgs(made.toolIndex, ev.item.arguments)
                  }
                } else if (!entry.sawArgs && ev.item.arguments) {
                  // Header went out but no deltas ever came.
                  entry.sawArgs = true
                  emitToolArgs(entry.toolIndex, ev.item.arguments)
                }
              } else if (
                ev.type === "response.completed" ||
                ev.type === "response.done"
              ) {
                captureCompleted(ev)
                const u = ev.response?.usage
                finish(
                  u
                    ? {
                        prompt_tokens: u.input_tokens ?? 0,
                        completion_tokens: u.output_tokens ?? 0,
                        // Only when the Responses API actually reported it —
                        // absent means unreported, not zero (docs/database.md).
                        ...(typeof u.input_tokens_details?.cached_tokens === "number"
                          ? {
                              prompt_tokens_details: {
                                cached_tokens: u.input_tokens_details.cached_tokens,
                              },
                            }
                          : {}),
                      }
                    : undefined,
                )
              } else if (ev.type === "response.failed" || ev.type === "error") {
                emitError(codexErrorMessage(ev))
              }
            } catch {
              /* */
            }
          }
        }
        // Upstream ended without response.completed: terminate the stream
        // properly so downstream consumers do not hang on a half-open turn.
        // A no-op when emitError() already finished the stream.
        finish()
        controller.close()
      } catch (e) {
        controller.error(e)
      }
    },
  })
}

/** Discriminant used by callers to tell a real completion from an upstream failure. */
export type CodexUpstreamError = { error: { message: string; type: "upstream_error" } }

export async function collectCodexSse(
  body: ReadableStream<Uint8Array>,
  model: string,
  opts?: CodexSseOptions,
): Promise<Record<string, unknown> | CodexUpstreamError> {
  const decoder = new TextDecoder()
  const reader = body.getReader()
  let buffer = ""
  let text = ""
  let reasoningText = ""
  let completed = false
  let usage:
    | {
        input_tokens?: number
        output_tokens?: number
        input_tokens_details?: { cached_tokens?: number }
      }
    | undefined
  let error: CodexUpstreamError["error"] | null = null
  const tool_calls: Array<Record<string, unknown>> = []
  for (;;) {
    const { done, value } = await reader.read()
    if (done) break
    buffer += decoder.decode(value, { stream: true })
    const lines = buffer.split("\n")
    buffer = lines.pop() ?? ""
    for (const line of lines) {
      if (!line.startsWith("data:")) continue
      const data = line.slice(5).trim()
      if (!data) continue
      // Stop processing once a failure event lands: the turn is over, and a
      // later well-formed event must not overwrite the error with a fake
      // partial success.
      if (error) continue
      try {
        const ev = JSON.parse(data) as CodexEvent
        if (ev.type === "response.failed" || ev.type === "error") {
          error = { message: codexErrorMessage(ev), type: "upstream_error" }
          continue
        }
        if (ev.type === "response.output_text.delta" && ev.delta) text += ev.delta
        if (ev.type === "response.reasoning_summary_text.delta" && ev.delta) {
          reasoningText += ev.delta
        }
        if (ev.type === "response.output_item.done" && ev.item?.type === "function_call") {
          tool_calls.push({
            id: ev.item.call_id,
            type: "function",
            function: {
              name: ev.item.name,
              arguments: ev.item.arguments ?? "{}",
            },
          })
        }
        if (ev.type === "response.completed" || ev.type === "response.done") {
          if (!completed) {
            completed = true
            replayItemsFromEvent(ev, text, opts)
          }
          if (ev.response?.usage) usage = ev.response.usage
        }
      } catch {
        /* */
      }
    }
  }
  if (error) return { error }
  return {
    id: `chatcmpl_${Date.now()}`,
    object: "chat.completion",
    created: Math.floor(Date.now() / 1000),
    model,
    choices: [
      {
        index: 0,
        message: {
          role: "assistant",
          content: text || null,
          // De-facto extension field (DeepSeek/OpenRouter convention).
          reasoning_content: reasoningText || undefined,
          tool_calls: tool_calls.length ? tool_calls : undefined,
        },
        finish_reason: tool_calls.length ? "tool_calls" : "stop",
      },
    ],
    ...(usage
      ? {
          usage: {
            prompt_tokens: usage.input_tokens ?? 0,
            completion_tokens: usage.output_tokens ?? 0,
            // Only when the Responses API actually reported it — absent
            // means unreported, not zero (docs/database.md).
            ...(typeof usage.input_tokens_details?.cached_tokens === "number"
              ? {
                  prompt_tokens_details: {
                    cached_tokens: usage.input_tokens_details.cached_tokens,
                  },
                }
              : {}),
          },
        }
      : {}),
  }
}
