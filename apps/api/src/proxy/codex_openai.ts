/** Codex Responses SSE → OpenAI Chat Completions. */

export function codexSseToOpenAIStream(
  body: ReadableStream<Uint8Array>,
  model: string,
): ReadableStream<Uint8Array> {
  const decoder = new TextDecoder()
  const encoder = new TextEncoder()
  let buffer = ""
  const id = `chatcmpl_${crypto.randomUUID().replace(/-/g, "").slice(0, 24)}`
  let sentRole = false

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
          const chunks = buffer.split("\n")
          buffer = chunks.pop() ?? ""
          for (const line of chunks) {
            if (!line.startsWith("data:")) continue
            const data = line.slice(5).trim()
            if (!data || data === "[DONE]") continue
            try {
              const ev = JSON.parse(data) as {
                type?: string
                delta?: string
                item?: Record<string, unknown>
              }
              if (
                ev.type === "response.output_text.delta" ||
                ev.type === "response.reasoning_summary_text.delta"
              ) {
                const text = ev.delta ?? ""
                if (!text) continue
                if (!sentRole) {
                  emit({
                    id,
                    object: "chat.completion.chunk",
                    created: Math.floor(Date.now() / 1000),
                    model,
                    choices: [
                      { index: 0, delta: { role: "assistant", content: "" }, finish_reason: null },
                    ],
                  })
                  sentRole = true
                }
                if (ev.type === "response.output_text.delta") {
                  emit({
                    id,
                    object: "chat.completion.chunk",
                    created: Math.floor(Date.now() / 1000),
                    model,
                    choices: [{ index: 0, delta: { content: text }, finish_reason: null }],
                  })
                }
              } else if (ev.type === "response.completed" || ev.type === "response.done") {
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
              /* */
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

export async function collectCodexSse(
  body: ReadableStream<Uint8Array>,
  model: string,
): Promise<Record<string, unknown>> {
  const decoder = new TextDecoder()
  const reader = body.getReader()
  let buffer = ""
  let text = ""
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
      try {
        const ev = JSON.parse(data) as {
          type?: string
          delta?: string
          item?: { type?: string; call_id?: string; name?: string; arguments?: string }
        }
        if (ev.type === "response.output_text.delta" && ev.delta) text += ev.delta
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
      } catch {
        /* */
      }
    }
  }
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
          tool_calls: tool_calls.length ? tool_calls : undefined,
        },
        finish_reason: tool_calls.length ? "tool_calls" : "stop",
      },
    ],
  }
}
