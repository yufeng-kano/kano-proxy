/**
 * OpenAI Responses API ↔ Chat Completions (docs/api.md § `POST
 * /openai/v1/responses`). The conversion path for every non-codex target:
 * a Responses request becomes the internal Chat shape, dispatches like
 * `/openai/v1/chat/completions`, and the Chat result becomes Responses
 * events / a Response object. Also the small in-stream error rewrite the
 * native codex path needs so dispatch's OpenAI-shaped error frames reach a
 * Responses client as `response.failed`.
 */

/** A request field the proxy cannot honour (needs server-side state, or an unconvertible part). Route → `400 unsupported_field`. */
export class UnsupportedResponsesField extends Error {
  constructor(
    readonly field: string,
    message: string,
  ) {
    super(message)
    this.name = "UnsupportedResponsesField"
  }
}

/** How a flattened Chat function name maps back to the Responses tool the client declared. */
export type ResponsesToolRef = {
  kind: "function" | "custom"
  /** Present for a tool that came out of a `namespace` group — echoed on the call item. */
  namespace?: string
  /** The client's own (unflattened) tool name. */
  name: string
}

export type ResponsesToolNames = Map<string, ResponsesToolRef>

export type ResponsesToChatResult = {
  /** Chat Completions-shaped body — the named fields dispatch reads, and the `rawBody` a custom-openai adapter forwards. */
  chat: Record<string, unknown>
  toolNames: ResponsesToolNames
}

/** Stub that replaces a hosted `web_search` tool on the conversion path (docs/api.md "Web search on the conversion path"). */
export const WEB_SEARCH_STUB_TOOL = {
  type: "function",
  function: {
    name: "web_search",
    description:
      "Web search is NOT available for this model through this proxy. Do not call this tool; it always fails. Answer from your own knowledge and the tools that do work.",
    parameters: {
      type: "object",
      properties: { query: { type: "string", description: "Unused." } },
      additionalProperties: false,
    },
  },
} as const

const NAMESPACE_SEPARATOR = "__"

function isRecord(v: unknown): v is Record<string, unknown> {
  return v !== null && typeof v === "object" && !Array.isArray(v)
}

function textOfParts(content: unknown, joiner: string): string {
  if (typeof content === "string") return content
  if (!Array.isArray(content)) return ""
  const out: string[] = []
  for (const part of content) {
    if (typeof part === "string") {
      out.push(part)
      continue
    }
    if (!isRecord(part)) continue
    if (
      (part.type === "input_text" || part.type === "output_text" || part.type === "text") &&
      typeof part.text === "string"
    ) {
      out.push(part.text)
    } else if (part.type === "refusal" && typeof part.refusal === "string") {
      out.push(part.refusal)
    }
  }
  return out.join(joiner)
}

/** User-role content: text and images become Chat parts; anything else is a hard reject rather than a silent drop. */
function userContentParts(content: unknown): string | unknown[] {
  if (typeof content === "string") return content
  if (!Array.isArray(content)) return ""
  const parts: unknown[] = []
  for (const part of content) {
    if (typeof part === "string") {
      parts.push({ type: "text", text: part })
      continue
    }
    if (!isRecord(part)) continue
    const type = part.type
    if ((type === "input_text" || type === "text") && typeof part.text === "string") {
      parts.push({ type: "text", text: part.text })
    } else if (type === "input_image") {
      const url =
        typeof part.image_url === "string"
          ? part.image_url
          : isRecord(part.image_url) && typeof part.image_url.url === "string"
            ? part.image_url.url
            : typeof part.file_id === "string"
              ? null
              : null
      if (!url) {
        throw new UnsupportedResponsesField(
          "input.content.input_image",
          "input_image needs an image_url (data: URL or https URL); file_id references are not supported",
        )
      }
      const image: Record<string, unknown> = { url }
      if (typeof part.detail === "string" && part.detail !== "auto") image.detail = part.detail
      parts.push({ type: "image_url", image_url: image })
    } else if (type === "input_file" || type === "input_audio") {
      throw new UnsupportedResponsesField(
        `input.content.${type}`,
        `${type} content parts are not supported on this endpoint`,
      )
    }
  }
  if (parts.length === 1) {
    const only = parts[0] as { type?: string; text?: string }
    if (only.type === "text" && typeof only.text === "string") return only.text
  }
  return parts
}

function customCallArguments(input: unknown): string {
  return JSON.stringify({ input: typeof input === "string" ? input : String(input ?? "") })
}

/**
 * Responses request → Chat Completions request. Throws
 * `UnsupportedResponsesField` for the fields the proxy cannot honour.
 */
export function responsesToChatRequest(body: Record<string, unknown>): ResponsesToChatResult {
  if (typeof body.previous_response_id === "string" && body.previous_response_id) {
    throw new UnsupportedResponsesField(
      "previous_response_id",
      "previous_response_id is not supported: this proxy stores no responses (store is always false); send the full input instead",
    )
  }
  if (body.conversation !== undefined && body.conversation !== null) {
    throw new UnsupportedResponsesField(
      "conversation",
      "conversation is not supported: this proxy keeps no server-side conversation state",
    )
  }
  if (body.background === true) {
    throw new UnsupportedResponsesField(
      "background",
      "background responses are not supported: nothing is stored to poll later",
    )
  }

  const messages: unknown[] = []
  if (typeof body.instructions === "string" && body.instructions) {
    messages.push({ role: "system", content: body.instructions })
  }

  // Consecutive `function_call` items — and an assistant text immediately
  // before them — fold into one assistant message, which is the only shape
  // Chat Completions has for "the assistant said this and called these".
  let pendingAssistant: { content: string | null; tool_calls: unknown[] } | null = null
  const flush = () => {
    if (!pendingAssistant) return
    const msg: Record<string, unknown> = { role: "assistant", content: pendingAssistant.content }
    if (pendingAssistant.tool_calls.length) msg.tool_calls = pendingAssistant.tool_calls
    messages.push(msg)
    pendingAssistant = null
  }
  const pushToolCall = (call: unknown) => {
    if (!pendingAssistant) pendingAssistant = { content: null, tool_calls: [] }
    pendingAssistant.tool_calls.push(call)
  }

  const input = body.input
  const items: unknown[] =
    typeof input === "string"
      ? [{ type: "message", role: "user", content: input }]
      : Array.isArray(input)
        ? input
        : []

  for (const raw of items) {
    if (!isRecord(raw)) continue
    const type = typeof raw.type === "string" ? raw.type : raw.role !== undefined ? "message" : ""
    switch (type) {
      case "message": {
        const role = String(raw.role ?? "user")
        if (role === "assistant") {
          flush()
          pendingAssistant = { content: textOfParts(raw.content, ""), tool_calls: [] }
          break
        }
        flush()
        if (role === "system" || role === "developer") {
          messages.push({ role: "system", content: textOfParts(raw.content, "\n\n") })
        } else {
          messages.push({ role: "user", content: userContentParts(raw.content) })
        }
        break
      }
      case "function_call": {
        const name = typeof raw.name === "string" ? raw.name : ""
        const flat =
          typeof raw.namespace === "string" && raw.namespace
            ? `${raw.namespace}${NAMESPACE_SEPARATOR}${name}`
            : name
        pushToolCall({
          id: String(raw.call_id ?? raw.id ?? ""),
          type: "function",
          function: {
            name: flat,
            arguments: typeof raw.arguments === "string" ? raw.arguments : JSON.stringify(raw.arguments ?? {}),
          },
        })
        break
      }
      case "custom_tool_call": {
        pushToolCall({
          id: String(raw.call_id ?? raw.id ?? ""),
          type: "function",
          function: {
            name: typeof raw.name === "string" ? raw.name : "",
            arguments: customCallArguments(raw.input),
          },
        })
        break
      }
      case "function_call_output":
      case "custom_tool_call_output": {
        flush()
        messages.push({
          role: "tool",
          tool_call_id: String(raw.call_id ?? ""),
          content: textOfParts(raw.output, "\n"),
        })
        break
      }
      case "item_reference":
        throw new UnsupportedResponsesField(
          "input.item_reference",
          "item_reference input items need stored responses, which this proxy does not keep",
        )
      default:
        // reasoning items, hosted tool calls (web_search_call, …), unknown
        // future item types: nothing on the Chat wire can carry them.
        break
    }
  }
  flush()

  const toolNames: ResponsesToolNames = new Map()
  const tools: unknown[] = []
  let webSearchStubbed = false
  const addFunction = (
    flat: string,
    ref: ResponsesToolRef,
    def: { description?: unknown; parameters?: unknown; strict?: unknown },
  ) => {
    const fn: Record<string, unknown> = { name: flat }
    if (typeof def.description === "string") fn.description = def.description
    if (def.parameters !== undefined && def.parameters !== null) fn.parameters = def.parameters
    if (typeof def.strict === "boolean") fn.strict = def.strict
    tools.push({ type: "function", function: fn })
    toolNames.set(flat, ref)
  }
  const addCustom = (flat: string, ref: ResponsesToolRef, def: { description?: unknown }) => {
    const parameters = {
      type: "object",
      properties: {
        input: {
          type: "string",
          description: "The raw text input for this tool.",
        },
      },
      required: ["input"],
      additionalProperties: false,
    }
    addFunction(flat, ref, { description: def.description, parameters })
  }
  if (Array.isArray(body.tools)) {
    for (const tool of body.tools) {
      if (!isRecord(tool)) continue
      switch (tool.type) {
        case "function": {
          const name = typeof tool.name === "string" ? tool.name : ""
          if (!name) break
          addFunction(name, { kind: "function", name }, tool)
          break
        }
        case "custom": {
          const name = typeof tool.name === "string" ? tool.name : ""
          if (!name) break
          addCustom(name, { kind: "custom", name }, tool)
          break
        }
        case "namespace": {
          const ns = typeof tool.name === "string" ? tool.name : ""
          if (!ns || !Array.isArray(tool.tools)) break
          for (const inner of tool.tools) {
            if (!isRecord(inner)) continue
            const name = typeof inner.name === "string" ? inner.name : ""
            if (!name) continue
            const flat = `${ns}${NAMESPACE_SEPARATOR}${name}`
            if (inner.type === "custom") addCustom(flat, { kind: "custom", namespace: ns, name }, inner)
            else addFunction(flat, { kind: "function", namespace: ns, name }, inner)
          }
          break
        }
        case "web_search":
        case "web_search_preview":
        case "web_search_preview_2025_03_11":
          webSearchStubbed = true
          break
        default:
          // image_generation, file_search, code_interpreter,
          // computer_use_preview, mcp, local_shell, shell: hosted tools
          // no Chat upstream can execute — dropped.
          break
      }
    }
  }
  if (webSearchStubbed && !toolNames.has(WEB_SEARCH_STUB_TOOL.function.name)) {
    tools.push(WEB_SEARCH_STUB_TOOL)
    toolNames.set(WEB_SEARCH_STUB_TOOL.function.name, {
      kind: "function",
      name: WEB_SEARCH_STUB_TOOL.function.name,
    })
  }

  const chat: Record<string, unknown> = {
    model: body.model,
    messages,
    stream: body.stream === true,
  }
  if (tools.length) chat.tools = tools

  const choice = body.tool_choice
  if (tools.length) {
    if (choice === "auto" || choice === "none" || choice === "required") {
      chat.tool_choice = choice
    } else if (isRecord(choice) && choice.type === "function" && typeof choice.name === "string") {
      const flat =
        typeof choice.namespace === "string" && choice.namespace
          ? `${choice.namespace}${NAMESPACE_SEPARATOR}${choice.name}`
          : choice.name
      chat.tool_choice = { type: "function", function: { name: flat } }
    } else if (choice !== undefined && choice !== null) {
      // allowed_tools / hosted-tool choices: the closest Chat has is auto.
      chat.tool_choice = "auto"
    }
  }

  const text = isRecord(body.text) ? body.text : undefined
  const format = text && isRecord(text.format) ? text.format : undefined
  if (format?.type === "json_schema") {
    const schema: Record<string, unknown> = {
      name: typeof format.name === "string" && format.name ? format.name : "response",
    }
    if (format.schema !== undefined) schema.schema = format.schema
    if (typeof format.strict === "boolean") schema.strict = format.strict
    if (typeof format.description === "string") schema.description = format.description
    chat.response_format = { type: "json_schema", json_schema: schema }
  } else if (format?.type === "json_object") {
    chat.response_format = { type: "json_object" }
  }

  const reasoning = isRecord(body.reasoning) ? body.reasoning : undefined
  if (reasoning && reasoning.effort !== undefined && reasoning.effort !== null) {
    chat.reasoning_effort = reasoning.effort
  }
  if (typeof body.max_output_tokens === "number") chat.max_tokens = body.max_output_tokens
  if (typeof body.temperature === "number") chat.temperature = body.temperature
  if (typeof body.top_p === "number") chat.top_p = body.top_p
  if (typeof body.prompt_cache_key === "string" && body.prompt_cache_key) {
    chat.prompt_cache_key = body.prompt_cache_key
  }
  if (typeof body.parallel_tool_calls === "boolean") chat.parallel_tool_calls = body.parallel_tool_calls

  return { chat, toolNames }
}

// ---------------------------------------------------------------------------
// Chat → Responses output
// ---------------------------------------------------------------------------

function newId(prefix: string): string {
  return `${prefix}_${crypto.randomUUID().replace(/-/g, "")}`
}

type ChatUsage = {
  prompt_tokens?: number
  completion_tokens?: number
  prompt_tokens_details?: { cached_tokens?: number }
  completion_tokens_details?: { reasoning_tokens?: number }
}

/** Chat `usage` → Responses `usage`; detail fields only when the upstream reported them (absent means unreported, never 0). */
export function chatUsageToResponses(u: ChatUsage | undefined | null): Record<string, unknown> | undefined {
  if (!u || typeof u !== "object") return undefined
  const input = typeof u.prompt_tokens === "number" ? u.prompt_tokens : 0
  const output = typeof u.completion_tokens === "number" ? u.completion_tokens : 0
  const usage: Record<string, unknown> = {
    input_tokens: input,
    output_tokens: output,
    total_tokens: input + output,
  }
  const cached = u.prompt_tokens_details?.cached_tokens
  if (typeof cached === "number") usage.input_tokens_details = { cached_tokens: cached }
  const reasoning = u.completion_tokens_details?.reasoning_tokens
  if (typeof reasoning === "number") usage.output_tokens_details = { reasoning_tokens: reasoning }
  return usage
}

function parseCustomInput(args: string): string {
  try {
    const parsed = JSON.parse(args) as { input?: unknown }
    if (parsed && typeof parsed === "object" && typeof parsed.input === "string") return parsed.input
  } catch {
    /* not JSON — the raw arguments are the input */
  }
  return args
}

/** The completed call item for a Chat tool call, mapped back to the tool the client declared. */
function toolCallItem(
  id: string,
  callId: string,
  flatName: string,
  args: string,
  toolNames: ResponsesToolNames,
): Record<string, unknown> {
  const ref = toolNames.get(flatName)
  if (ref?.kind === "custom") {
    return {
      id,
      type: "custom_tool_call",
      call_id: callId,
      name: ref.name,
      input: parseCustomInput(args),
      status: "completed",
    }
  }
  const item: Record<string, unknown> = {
    id,
    type: "function_call",
    call_id: callId,
    name: ref?.name ?? flatName,
    arguments: args,
    status: "completed",
  }
  if (ref?.namespace) item.namespace = ref.namespace
  return item
}

export type ResponsesOutputOptions = {
  /** `response.model` — the client-facing id, same as the Chat path's chunks. */
  model: string
  toolNames: ResponsesToolNames
}

function responseStatusFromFinish(finishReason: string | null): {
  status: string
  incomplete_details?: { reason: string }
} {
  if (finishReason === "length") {
    return { status: "incomplete", incomplete_details: { reason: "max_output_tokens" } }
  }
  if (finishReason === "content_filter") {
    return { status: "incomplete", incomplete_details: { reason: "content_filter" } }
  }
  return { status: "completed" }
}

/**
 * Chat Completions SSE → Responses SSE. Streams as it goes: one Chat chunk
 * in, its Responses events out. Never buffers the turn — only the text of
 * the item currently open is kept, so its `*.done` events can carry it.
 */
export function openaiSseToResponsesStream(
  body: ReadableStream<Uint8Array>,
  opts: ResponsesOutputOptions,
): ReadableStream<Uint8Array> {
  const decoder = new TextDecoder()
  const encoder = new TextEncoder()
  let buffer = ""
  let seq = 0
  const responseId = newId("resp")
  const createdAt = Math.floor(Date.now() / 1000)
  const output: Record<string, unknown>[] = []
  let finished = false
  let finishReason: string | null = null
  let usage: ChatUsage | undefined

  type Open =
    | { kind: "reasoning"; id: string; index: number; text: string }
    | { kind: "message"; id: string; index: number; text: string }
    | { kind: "tool"; id: string; index: number; toolIndex: number }
  let open: Open | null = null
  /** Chat `tool_calls[].index` → accumulated call. `closed` once its item is done (a late delta then has nowhere to go). */
  const tools = new Map<
    number,
    { id: string; callId: string; name: string; args: string; custom: boolean; closed: boolean }
  >()

  return new ReadableStream<Uint8Array>({
    async start(controller) {
      const reader = body.getReader()
      const emit = (type: string, payload: Record<string, unknown>) => {
        controller.enqueue(
          encoder.encode(
            `event: ${type}\ndata: ${JSON.stringify({ type, sequence_number: seq++, ...payload })}\n\n`,
          ),
        )
      }
      const snapshot = (extra: Record<string, unknown>) => ({
        id: responseId,
        object: "response",
        created_at: createdAt,
        model: opts.model,
        output,
        ...extra,
      })

      const closeOpen = () => {
        if (!open) return
        const cur = open
        open = null
        if (cur.kind === "reasoning") {
          const item = { id: cur.id, type: "reasoning", summary: [{ type: "summary_text", text: cur.text }] }
          emit("response.reasoning_summary_text.done", {
            item_id: cur.id,
            output_index: cur.index,
            summary_index: 0,
            text: cur.text,
          })
          emit("response.reasoning_summary_part.done", {
            item_id: cur.id,
            output_index: cur.index,
            summary_index: 0,
            part: { type: "summary_text", text: cur.text },
          })
          output[cur.index] = item
          emit("response.output_item.done", { output_index: cur.index, item })
          return
        }
        if (cur.kind === "message") {
          const part = { type: "output_text", text: cur.text, annotations: [] }
          const item = { id: cur.id, type: "message", role: "assistant", status: "completed", content: [part] }
          emit("response.output_text.done", {
            item_id: cur.id,
            output_index: cur.index,
            content_index: 0,
            text: cur.text,
          })
          emit("response.content_part.done", {
            item_id: cur.id,
            output_index: cur.index,
            content_index: 0,
            part,
          })
          output[cur.index] = item
          emit("response.output_item.done", { output_index: cur.index, item })
          return
        }
        const tool = tools.get(cur.toolIndex)
        if (!tool) return
        tool.closed = true
        const item = toolCallItem(tool.id, tool.callId, tool.name, tool.args, opts.toolNames)
        if (tool.custom) {
          // Custom tools carry a raw string the model produced as JSON
          // fragments — only whole at the end, so the item goes out whole.
          emit("response.output_item.added", { output_index: cur.index, item: { ...item, status: "in_progress" } })
        } else {
          emit("response.function_call_arguments.done", {
            item_id: tool.id,
            output_index: cur.index,
            arguments: tool.args,
          })
        }
        output[cur.index] = item
        emit("response.output_item.done", { output_index: cur.index, item })
      }

      const openReasoning = () => {
        if (open?.kind === "reasoning") return open
        closeOpen()
        const id = newId("rs")
        const index = output.length
        output.push({ id, type: "reasoning", summary: [] })
        open = { kind: "reasoning", id, index, text: "" }
        emit("response.output_item.added", { output_index: index, item: { id, type: "reasoning", summary: [] } })
        emit("response.reasoning_summary_part.added", {
          item_id: id,
          output_index: index,
          summary_index: 0,
          part: { type: "summary_text", text: "" },
        })
        return open
      }
      const openMessage = () => {
        if (open?.kind === "message") return open
        closeOpen()
        const id = newId("msg")
        const index = output.length
        output.push({ id, type: "message", role: "assistant", status: "in_progress", content: [] })
        open = { kind: "message", id, index, text: "" }
        emit("response.output_item.added", {
          output_index: index,
          item: { id, type: "message", role: "assistant", status: "in_progress", content: [] },
        })
        emit("response.content_part.added", {
          item_id: id,
          output_index: index,
          content_index: 0,
          part: { type: "output_text", text: "", annotations: [] },
        })
        return open
      }
      const openTool = (toolIndex: number, callId: string, name: string) => {
        closeOpen()
        const id = newId(opts.toolNames.get(name)?.kind === "custom" ? "ctc" : "fc")
        const custom = opts.toolNames.get(name)?.kind === "custom"
        const index = output.length
        const entry = { id, callId, name, args: "", custom, closed: false }
        tools.set(toolIndex, entry)
        output.push({})
        open = { kind: "tool", id, index, toolIndex }
        if (!custom) {
          const ref = opts.toolNames.get(name)
          const item: Record<string, unknown> = {
            id,
            type: "function_call",
            call_id: callId,
            name: ref?.name ?? name,
            arguments: "",
            status: "in_progress",
          }
          if (ref?.namespace) item.namespace = ref.namespace
          emit("response.output_item.added", { output_index: index, item })
        }
      }

      const fail = (message: string, code: string | undefined) => {
        if (finished) return
        finished = true
        closeOpen()
        emit("response.failed", {
          response: snapshot({ status: "failed", error: { code: code ?? "upstream_error", message } }),
        })
      }
      const finish = () => {
        if (finished) return
        finished = true
        closeOpen()
        const { status, incomplete_details } = responseStatusFromFinish(finishReason)
        const extra: Record<string, unknown> = { status }
        if (incomplete_details) extra.incomplete_details = incomplete_details
        const u = chatUsageToResponses(usage)
        if (u) extra.usage = u
        emit(status === "incomplete" ? "response.incomplete" : "response.completed", {
          response: snapshot(extra),
        })
      }

      emit("response.created", { response: snapshot({ status: "in_progress" }) })
      emit("response.in_progress", { response: snapshot({ status: "in_progress" }) })

      try {
        for (;;) {
          const { done, value } = await reader.read()
          if (done) break
          buffer += decoder.decode(value, { stream: true })
          const lines = buffer.split("\n")
          buffer = lines.pop() ?? ""
          for (const line of lines) {
            if (finished) break
            if (!line.startsWith("data:")) continue
            const data = line.slice(5).trim()
            if (!data) continue
            if (data === "[DONE]") {
              finish()
              break
            }
            let json: Record<string, unknown>
            try {
              json = JSON.parse(data) as Record<string, unknown>
            } catch {
              continue
            }
            if (isRecord(json.error)) {
              const err = json.error as { message?: unknown; code?: unknown }
              fail(
                typeof err.message === "string" && err.message ? err.message : "upstream error",
                typeof err.code === "string" ? err.code : undefined,
              )
              break
            }
            if (isRecord(json.usage)) usage = json.usage as ChatUsage
            const choice = Array.isArray(json.choices) ? (json.choices[0] as Record<string, unknown>) : undefined
            if (!choice) continue
            const delta = isRecord(choice.delta) ? choice.delta : undefined
            if (delta) {
              const reasoning = delta.reasoning_content
              if (typeof reasoning === "string" && reasoning) {
                const cur = openReasoning()
                cur.text += reasoning
                emit("response.reasoning_summary_text.delta", {
                  item_id: cur.id,
                  output_index: cur.index,
                  summary_index: 0,
                  delta: reasoning,
                })
              }
              const content = delta.content
              if (typeof content === "string" && content) {
                const cur = openMessage()
                cur.text += content
                emit("response.output_text.delta", {
                  item_id: cur.id,
                  output_index: cur.index,
                  content_index: 0,
                  delta: content,
                })
              }
              if (Array.isArray(delta.tool_calls)) {
                for (const tc of delta.tool_calls) {
                  if (!isRecord(tc)) continue
                  const toolIndex = typeof tc.index === "number" ? tc.index : 0
                  const fn = isRecord(tc.function) ? tc.function : undefined
                  let entry = tools.get(toolIndex)
                  if (!entry) {
                    const name = typeof fn?.name === "string" ? fn.name : ""
                    const callId = typeof tc.id === "string" && tc.id ? tc.id : `call_${toolIndex}_${seq}`
                    openTool(toolIndex, callId, name)
                    entry = tools.get(toolIndex)!
                  }
                  const args = fn?.arguments
                  if (typeof args === "string" && args) {
                    if (entry.closed) continue
                    entry.args += args
                    if (!entry.custom && open?.kind === "tool" && open.toolIndex === toolIndex) {
                      emit("response.function_call_arguments.delta", {
                        item_id: entry.id,
                        output_index: open.index,
                        delta: args,
                      })
                    }
                  }
                }
              }
            }
            if (typeof choice.finish_reason === "string" && choice.finish_reason) {
              finishReason = choice.finish_reason
            }
          }
          if (finished) break
        }
        // Clean EOF without [DONE]: end the turn properly rather than leave
        // the client waiting for a completion event that never comes.
        finish()
        controller.close()
      } catch (e) {
        controller.error(e)
      } finally {
        try {
          reader.releaseLock()
        } catch {
          /* */
        }
      }
    },
  })
}

/** Non-stream Chat completion → one Response object (same item shapes as the stream). */
export function openaiToResponsesObject(
  json: Record<string, unknown>,
  opts: ResponsesOutputOptions,
): Record<string, unknown> {
  const output: Record<string, unknown>[] = []
  const choice = Array.isArray(json.choices) ? (json.choices[0] as Record<string, unknown>) : undefined
  const message = choice && isRecord(choice.message) ? choice.message : undefined
  if (message) {
    if (typeof message.reasoning_content === "string" && message.reasoning_content) {
      output.push({
        id: newId("rs"),
        type: "reasoning",
        summary: [{ type: "summary_text", text: message.reasoning_content }],
      })
    }
    const text =
      typeof message.content === "string"
        ? message.content
        : Array.isArray(message.content)
          ? textOfParts(message.content, "")
          : ""
    if (text) {
      output.push({
        id: newId("msg"),
        type: "message",
        role: "assistant",
        status: "completed",
        content: [{ type: "output_text", text, annotations: [] }],
      })
    }
    if (Array.isArray(message.tool_calls)) {
      for (const tc of message.tool_calls) {
        if (!isRecord(tc)) continue
        const fn = isRecord(tc.function) ? tc.function : undefined
        const name = typeof fn?.name === "string" ? fn.name : ""
        const args = typeof fn?.arguments === "string" ? fn.arguments : "{}"
        const custom = opts.toolNames.get(name)?.kind === "custom"
        output.push(
          toolCallItem(
            newId(custom ? "ctc" : "fc"),
            typeof tc.id === "string" ? tc.id : newId("call"),
            name,
            args,
            opts.toolNames,
          ),
        )
      }
    }
  }
  const finishReason = typeof choice?.finish_reason === "string" ? choice.finish_reason : null
  const { status, incomplete_details } = responseStatusFromFinish(finishReason)
  const response: Record<string, unknown> = {
    id: newId("resp"),
    object: "response",
    created_at: typeof json.created === "number" ? json.created : Math.floor(Date.now() / 1000),
    status,
    model: opts.model,
    output,
  }
  if (incomplete_details) response.incomplete_details = incomplete_details
  const usage = chatUsageToResponses(json.usage as ChatUsage | undefined)
  if (usage) response.usage = usage
  return response
}

// ---------------------------------------------------------------------------
// Native codex path helpers
// ---------------------------------------------------------------------------

/**
 * Rewrites dispatch's OpenAI-shaped in-stream error lines
 * (`data: {"error":…}` — pool exhaustion, upstream non-2xx, stall) into a
 * `response.failed` event, leaving every other byte of the relayed
 * Responses SSE untouched. Line-bounded: holds one partial line at most.
 */
export function rewriteOpenAIErrorFramesToResponses(
  body: ReadableStream<Uint8Array>,
  model: string,
): ReadableStream<Uint8Array> {
  const decoder = new TextDecoder()
  const encoder = new TextEncoder()
  let carry = ""
  const rewrite = (line: string): string => {
    if (!line.startsWith("data: {\"error\"") && !line.startsWith("data:{\"error\"")) return line
    try {
      const json = JSON.parse(line.slice(5).trim()) as { error?: { message?: unknown; code?: unknown; type?: unknown } }
      if (!isRecord(json.error)) return line
      const err = json.error as { message?: unknown; code?: unknown }
      const payload = {
        type: "response.failed",
        response: {
          id: newId("resp"),
          object: "response",
          created_at: Math.floor(Date.now() / 1000),
          status: "failed",
          model,
          output: [],
          error: {
            code: typeof err.code === "string" ? err.code : "upstream_error",
            message: typeof err.message === "string" ? err.message : "upstream error",
          },
        },
      }
      return `event: response.failed\ndata: ${JSON.stringify(payload)}`
    } catch {
      return line
    }
  }
  return body.pipeThrough(
    new TransformStream<Uint8Array, Uint8Array>({
      transform(chunk, controller) {
        carry += decoder.decode(chunk, { stream: true })
        const lines = carry.split("\n")
        carry = lines.pop() ?? ""
        if (lines.length) controller.enqueue(encoder.encode(lines.map(rewrite).join("\n") + "\n"))
      },
      flush(controller) {
        carry += decoder.decode()
        if (carry) controller.enqueue(encoder.encode(rewrite(carry)))
      },
    }),
  )
}

export type CollectedResponses =
  | { response: Record<string, unknown> }
  | { error: { message: string; type: "upstream_error" } }

/** Non-stream native path: drain a Responses SSE and return its terminal `response` object, or the failure. */
export async function collectResponsesSse(body: ReadableStream<Uint8Array>): Promise<CollectedResponses> {
  const decoder = new TextDecoder()
  const reader = body.getReader()
  let buffer = ""
  let response: Record<string, unknown> | null = null
  let error: string | null = null
  for (;;) {
    const { done, value } = await reader.read()
    if (done) break
    buffer += decoder.decode(value, { stream: true })
    const lines = buffer.split("\n")
    buffer = lines.pop() ?? ""
    for (const line of lines) {
      if (!line.startsWith("data:")) continue
      const data = line.slice(5).trim()
      if (!data || data === "[DONE]" || error) continue
      try {
        const ev = JSON.parse(data) as {
          type?: string
          response?: Record<string, unknown> & { error?: { message?: string } }
          error?: { message?: string }
          message?: string
        }
        if (ev.type === "response.failed" || ev.type === "error") {
          error = ev.response?.error?.message || ev.error?.message || ev.message || "codex upstream failure"
        } else if (
          (ev.type === "response.completed" || ev.type === "response.incomplete" || ev.type === "response.done") &&
          isRecord(ev.response)
        ) {
          response = ev.response
        }
      } catch {
        /* */
      }
    }
  }
  if (error) return { error: { message: error, type: "upstream_error" } }
  if (!response) {
    return { error: { message: "upstream ended without response.completed", type: "upstream_error" } }
  }
  return { response }
}
