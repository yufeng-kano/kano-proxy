/**
 * OpenAI Chat Completions ↔ Anthropic Messages conversion.
 * OpenAI→Claude: never invent cache_control.
 */

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

/** Best-effort Anthropic SSE → OpenAI SSE (text deltas). */
export function anthropicSseToOpenAIStream(
  body: ReadableStream<Uint8Array>,
  model: string,
): ReadableStream<Uint8Array> {
  const decoder = new TextDecoder()
  const encoder = new TextEncoder()
  let buffer = ""
  const id = `chatcmpl_${crypto.randomUUID().replace(/-/g, "").slice(0, 24)}`
  let sentRole = false
  // SSE event name must survive chunk boundaries: the `event:` line and its
  // `data:` line routinely arrive in different reads.
  let event = ""

  return new ReadableStream({
    async start(controller) {
      const reader = body.getReader()
      const emit = (obj: unknown) => {
        controller.enqueue(encoder.encode(`data: ${JSON.stringify(obj)}\n\n`))
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
                if (event === "content_block_delta") {
                  const delta = json.delta as { type?: string; text?: string } | undefined
                  if (delta?.type === "text_delta" && delta.text) {
                    if (!sentRole) {
                      emit({
                        id,
                        object: "chat.completion.chunk",
                        created: Math.floor(Date.now() / 1000),
                        model,
                        choices: [{ index: 0, delta: { role: "assistant", content: "" }, finish_reason: null }],
                      })
                      sentRole = true
                    }
                    emit({
                      id,
                      object: "chat.completion.chunk",
                      created: Math.floor(Date.now() / 1000),
                      model,
                      choices: [{ index: 0, delta: { content: delta.text }, finish_reason: null }],
                    })
                  }
                } else if (event === "message_stop") {
                  emit({
                    id,
                    object: "chat.completion.chunk",
                    created: Math.floor(Date.now() / 1000),
                    model,
                    choices: [{ index: 0, delta: {}, finish_reason: "stop" }],
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
