import { describe, expect, it } from "vitest"
import { codexSseToOpenAIStream, collectCodexSse } from "../src/proxy/codex_openai"
import { openaiSseToAnthropicStream } from "../src/proxy/openai_anthropic"

function chunked(text: string, size: number): ReadableStream<Uint8Array> {
  const bytes = new TextEncoder().encode(text)
  return new ReadableStream({
    start(c) {
      for (let i = 0; i < bytes.length; i += size) c.enqueue(bytes.subarray(i, i + size))
      c.close()
    },
  })
}

async function collect(stream: ReadableStream<Uint8Array>): Promise<string> {
  const reader = stream.getReader()
  let out = ""
  for (;;) {
    const { done, value } = await reader.read()
    if (done) break
    out += new TextDecoder().decode(value)
  }
  return out
}

/** Reassemble OpenAI chunk stream: text, reasoning, tool_calls by index, finish, usage. */
function parseChunks(sse: string): {
  text: string
  reasoning: string
  tools: Array<{ index: number; id?: string; name?: string; args: string }>
  finish: string | null
  usage: Record<string, number> | null
  doneCount: number
} {
  const tools = new Map<number, { index: number; id?: string; name?: string; args: string }>()
  let text = ""
  let reasoning = ""
  let finish: string | null = null
  let usage: Record<string, number> | null = null
  let doneCount = 0
  let roleCount = 0
  let chunkCount = 0
  for (const line of sse.split("\n")) {
    if (!line.startsWith("data:")) continue
    const data = line.slice(5).trim()
    if (!data) continue
    if (data === "[DONE]") {
      doneCount++
      continue
    }
    const j = JSON.parse(data)
    chunkCount++
    if (j.usage) usage = j.usage
    const choice = j.choices?.[0]
    if (!choice) continue
    if (choice.delta?.role) {
      roleCount++
      expect(chunkCount, "role chunk must come first").toBe(1)
    }
    if (choice.finish_reason) {
      expect(finish, "finish_reason emitted twice").toBe(null)
      finish = choice.finish_reason
    }
    if (typeof choice.delta?.content === "string") text += choice.delta.content
    if (typeof choice.delta?.reasoning_content === "string") {
      reasoning += choice.delta.reasoning_content
    }
    for (const tc of choice.delta?.tool_calls ?? []) {
      let entry = tools.get(tc.index)
      if (!entry) {
        entry = { index: tc.index, args: "" }
        tools.set(tc.index, entry)
      }
      if (tc.id) entry.id = tc.id
      if (tc.function?.name) entry.name = tc.function.name
      if (typeof tc.function?.arguments === "string") entry.args += tc.function.arguments
    }
  }
  expect(roleCount, "assistant role chunk must appear exactly once").toBe(1)
  return {
    text,
    reasoning,
    tools: [...tools.values()].sort((a, b) => a.index - b.index),
    finish,
    usage,
    doneCount,
  }
}

const d = (obj: unknown) => `data: ${JSON.stringify(obj)}`

const TOOL_CALL_EVENTS = [
  d({
    type: "response.output_item.added",
    output_index: 0,
    item: { type: "function_call", id: "fc_1", call_id: "call_1", name: "Read", arguments: "" },
  }),
  "",
  d({
    type: "response.function_call_arguments.delta",
    item_id: "fc_1",
    output_index: 0,
    delta: '{"file_path":',
  }),
  "",
  d({
    type: "response.function_call_arguments.delta",
    item_id: "fc_1",
    output_index: 0,
    delta: '"/repo/package.json"}',
  }),
  "",
  d({
    type: "response.function_call_arguments.done",
    item_id: "fc_1",
    arguments: '{"file_path":"/repo/package.json"}',
  }),
  "",
  d({
    type: "response.output_item.done",
    output_index: 0,
    item: {
      type: "function_call",
      id: "fc_1",
      call_id: "call_1",
      name: "Read",
      arguments: '{"file_path":"/repo/package.json"}',
    },
  }),
  "",
  d({
    type: "response.completed",
    response: { usage: { input_tokens: 120, output_tokens: 18 } },
  }),
  "",
]

const TOOL_CALL_SSE = TOOL_CALL_EVENTS.join("\n")

/** Same tool round, preceded by a reasoning summary that streams first. */
const TOOL_CALL_WITH_REASONING_SSE = [
  d({ type: "response.reasoning_summary_text.delta", delta: "Let me check " }),
  "",
  d({ type: "response.reasoning_summary_text.delta", delta: "the file first." }),
  "",
  ...TOOL_CALL_EVENTS,
].join("\n")

describe("codexSseToOpenAIStream", () => {
  it("streams text and finishes with stop", async () => {
    const sse = [
      d({ type: "response.output_text.delta", delta: "hel" }),
      "",
      d({ type: "response.output_text.delta", delta: "lo" }),
      "",
      d({ type: "response.completed", response: {} }),
      "",
    ].join("\n")
    const out = parseChunks(await collect(codexSseToOpenAIStream(chunked(sse, 9), "gpt-5.2")))
    expect(out.text).toBe("hello")
    expect(out.tools).toEqual([])
    expect(out.finish).toBe("stop")
    expect(out.doneCount).toBe(1)
  })

  it("maps input_tokens_details.cached_tokens onto prompt_tokens_details.cached_tokens", async () => {
    const sse = [
      d({ type: "response.output_text.delta", delta: "hi" }),
      "",
      d({
        type: "response.completed",
        response: {
          usage: { input_tokens: 500, output_tokens: 20, input_tokens_details: { cached_tokens: 200 } },
        },
      }),
      "",
    ].join("\n")
    const out = parseChunks(await collect(codexSseToOpenAIStream(chunked(sse, 15), "gpt-5.2")))
    expect(out.usage).toEqual({
      prompt_tokens: 500,
      completion_tokens: 20,
      prompt_tokens_details: { cached_tokens: 200 },
    })
  })

  it("omits prompt_tokens_details when the Responses API reports no cache detail", async () => {
    const sse = [
      d({ type: "response.output_text.delta", delta: "hi" }),
      "",
      d({ type: "response.completed", response: { usage: { input_tokens: 5, output_tokens: 1 } } }),
      "",
    ].join("\n")
    const out = parseChunks(await collect(codexSseToOpenAIStream(chunked(sse, 15), "gpt-5.2")))
    expect(out.usage).toEqual({ prompt_tokens: 5, completion_tokens: 1 })
  })

  it("maps a streamed function call to tool_calls chunks", async () => {
    const out = parseChunks(
      await collect(codexSseToOpenAIStream(chunked(TOOL_CALL_SSE, 13), "gpt-5.2")),
    )
    expect(out.tools).toHaveLength(1)
    expect(out.tools[0]!.id).toBe("call_1")
    expect(out.tools[0]!.name).toBe("Read")
    // Deltas streamed once; arguments.done and item.done must not re-append.
    expect(JSON.parse(out.tools[0]!.args)).toEqual({ file_path: "/repo/package.json" })
    expect(out.finish).toBe("tool_calls")
    expect(out.usage).toEqual({ prompt_tokens: 120, completion_tokens: 18 })
    expect(out.doneCount).toBe(1)
  })

  it("falls back to the done item when no argument deltas were sent", async () => {
    const sse = [
      d({
        type: "response.output_item.added",
        item: { type: "function_call", id: "fc_1", call_id: "call_1", name: "Bash", arguments: "" },
      }),
      "",
      d({
        type: "response.output_item.done",
        item: {
          type: "function_call",
          id: "fc_1",
          call_id: "call_1",
          name: "Bash",
          arguments: '{"command":"ls"}',
        },
      }),
      "",
      d({ type: "response.completed", response: {} }),
      "",
    ].join("\n")
    const out = parseChunks(await collect(codexSseToOpenAIStream(chunked(sse, 17), "m")))
    expect(out.tools).toHaveLength(1)
    expect(JSON.parse(out.tools[0]!.args)).toEqual({ command: "ls" })
    expect(out.finish).toBe("tool_calls")
  })

  it("recovers a call whose added event was missed", async () => {
    const sse = [
      d({ type: "response.function_call_arguments.delta", item_id: "fc_9", delta: '{"a":' }),
      "",
      d({ type: "response.function_call_arguments.delta", item_id: "fc_9", delta: "1}" }),
      "",
      d({
        type: "response.output_item.done",
        item: { type: "function_call", id: "fc_9", call_id: "call_9", name: "T", arguments: '{"a":1}' },
      }),
      "",
      d({ type: "response.completed", response: {} }),
      "",
    ].join("\n")
    const out = parseChunks(await collect(codexSseToOpenAIStream(chunked(sse, 11), "m")))
    expect(out.tools).toHaveLength(1)
    expect(out.tools[0]!.id).toBe("call_9")
    expect(out.tools[0]!.name).toBe("T")
    // Stashed deltas flush once; the done item must not append a second copy.
    expect(JSON.parse(out.tools[0]!.args)).toEqual({ a: 1 })
  })

  it("keeps two calls on distinct tool indices", async () => {
    const sse = [
      d({
        type: "response.output_item.added",
        item: { type: "function_call", id: "fc_1", call_id: "call_a", name: "A", arguments: "" },
      }),
      "",
      d({ type: "response.function_call_arguments.delta", item_id: "fc_1", delta: '{"a":1}' }),
      "",
      d({
        type: "response.output_item.added",
        item: { type: "function_call", id: "fc_2", call_id: "call_b", name: "B", arguments: "" },
      }),
      "",
      d({ type: "response.function_call_arguments.delta", item_id: "fc_2", delta: '{"b":2}' }),
      "",
      d({ type: "response.completed", response: {} }),
      "",
    ].join("\n")
    const out = parseChunks(await collect(codexSseToOpenAIStream(chunked(sse, 19), "m")))
    expect(out.tools.map((t) => [t.index, t.id, t.name])).toEqual([
      [0, "call_a", "A"],
      [1, "call_b", "B"],
    ])
    expect(out.tools.map((t) => JSON.parse(t.args))).toEqual([{ a: 1 }, { b: 2 }])
  })

  it("ignores events that arrive after response.completed", async () => {
    const sse = [
      d({ type: "response.output_text.delta", delta: "done" }),
      "",
      d({ type: "response.completed", response: {} }),
      "",
      d({ type: "response.output_text.delta", delta: "late" }),
      "",
      d({ type: "response.completed", response: {} }),
      "",
    ].join("\n")
    const out = parseChunks(await collect(codexSseToOpenAIStream(chunked(sse, 21), "m")))
    expect(out.text).toBe("done")
    expect(out.finish).toBe("stop")
    expect(out.doneCount).toBe(1)
  })

  it("uses stashed deltas when the done item carries no arguments", async () => {
    // pendingArgs is the only source here: `added` was missed and the done
    // item's arguments are empty.
    const sse = [
      d({ type: "response.function_call_arguments.delta", item_id: "fc_9", delta: '{"a":' }),
      "",
      d({ type: "response.function_call_arguments.delta", item_id: "fc_9", delta: "1}" }),
      "",
      d({
        type: "response.output_item.done",
        item: { type: "function_call", id: "fc_9", call_id: "call_9", name: "T", arguments: "" },
      }),
      "",
      d({ type: "response.completed", response: {} }),
      "",
    ].join("\n")
    const out = parseChunks(await collect(codexSseToOpenAIStream(chunked(sse, 11), "m")))
    expect(out.tools).toHaveLength(1)
    expect(JSON.parse(out.tools[0]!.args)).toEqual({ a: 1 })
  })

  it("prefers the done item's full arguments over a partial stash", async () => {
    // `added` missed and one delta lost: the stash holds broken JSON, the done
    // item holds the backend's complete copy.
    const sse = [
      d({ type: "response.function_call_arguments.delta", item_id: "fc_9", delta: '{"a":' }),
      "",
      d({
        type: "response.output_item.done",
        item: { type: "function_call", id: "fc_9", call_id: "call_9", name: "T", arguments: '{"a":1}' },
      }),
      "",
      d({ type: "response.completed", response: {} }),
      "",
    ].join("\n")
    const out = parseChunks(await collect(codexSseToOpenAIStream(chunked(sse, 15), "m")))
    expect(out.tools).toHaveLength(1)
    expect(JSON.parse(out.tools[0]!.args)).toEqual({ a: 1 })
  })

  it("terminates the stream when upstream ends without response.completed", async () => {
    const sse = [d({ type: "response.output_text.delta", delta: "hi" }), ""].join("\n")
    const out = parseChunks(await collect(codexSseToOpenAIStream(chunked(sse, 7), "m")))
    expect(out.text).toBe("hi")
    expect(out.finish).toBe("stop")
    expect(out.doneCount).toBe(1)
  })

  it("streams reasoning_summary_text.delta as delta.reasoning_content, separate from content", async () => {
    const sse = [
      d({ type: "response.reasoning_summary_text.delta", delta: "thinking " }),
      "",
      d({ type: "response.reasoning_summary_text.delta", delta: "hard" }),
      "",
      d({ type: "response.output_text.delta", delta: "answer" }),
      "",
      d({ type: "response.completed", response: {} }),
      "",
    ].join("\n")
    const out = parseChunks(await collect(codexSseToOpenAIStream(chunked(sse, 10), "gpt-5.2")))
    expect(out.reasoning).toBe("thinking hard")
    expect(out.text).toBe("answer")
    expect(out.finish).toBe("stop")
  })

  it("composes with the Anthropic converter for a full codex tool round", async () => {
    // /anthropic + codex/*: Responses SSE → chat chunks → Anthropic events.
    const anthropic = await collect(
      openaiSseToAnthropicStream(
        codexSseToOpenAIStream(chunked(TOOL_CALL_SSE, 13), "gpt-5.2"),
        "codex/gpt-5.2",
      ),
    )
    expect(anthropic).toContain('"type":"tool_use"')
    expect(anthropic).toContain('"id":"call_1"')
    expect(anthropic).toContain('"name":"Read"')
    expect(anthropic).toContain('"stop_reason":"tool_use"')
    expect(anthropic).toContain('"input_tokens":120')
    expect(anthropic).toContain('"output_tokens":18')
    // Reassemble the streamed input JSON across content_block_delta events.
    const args = [...anthropic.matchAll(/"partial_json":"((?:[^"\\]|\\.)*)"/g)]
      .map((m) => JSON.parse(`"${m[1]}"`))
      .join("")
    expect(JSON.parse(args)).toEqual({ file_path: "/repo/package.json" })
  })

  it("composes with the Anthropic converter when reasoning deltas precede the tool round", async () => {
    // Same round as above, but the upstream also streams a reasoning summary
    // first. reasoning_content has no Anthropic equivalent: the OpenAI SSE →
    // Anthropic SSE converter only reads `content` / `tool_calls`, so this
    // must pass through harmlessly — no extra block, no protocol break, and
    // the rest of the turn (tool_use, stop_reason, usage) unaffected.
    const anthropic = await collect(
      openaiSseToAnthropicStream(
        codexSseToOpenAIStream(chunked(TOOL_CALL_WITH_REASONING_SSE, 17), "gpt-5.2"),
        "codex/gpt-5.2",
      ),
    )
    expect(anthropic).not.toContain("reasoning_content")
    expect(anthropic).not.toContain("Let me check")
    expect(anthropic).toContain('"type":"tool_use"')
    expect(anthropic).toContain('"id":"call_1"')
    expect(anthropic).toContain('"name":"Read"')
    expect(anthropic).toContain('"stop_reason":"tool_use"')
    expect(anthropic).toContain('"input_tokens":120')
    expect(anthropic).toContain('"output_tokens":18')
    const args = [...anthropic.matchAll(/"partial_json":"((?:[^"\\]|\\.)*)"/g)]
      .map((m) => JSON.parse(`"${m[1]}"`))
      .join("")
    expect(JSON.parse(args)).toEqual({ file_path: "/repo/package.json" })
  })

  it("emits a single OpenAI-shaped error line on response.failed — no finish chunk, no [DONE]", async () => {
    const sse = [
      d({ type: "response.output_text.delta", delta: "partial" }),
      "",
      d({
        type: "response.failed",
        response: { error: { message: "rate limited, try again" } },
      }),
      "",
    ].join("\n")
    const out = await collect(codexSseToOpenAIStream(chunked(sse, 11), "gpt-5.2"))
    expect(out).toContain(
      'data: {"error":{"message":"rate limited, try again","type":"upstream_error"}}',
    )
    expect(out).not.toContain("[DONE]")
    expect(out).not.toContain('"finish_reason":"stop"')
    expect(out).not.toContain('"finish_reason":"tool_calls"')
  })

  it("stops processing further events once response.failed lands", async () => {
    const sse = [
      d({ type: "response.failed", error: { message: "boom" } }),
      "",
      d({ type: "response.output_text.delta", delta: "should not appear" }),
      "",
      d({ type: "response.completed", response: {} }),
      "",
    ].join("\n")
    const out = await collect(codexSseToOpenAIStream(chunked(sse, 9), "m"))
    expect(out).not.toContain("should not appear")
    expect(out).not.toContain("[DONE]")
    const errorLines = out.split("\n\n").filter((l) => l.includes('"error"'))
    expect(errorLines).toHaveLength(1)
  })

  it("composes with the Anthropic converter: response.failed becomes an Anthropic event: error, nothing after", async () => {
    const sse = [
      d({ type: "response.output_text.delta", delta: "hi" }),
      "",
      d({ type: "response.failed", error: { message: "overloaded" } }),
      "",
    ].join("\n")
    const anthropic = await collect(
      openaiSseToAnthropicStream(
        codexSseToOpenAIStream(chunked(sse, 9), "gpt-5.2"),
        "codex/gpt-5.2",
      ),
    )
    expect(anthropic).toContain("event: error")
    expect(anthropic).toContain(
      'data: {"type":"error","error":{"type":"api_error","message":"overloaded"}}',
    )
    expect(anthropic).not.toContain("event: message_delta")
    expect(anthropic).not.toContain("event: message_stop")
  })
})

describe("collectCodexSse", () => {
  /** These fixtures never emit response.failed/error — always the success shape. */
  async function collectOk(
    stream: ReadableStream<Uint8Array>,
    model: string,
  ): Promise<Record<string, unknown>> {
    const result = await collectCodexSse(stream, model)
    if ("error" in result) throw new Error(`unexpected error result: ${JSON.stringify(result)}`)
    return result
  }

  it("captures usage from response.completed", async () => {
    const completion = await collectOk(chunked(TOOL_CALL_SSE, 23), "gpt-5.2")
    expect(completion.usage).toEqual({ prompt_tokens: 120, completion_tokens: 18 })
    const choice = (completion.choices as Array<Record<string, unknown>>)[0]!
    expect(choice.finish_reason).toBe("tool_calls")
  })

  it("maps input_tokens_details.cached_tokens onto prompt_tokens_details.cached_tokens", async () => {
    const sse = [
      d({ type: "response.output_text.delta", delta: "hi" }),
      "",
      d({
        type: "response.completed",
        response: {
          usage: { input_tokens: 500, output_tokens: 20, input_tokens_details: { cached_tokens: 200 } },
        },
      }),
      "",
    ].join("\n")
    const completion = await collectOk(chunked(sse, 27), "gpt-5.2")
    expect(completion.usage).toEqual({
      prompt_tokens: 500,
      completion_tokens: 20,
      prompt_tokens_details: { cached_tokens: 200 },
    })
  })

  it("omits prompt_tokens_details when the Responses API reports no cache detail", async () => {
    const sse = [
      d({ type: "response.output_text.delta", delta: "hi" }),
      "",
      d({ type: "response.completed", response: { usage: { input_tokens: 5, output_tokens: 1 } } }),
      "",
    ].join("\n")
    const completion = await collectOk(chunked(sse, 27), "gpt-5.2")
    expect(completion.usage).toEqual({ prompt_tokens: 5, completion_tokens: 1 })
  })

  it("omits message.reasoning_content when no reasoning summary streamed", async () => {
    const completion = await collectOk(chunked(TOOL_CALL_SSE, 23), "gpt-5.2")
    const choice = (completion.choices as Array<Record<string, unknown>>)[0]!
    const message = choice.message as Record<string, unknown>
    expect(message.reasoning_content).toBeUndefined()
  })

  it("sets message.reasoning_content from the accumulated reasoning summary", async () => {
    const completion = await collectOk(chunked(TOOL_CALL_WITH_REASONING_SSE, 31), "gpt-5.2")
    const choice = (completion.choices as Array<Record<string, unknown>>)[0]!
    const message = choice.message as Record<string, unknown>
    expect(message.reasoning_content).toBe("Let me check the file first.")
    // Tool round itself is unaffected by the leading reasoning summary.
    expect(message.content).toBe(null)
    expect(choice.finish_reason).toBe("tool_calls")
  })

  it("returns an error marker instead of a fake completion on response.failed", async () => {
    const sse = [
      d({
        type: "response.failed",
        response: { error: { message: "rate limited" } },
      }),
      "",
    ].join("\n")
    const result = await collectCodexSse(chunked(sse, 9), "gpt-5.2")
    expect(result).toEqual({ error: { message: "rate limited", type: "upstream_error" } })
  })

  it("falls back through error.message, message, then a default", async () => {
    const viaError = await collectCodexSse(
      chunked(d({ type: "error", error: { message: "bad request" } }) + "\n", 7),
      "m",
    )
    expect(viaError).toEqual({ error: { message: "bad request", type: "upstream_error" } })

    const viaMessage = await collectCodexSse(
      chunked(d({ type: "error", message: "top-level message" }) + "\n", 7),
      "m",
    )
    expect(viaMessage).toEqual({
      error: { message: "top-level message", type: "upstream_error" },
    })

    const viaDefault = await collectCodexSse(chunked(d({ type: "error" }) + "\n", 7), "m")
    expect(viaDefault).toEqual({
      error: { message: "codex upstream failure", type: "upstream_error" },
    })
  })

  it("stops accumulating once a failure event lands", async () => {
    const sse = [
      d({ type: "response.output_text.delta", delta: "partial" }),
      "",
      d({ type: "response.failed", error: { message: "boom" } }),
      "",
      d({ type: "response.completed", response: { usage: { input_tokens: 1, output_tokens: 1 } } }),
      "",
    ].join("\n")
    const result = await collectCodexSse(chunked(sse, 13), "m")
    expect(result).toEqual({ error: { message: "boom", type: "upstream_error" } })
  })
})
