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
} {
  const cleaned = stripCacheControl(body) as Record<string, unknown>
  const messages: unknown[] = []

  const system = cleaned.system
  if (typeof system === "string" && system) {
    messages.push({ role: "system", content: system })
  } else if (Array.isArray(system)) {
    const text = system
      .map((b) => {
        if (b && typeof b === "object" && (b as { type?: string }).type === "text") {
          return String((b as { text?: string }).text ?? "")
        }
        return ""
      })
      .join("")
    if (text) messages.push({ role: "system", content: text })
  }

  const inMessages = (cleaned.messages as Array<Record<string, unknown>>) ?? []
  for (const m of inMessages) {
    const role = String(m.role ?? "")
    if (role === "assistant") {
      const blocks = Array.isArray(m.content) ? (m.content as Array<Record<string, unknown>>) : null
      if (blocks) {
        let text = ""
        const tool_calls: Array<Record<string, unknown>> = []
        for (const b of blocks) {
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
        const msg: Record<string, unknown> = {
          role: "assistant",
          content: text || null,
        }
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
            messages.push({
              role: "tool",
              tool_call_id: b.tool_use_id,
              content: contentToText(b.content),
            })
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
  } = {
    messages,
    stream: !!cleaned.stream,
  }
  if (typeof cleaned.max_tokens === "number") out.max_tokens = cleaned.max_tokens

  if (Array.isArray(cleaned.tools)) {
    out.tools = (cleaned.tools as Array<Record<string, unknown>>).map((t) => {
      if (t && typeof t === "object" && t.name && !t.function) {
        return {
          type: "function",
          function: {
            name: t.name,
            description: t.description,
            parameters: t.input_schema ?? { type: "object", properties: {} },
          },
        }
      }
      return t
    })
  }
  if (cleaned.tool_choice) {
    out.tool_choice = mapAnthropicToolChoice(cleaned.tool_choice)
  }
  if (cleaned.output_format && typeof cleaned.output_format === "object") {
    const of = cleaned.output_format as { type?: string; schema?: unknown }
    if (of.type === "json_schema" && of.schema) {
      out.response_format = {
        type: "json_schema",
        json_schema: { schema: of.schema },
      }
    }
  }
  // optional extension if clients send reasoning_effort on Anthropic body
  if (cleaned.reasoning_effort != null) out.reasoning_effort = cleaned.reasoning_effort

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
    | { prompt_tokens?: number; completion_tokens?: number }
    | undefined

  return {
    id: String(completion.id ?? `msg_${Date.now()}`),
    type: "message",
    role: "assistant",
    model,
    content: content.length ? content : [{ type: "text", text: "" }],
    stop_reason,
    stop_sequence: null,
    usage: usage
      ? {
          input_tokens: usage.prompt_tokens ?? 0,
          output_tokens: usage.completion_tokens ?? 0,
        }
      : { input_tokens: 0, output_tokens: 0 },
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
  let nextBlockIndex = 0
  let outputTokens = 0
  let stopped = false
  let sawToolCall = false
  /** OpenAI tool_calls[].index → Anthropic content block index + metadata */
  const toolBlocks = new Map<
    number,
    { blockIndex: number; id: string; name: string; started: boolean }
  >()

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
      const ensureTextBlock = () => {
        if (textBlockOpen) return
        ensureStart()
        closeOpenToolBlocks()
        textBlockIndex = nextBlockIndex++
        textBlockOpen = true
        emitEvent("content_block_start", {
          type: "content_block_start",
          index: textBlockIndex,
          content_block: { type: "text", text: "" },
        })
      }
      const closeOpenToolBlocks = () => {
        // Close in ascending content-block index order for stable clients.
        const open = [...toolBlocks.values()]
          .filter((t) => t.started)
          .sort((a, b) => a.blockIndex - b.blockIndex)
        for (const t of open) {
          emitEvent("content_block_stop", {
            type: "content_block_stop",
            index: t.blockIndex,
          })
          t.started = false
        }
      }
      const ensureToolBlock = (
        openaiIndex: number,
        id: string | undefined,
        name: string | undefined,
      ): { blockIndex: number; id: string; name: string; started: boolean } => {
        let entry = toolBlocks.get(openaiIndex)
        if (!entry) {
          entry = {
            blockIndex: -1,
            id: id || `toolu_${openaiIndex}_${msgId.slice(4, 12)}`,
            name: name || "unknown",
            started: false,
          }
          toolBlocks.set(openaiIndex, entry)
        } else {
          if (id) entry.id = id
          if (name) entry.name = name
        }
        if (!entry.started) {
          ensureStart()
          closeTextBlock()
          // If another tool was open at a different openai index, leave it open
          // (parallel tool calls are separate content blocks; stop on finish).
          entry.blockIndex = nextBlockIndex++
          entry.started = true
          sawToolCall = true
          emitEvent("content_block_start", {
            type: "content_block_start",
            index: entry.blockIndex,
            content_block: {
              type: "tool_use",
              id: entry.id,
              name: entry.name,
              input: {},
            },
          })
        }
        return entry
      }
      const finish = (stop_reason: string) => {
        if (stopped) return
        stopped = true
        closeTextBlock()
        // Close any tool blocks still open
        for (const t of [...toolBlocks.values()].sort(
          (a, b) => a.blockIndex - b.blockIndex,
        )) {
          if (t.started) {
            emitEvent("content_block_stop", {
              type: "content_block_stop",
              index: t.blockIndex,
            })
            t.started = false
          }
        }
        ensureStart()
        // Prefer tool_use if we streamed any tools even when finish_reason was missing.
        const reason =
          stop_reason === "end_turn" && sawToolCall ? "tool_use" : stop_reason
        emitEvent("message_delta", {
          type: "message_delta",
          delta: { stop_reason: reason, stop_sequence: null },
          usage: { output_tokens: outputTokens },
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
              finish(sawToolCall ? "tool_use" : "end_turn")
              continue
            }
            try {
              const json = JSON.parse(data) as Record<string, unknown>
              const choice = (json.choices as Array<Record<string, unknown>> | undefined)?.[0]
              if (!choice) continue
              const delta = choice.delta as Record<string, unknown> | undefined
              const content = delta?.content
              if (typeof content === "string" && content) {
                ensureTextBlock()
                outputTokens += Math.max(1, Math.ceil(content.length / 4))
                emitEvent("content_block_delta", {
                  type: "content_block_delta",
                  index: textBlockIndex,
                  delta: { type: "text_delta", text: content },
                })
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
                  // Start block when we have id/name, or when arguments arrive
                  // without a prior start (some providers only send args chunks).
                  const hasStartInfo = !!(id || name)
                  const args =
                    typeof fn?.arguments === "string" ? fn.arguments : undefined
                  if (hasStartInfo || args != null) {
                    const entry = ensureToolBlock(openaiIndex, id, name)
                    if (args) {
                      outputTokens += Math.max(1, Math.ceil(args.length / 4))
                      emitEvent("content_block_delta", {
                        type: "content_block_delta",
                        index: entry.blockIndex,
                        delta: {
                          type: "input_json_delta",
                          partial_json: args,
                        },
                      })
                    }
                  }
                }
              }

              const fr = choice.finish_reason
              if (fr) {
                let stop_reason = "end_turn"
                if (fr === "tool_calls") stop_reason = "tool_use"
                else if (fr === "length") stop_reason = "max_tokens"
                else if (fr === "content_filter") stop_reason = "end_turn"
                finish(stop_reason)
              }
            } catch {
              /* ignore parse */
            }
          }
        }
        if (started && !stopped) finish(sawToolCall ? "tool_use" : "end_turn")
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
                if (event === "content_block_start") {
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
                } else if (event === "message_stop") {
                  emit({
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
                  })
                  controller.enqueue(encoder.encode("data: [DONE]\n\n"))
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
