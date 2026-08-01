import { describe, expect, it } from "vitest"
import {
  anthropicSseToOpenAIStream,
  anthropicToOpenAIChatRequest,
  anthropicToOpenAIResponse,
  openaiSseToAnthropicStream,
  openaiToAnthropicMessage,
  openaiToAnthropicMessages,
  stripCacheControl,
} from "../src/proxy/openai_anthropic"

describe("openaiToAnthropicMessages", () => {
  it("does not invent cache_control", () => {
    const body = openaiToAnthropicMessages({
      model: "claude-opus-5",
      max_tokens: 100,
      messages: [
        { role: "system", content: "hi" },
        { role: "user", content: "hello" },
      ],
    })
    const s = JSON.stringify(body)
    expect(s).not.toContain("cache_control")
    expect(body.model).toBe("claude-opus-5")
    expect(body.system).toBe("hi")
  })

  it("maps stop to stop_sequences", () => {
    const body = openaiToAnthropicMessages({
      model: "m",
      max_tokens: 10,
      messages: [{ role: "user", content: "x" }],
      stop: ["END"],
    })
    expect(body.stop_sequences).toEqual(["END"])
    expect(
      openaiToAnthropicMessages({
        model: "m",
        max_tokens: 10,
        messages: [{ role: "user", content: "x" }],
      }).stop_sequences,
    ).toBeUndefined()
  })

  it("maps tools", () => {
    const body = openaiToAnthropicMessages({
      model: "m",
      max_tokens: 10,
      messages: [{ role: "user", content: "x" }],
      tools: [
        {
          type: "function",
          function: {
            name: "foo",
            description: "d",
            parameters: { type: "object", properties: {} },
          },
        },
      ],
    })
    expect(body.tools).toEqual([
      {
        name: "foo",
        description: "d",
        input_schema: { type: "object", properties: {} },
      },
    ])
  })

  it("maps tool role to tool_result user message", () => {
    const body = openaiToAnthropicMessages({
      model: "m",
      max_tokens: 10,
      messages: [
        { role: "user", content: "call tool" },
        {
          role: "assistant",
          content: null,
          tool_calls: [
            {
              id: "call_1",
              type: "function",
              function: { name: "lookup", arguments: '{"q":"a"}' },
            },
          ],
        },
        {
          role: "tool",
          tool_call_id: "call_1",
          content: "result text",
        },
      ],
    })
    const messages = body.messages as Array<Record<string, unknown>>
    expect(messages).toHaveLength(3)
    expect(messages[2]).toEqual({
      role: "user",
      content: [
        {
          type: "tool_result",
          tool_use_id: "call_1",
          content: "result text",
        },
      ],
    })
  })

  it("maps tool role content arrays to text for tool_result", () => {
    const body = openaiToAnthropicMessages({
      model: "m",
      max_tokens: 10,
      messages: [
        {
          role: "tool",
          tool_call_id: "call_x",
          content: [
            { type: "text", text: "part-a" },
            { type: "text", text: "part-b" },
          ],
        },
      ],
    })
    const messages = body.messages as Array<Record<string, unknown>>
    expect(messages[0]).toEqual({
      role: "user",
      content: [
        {
          type: "tool_result",
          tool_use_id: "call_x",
          content: "part-apart-b",
        },
      ],
    })
  })

  it("maps assistant tool_calls to tool_use blocks", () => {
    const body = openaiToAnthropicMessages({
      model: "m",
      max_tokens: 10,
      messages: [
        {
          role: "assistant",
          content: "using tools",
          tool_calls: [
            {
              id: "call_a",
              type: "function",
              function: { name: "search", arguments: '{"q":"cats"}' },
            },
            {
              id: "call_b",
              type: "function",
              function: { name: "noop", arguments: "{}" },
            },
          ],
        },
      ],
    })
    const messages = body.messages as Array<Record<string, unknown>>
    expect(messages[0]).toEqual({
      role: "assistant",
      content: [
        { type: "text", text: "using tools" },
        {
          type: "tool_use",
          id: "call_a",
          name: "search",
          input: { q: "cats" },
        },
        {
          type: "tool_use",
          id: "call_b",
          name: "noop",
          input: {},
        },
      ],
    })
  })

  it("maps invalid tool_call arguments to raw fallback", () => {
    const body = openaiToAnthropicMessages({
      model: "m",
      max_tokens: 10,
      messages: [
        {
          role: "assistant",
          content: "",
          tool_calls: [
            {
              id: "call_bad",
              type: "function",
              function: { name: "f", arguments: "not-json{" },
            },
          ],
        },
      ],
    })
    const messages = body.messages as Array<{ content: Array<Record<string, unknown>> }>
    expect(messages[0]!.content).toEqual([
      {
        type: "tool_use",
        id: "call_bad",
        name: "f",
        input: { raw: "not-json{" },
      },
    ])
  })

  it("maps image_url data URLs to base64 image blocks", () => {
    const body = openaiToAnthropicMessages({
      model: "m",
      max_tokens: 10,
      messages: [
        {
          role: "user",
          content: [
            { type: "text", text: "what is this?" },
            {
              type: "image_url",
              image_url: { url: "data:image/png;base64,abc123" },
            },
          ],
        },
      ],
    })
    const messages = body.messages as Array<{ content: Array<Record<string, unknown>> }>
    expect(messages[0]!.content).toEqual([
      { type: "text", text: "what is this?" },
      {
        type: "image",
        source: { type: "base64", media_type: "image/png", data: "abc123" },
      },
    ])
  })

  it("maps image_url https URLs to url image blocks", () => {
    const body = openaiToAnthropicMessages({
      model: "m",
      max_tokens: 10,
      messages: [
        {
          role: "user",
          content: [
            {
              type: "image_url",
              image_url: { url: "https://example.com/a.png" },
            },
          ],
        },
      ],
    })
    const messages = body.messages as Array<{ content: Array<Record<string, unknown>> }>
    expect(messages[0]!.content).toEqual([
      {
        type: "image",
        source: { type: "url", url: "https://example.com/a.png" },
      },
    ])
  })

  it("maps tool_choice variants", () => {
    const auto = openaiToAnthropicMessages({
      model: "m",
      max_tokens: 1,
      messages: [{ role: "user", content: "x" }],
      tool_choice: "auto",
    })
    expect(auto.tool_choice).toEqual({ type: "auto" })

    const none = openaiToAnthropicMessages({
      model: "m",
      max_tokens: 1,
      messages: [{ role: "user", content: "x" }],
      tool_choice: "none",
    })
    expect(none.tool_choice).toEqual({ type: "none" })

    const required = openaiToAnthropicMessages({
      model: "m",
      max_tokens: 1,
      messages: [{ role: "user", content: "x" }],
      tool_choice: "required",
    })
    expect(required.tool_choice).toEqual({ type: "any" })

    const named = openaiToAnthropicMessages({
      model: "m",
      max_tokens: 1,
      messages: [{ role: "user", content: "x" }],
      tool_choice: { type: "function", function: { name: "foo" } },
    })
    expect(named.tool_choice).toEqual({ type: "tool", name: "foo" })
  })
})

describe("anthropicToOpenAIResponse", () => {
  it("maps text and usage", () => {
    const out = anthropicToOpenAIResponse(
      {
        id: "msg_1",
        content: [{ type: "text", text: "ok" }],
        stop_reason: "end_turn",
        usage: { input_tokens: 3, output_tokens: 1 },
      },
      "claude-code/claude-opus-5",
    )
    const choices = out.choices as Array<{ message: { content: string } }>
    const usage = out.usage as { prompt_tokens: number; completion_tokens: number }
    expect(choices[0]!.message.content).toBe("ok")
    expect(usage.prompt_tokens).toBe(3)
    expect(usage.completion_tokens).toBe(1)
  })

  it("maps tool_use blocks to OpenAI tool_calls", () => {
    const out = anthropicToOpenAIResponse(
      {
        id: "msg_2",
        content: [
          { type: "text", text: "calling" },
          {
            type: "tool_use",
            id: "tu_1",
            name: "search",
            input: { q: "x" },
          },
        ],
        stop_reason: "tool_use",
        usage: {
          input_tokens: 2,
          output_tokens: 4,
          cache_read_input_tokens: 1,
          cache_creation_input_tokens: 1,
        },
      },
      "claude-code/m",
    )
    const choice = (out.choices as Array<Record<string, unknown>>)[0]! as {
      message: {
        content: string | null
        tool_calls: Array<Record<string, unknown>>
      }
      finish_reason: string
    }
    expect(choice.finish_reason).toBe("tool_calls")
    expect(choice.message.content).toBe("calling")
    expect(choice.message.tool_calls).toEqual([
      {
        id: "tu_1",
        type: "function",
        function: { name: "search", arguments: '{"q":"x"}' },
      },
    ])
    const usage = out.usage as {
      prompt_tokens: number
      completion_tokens: number
      total_tokens: number
    }
    // 2 + 1 + 1 input-side tokens
    expect(usage.prompt_tokens).toBe(4)
    expect(usage.completion_tokens).toBe(4)
    expect(usage.total_tokens).toBe(8)
  })

  it("maps max_tokens stop to length finish_reason", () => {
    const out = anthropicToOpenAIResponse(
      {
        id: "msg_3",
        content: [{ type: "text", text: "cut" }],
        stop_reason: "max_tokens",
      },
      "m",
    )
    const choices = out.choices as Array<{ finish_reason: string }>
    expect(choices[0]!.finish_reason).toBe("length")
  })
})

describe("anthropicSseToOpenAIStream", () => {
  const SSE = [
    "event: message_start",
    'data: {"type":"message_start","message":{"id":"msg_1"}}',
    "",
    "event: content_block_start",
    'data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}',
    "",
    "event: content_block_delta",
    'data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hel"}}',
    "",
    "event: content_block_delta",
    'data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}',
    "",
    "event: message_stop",
    'data: {"type":"message_stop"}',
    "",
    "",
  ].join("\n")

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

  // Event name must survive chunk boundaries: `event:` and `data:` lines
  // routinely arrive in different network reads.
  for (const size of [SSE.length, 7, 1]) {
    it(`emits deltas and [DONE] with chunk size ${size}`, async () => {
      const out = await collect(anthropicSseToOpenAIStream(chunked(SSE, size), "m"))
      expect(out).toContain('"content":"hel"')
      expect(out).toContain('"content":"lo"')
      expect(out).toContain('"finish_reason":"stop"')
      expect(out).toContain("data: [DONE]")
    })
  }
})

describe("stripCacheControl / anthropicToOpenAIChatRequest", () => {
  it("joins multi-block system prompts without running lines together", () => {
    const out = anthropicToOpenAIChatRequest({
      model: "grok/m",
      messages: [{ role: "user", content: "hi" }],
      system: [
        { type: "text", text: "You are Claude Code." },
        { type: "text", text: "" },
        { type: "text", text: "You are an interactive agent.", cache_control: { type: "ephemeral" } },
      ],
    })
    expect(out.messages[0]).toEqual({
      role: "system",
      content: "You are Claude Code.\n\nYou are an interactive agent.",
    })
  })

  it("forwards stop_sequences as OpenAI stop, dropping empties", () => {
    const out = anthropicToOpenAIChatRequest({
      model: "grok/m",
      messages: [{ role: "user", content: "hi" }],
      stop_sequences: ["END", "", "\n\nHuman:"],
    })
    expect(out.stop).toEqual(["END", "\n\nHuman:"])
    expect(
      anthropicToOpenAIChatRequest({
        model: "grok/m",
        messages: [{ role: "user", content: "hi" }],
      }).stop,
    ).toBeUndefined()
  })

  it("strips cache_control deeply", () => {
    const stripped = stripCacheControl({
      system: [{ type: "text", text: "s", cache_control: { type: "ephemeral" } }],
      messages: [
        {
          role: "user",
          content: [{ type: "text", text: "hi", cache_control: { type: "ephemeral" } }],
        },
      ],
      cache_control: { type: "ephemeral" },
    }) as Record<string, unknown>
    expect(JSON.stringify(stripped)).not.toContain("cache_control")
  })

  it("converts messages and drops cache_control", () => {
    const out = anthropicToOpenAIChatRequest({
      model: "grok/grok-4.5",
      max_tokens: 32,
      system: [
        { type: "text", text: "You are helpful.", cache_control: { type: "ephemeral" } },
      ],
      messages: [
        {
          role: "user",
          content: [{ type: "text", text: "hello", cache_control: { type: "ephemeral" } }],
        },
      ],
      tools: [
        {
          name: "lookup",
          description: "d",
          input_schema: { type: "object", properties: {} },
        },
      ],
      tool_choice: { type: "auto" },
    })
    expect(JSON.stringify(out)).not.toContain("cache_control")
    expect(out.messages).toEqual([
      { role: "system", content: "You are helpful." },
      { role: "user", content: "hello" },
    ])
    expect(out.max_tokens).toBe(32)
    expect(out.tools).toEqual([
      {
        type: "function",
        function: {
          name: "lookup",
          description: "d",
          parameters: { type: "object", properties: {} },
        },
      },
    ])
    expect(out.tool_choice).toBe("auto")
  })

  it("maps tool_use / tool_result", () => {
    const out = anthropicToOpenAIChatRequest({
      model: "grok/x",
      max_tokens: 10,
      messages: [
        {
          role: "assistant",
          content: [
            { type: "tool_use", id: "t1", name: "foo", input: { a: 1 } },
          ],
        },
        {
          role: "user",
          content: [{ type: "tool_result", tool_use_id: "t1", content: "ok" }],
        },
      ],
    })
    expect(out.messages[0]).toMatchObject({
      role: "assistant",
      tool_calls: [
        {
          id: "t1",
          type: "function",
          function: { name: "foo", arguments: '{"a":1}' },
        },
      ],
    })
    expect(out.messages[1]).toEqual({
      role: "tool",
      tool_call_id: "t1",
      content: "ok",
    })
  })
})

describe("openaiToAnthropicMessage", () => {
  it("maps text completion", () => {
    const out = openaiToAnthropicMessage(
      {
        id: "chatcmpl_1",
        choices: [
          {
            message: { role: "assistant", content: "hi" },
            finish_reason: "stop",
          },
        ],
        usage: { prompt_tokens: 3, completion_tokens: 1 },
      },
      "grok/grok-4.5",
    )
    expect(out).toMatchObject({
      type: "message",
      role: "assistant",
      model: "grok/grok-4.5",
      stop_reason: "end_turn",
      content: [{ type: "text", text: "hi" }],
      usage: { input_tokens: 3, output_tokens: 1 },
    })
  })

  it("maps tool_calls to tool_use", () => {
    const out = openaiToAnthropicMessage(
      {
        id: "c",
        choices: [
          {
            message: {
              role: "assistant",
              content: null,
              tool_calls: [
                {
                  id: "call_1",
                  type: "function",
                  function: { name: "foo", arguments: '{"x":2}' },
                },
              ],
            },
            finish_reason: "tool_calls",
          },
        ],
      },
      "grok/m",
    )
    expect(out.stop_reason).toBe("tool_use")
    expect(out.content).toEqual([
      { type: "tool_use", id: "call_1", name: "foo", input: { x: 2 } },
    ])
  })
})

describe("openaiSseToAnthropicStream", () => {
  const OPENAI_SSE = [
    'data: {"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}',
    "",
    'data: {"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"hel"},"finish_reason":null}]}',
    "",
    'data: {"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"lo"},"finish_reason":null}]}',
    "",
    'data: {"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}',
    "",
    "data: [DONE]",
    "",
  ].join("\n")

  const OPENAI_TOOL_SSE = [
    'data: {"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":null},"finish_reason":null}]}',
    "",
    'data: {"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"Read","arguments":""}}]},"finish_reason":null}]}',
    "",
    'data: {"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\\"path\\""}}]},"finish_reason":null}]}',
    "",
    'data: {"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":":\\"a.ts\\"}"}}]},"finish_reason":null}]}',
    "",
    'data: {"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}',
    "",
    "data: [DONE]",
    "",
  ].join("\n")

  const OPENAI_TEXT_THEN_TOOL_SSE = [
    'data: {"id":"c1","choices":[{"index":0,"delta":{"role":"assistant","content":"ok"},"finish_reason":null}]}',
    "",
    'data: {"id":"c1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_x","type":"function","function":{"name":"Bash","arguments":"{\\"cmd\\":\\"ls\\"}"}}]},"finish_reason":null}]}',
    "",
    'data: {"id":"c1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}',
    "",
    "data: [DONE]",
    "",
  ].join("\n")

  const OPENAI_PARALLEL_TOOLS_SSE = [
    'data: {"id":"c1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"A","arguments":"{\\"a\\":1}"}}]},"finish_reason":null}]}',
    "",
    'data: {"id":"c1","choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call_b","type":"function","function":{"name":"B","arguments":"{\\"b\\":2}"}}]},"finish_reason":null}]}',
    "",
    'data: {"id":"c1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}',
    "",
    "data: [DONE]",
    "",
  ].join("\n")

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

  it("emits Anthropic text deltas and message_stop", async () => {
    const out = await collect(openaiSseToAnthropicStream(chunked(OPENAI_SSE, 11), "grok/m"))
    expect(out).toContain("event: message_start")
    expect(out).toContain("event: content_block_delta")
    expect(out).toContain('"text":"hel"')
    expect(out).toContain('"text":"lo"')
    expect(out).toContain("event: message_stop")
  })

  it("maps streamed tool_calls to tool_use + input_json_delta", async () => {
    const out = await collect(
      openaiSseToAnthropicStream(chunked(OPENAI_TOOL_SSE, 13), "grok/grok-4.5"),
    )
    expect(out).toContain("event: content_block_start")
    expect(out).toContain('"type":"tool_use"')
    expect(out).toContain('"id":"call_1"')
    expect(out).toContain('"name":"Read"')
    expect(out).toContain('"type":"input_json_delta"')
    expect(out).toContain('"partial_json":"{\\"path\\""')
    expect(out).toContain('"partial_json":":\\"a.ts\\"}"')
    expect(out).toContain("event: content_block_stop")
    expect(out).toContain('"stop_reason":"tool_use"')
    expect(out).toContain("event: message_stop")
  })

  it("closes text block before tool_use when both appear", async () => {
    const out = await collect(
      openaiSseToAnthropicStream(chunked(OPENAI_TEXT_THEN_TOOL_SSE, OPENAI_TEXT_THEN_TOOL_SSE.length), "grok/m"),
    )
    const textStart = out.indexOf('"type":"text"')
    const toolStart = out.indexOf('"type":"tool_use"')
    const firstStop = out.indexOf("event: content_block_stop")
    expect(textStart).toBeGreaterThan(-1)
    expect(toolStart).toBeGreaterThan(textStart)
    expect(firstStop).toBeGreaterThan(textStart)
    expect(firstStop).toBeLessThan(toolStart)
    expect(out).toContain('"name":"Bash"')
    expect(out).toContain('"stop_reason":"tool_use"')
  })

  it("supports parallel tool_calls indices", async () => {
    const out = await collect(
      openaiSseToAnthropicStream(
        chunked(OPENAI_PARALLEL_TOOLS_SSE, OPENAI_PARALLEL_TOOLS_SSE.length),
        "grok/m",
      ),
    )
    expect(out).toContain('"id":"call_a"')
    expect(out).toContain('"id":"call_b"')
    expect(out).toContain('"name":"A"')
    expect(out).toContain('"name":"B"')
    // two tool_use starts
    expect(out.match(/"type":"tool_use"/g)?.length).toBe(2)
    expect(out).toContain('"stop_reason":"tool_use"')
  })

  /**
   * Rebuild content blocks from the emitted SSE and assert the Anthropic
   * invariant: blocks are strictly sequential — one open at a time, dense
   * ascending indices, never reopened.
   */
  function parseBlocks(
    sse: string,
  ): Array<{ index: number; type: string; id?: string; name?: string; body: string }> {
    const blocks: Array<{
      index: number
      type: string
      id?: string
      name?: string
      body: string
    }> = []
    const byIndex = new Map<number, number>()
    let open: number | null = null
    let event = ""
    let sawMessageStart = false
    let ended = false
    for (const line of sse.split("\n")) {
      if (line.startsWith("event:")) {
        event = line.slice(6).trim()
        continue
      }
      if (!line.startsWith("data:")) continue
      const json = JSON.parse(line.slice(5).trim())
      if (event === "message_start") {
        sawMessageStart = true
      } else if (event === "message_delta" || event === "message_stop") {
        expect(open, "message ended with a content block still open").toBe(null)
        ended = true
      } else if (event.startsWith("content_block")) {
        expect(sawMessageStart, "content block before message_start").toBe(true)
        expect(ended, "content block after the message ended").toBe(false)
      }
      if (event === "content_block_start") {
        expect(open, "a block started while another was still open").toBe(null)
        expect(byIndex.has(json.index), "content block index reused").toBe(false)
        expect(json.index, "content block indices must be dense").toBe(blocks.length)
        byIndex.set(json.index, blocks.length)
        blocks.push({
          index: json.index,
          type: json.content_block.type,
          id: json.content_block.id,
          name: json.content_block.name,
          body: "",
        })
        open = json.index
      } else if (event === "content_block_delta") {
        expect(open, "delta outside an open block").toBe(json.index)
        const b = blocks[byIndex.get(json.index)!]!
        b.body += json.delta.partial_json ?? json.delta.text ?? ""
      } else if (event === "content_block_stop") {
        expect(open).toBe(json.index)
        open = null
      }
    }
    expect(open, "stream ended with an unclosed content block").toBe(null)
    expect(ended, "stream ended without message_delta / message_stop").toBe(true)
    return blocks
  }

  it("keeps one tool call in one block when text interleaves its arguments", async () => {
    // Grok 4.5 emits `content` between `arguments` fragments. Closing the tool
    // block for that text used to split one call across two blocks sharing an
    // id, truncating file_path and making both halves invalid JSON.
    const sse = [
      'data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"Read","arguments":""}}]}}]}',
      "",
      'data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\\"file_path\\":\\"/repo/p"}}]}}]}',
      "",
      'data: {"choices":[{"index":0,"delta":{"content":"one moment"}}]}',
      "",
      'data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ackage.json\\"}"}}]}}]}',
      "",
      'data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}',
      "",
      "data: [DONE]",
      "",
    ].join("\n")

    const out = await collect(
      openaiSseToAnthropicStream(chunked(sse, 17), "grok/grok-4.5"),
    )
    const blocks = parseBlocks(out)
    const tools = blocks.filter((b) => b.type === "tool_use")
    expect(tools).toHaveLength(1)
    expect(tools[0]!.id).toBe("call_1")
    expect(JSON.parse(tools[0]!.body)).toEqual({ file_path: "/repo/package.json" })
    // Text is preserved, in its own block.
    expect(blocks.filter((b) => b.type === "text").map((b) => b.body)).toEqual([
      "one moment",
    ])
    expect(out).toContain('"stop_reason":"tool_use"')
  })

  it("flushes buffered text between two tool calls in order", async () => {
    const sse = [
      'data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"Read","arguments":"{\\"file_path\\":\\"/a\\"}"}}]}}]}',
      "",
      'data: {"choices":[{"index":0,"delta":{"content":"now the other one"}}]}',
      "",
      'data: {"choices":[{"index":1,"delta":{"tool_calls":[{"index":1,"id":"call_b","type":"function","function":{"name":"Read","arguments":"{\\"file_path\\":\\"/b\\"}"}}]}}]}',
      "",
      'data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}',
      "",
      "data: [DONE]",
      "",
    ].join("\n")

    const blocks = parseBlocks(
      await collect(openaiSseToAnthropicStream(chunked(sse, 29), "grok/m")),
    )
    expect(blocks.map((b) => b.type)).toEqual(["tool_use", "text", "tool_use"])
    expect(blocks[1]!.body).toBe("now the other one")
    expect(blocks.map((b) => b.id).filter(Boolean)).toEqual(["call_a", "call_b"])
  })

  it("closes a tool block and flushes text when the stream ends abruptly", async () => {
    // No finish_reason, no [DONE] — upstream connection simply drops.
    const sse = [
      'data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"Read","arguments":"{\\"file_path\\":\\"/a\\"}"}}]}}]}',
      "",
      'data: {"choices":[{"index":0,"delta":{"content":"trailing"}}]}',
      "",
    ].join("\n")

    const out = await collect(openaiSseToAnthropicStream(chunked(sse, 31), "grok/m"))
    const blocks = parseBlocks(out)
    expect(blocks.map((b) => b.type)).toEqual(["tool_use", "text"])
    expect(JSON.parse(blocks[0]!.body)).toEqual({ file_path: "/a" })
    expect(blocks[1]!.body).toBe("trailing")
    expect(out).toContain('"stop_reason":"tool_use"')
  })

  it("emits a well-formed empty message when upstream sends nothing usable", async () => {
    const out = await collect(
      openaiSseToAnthropicStream(chunked("data: [DONE]\n\n", 8), "grok/m"),
    )
    const blocks = parseBlocks(out)
    expect(blocks.map((b) => b.type)).toEqual(["text"])
    expect(blocks[0]!.body).toBe("")
    expect(out).toContain("event: message_start")
    expect(out).toContain('"stop_reason":"end_turn"')
    expect(out).toContain("event: message_stop")
  })

  it("waits for the id before opening a block when name arrives first", async () => {
    const sse = [
      'data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"Read"}}]}}]}',
      "",
      'data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_late","type":"function","function":{"arguments":"{\\"file_path\\":\\"/a\\"}"}}]}}]}',
      "",
      'data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}',
      "",
      "data: [DONE]",
      "",
    ].join("\n")

    const tools = parseBlocks(
      await collect(openaiSseToAnthropicStream(chunked(sse, 41), "grok/m")),
    ).filter((b) => b.type === "tool_use")
    expect(tools).toHaveLength(1)
    expect(tools[0]!.id).toBe("call_late")
    expect(tools[0]!.name).toBe("Read")
    expect(JSON.parse(tools[0]!.body)).toEqual({ file_path: "/a" })
  })

  it("still emits a name-only tool call as a zero-argument call", async () => {
    const sse = [
      'data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"Ping"}}]}}]}',
      "",
      'data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}',
      "",
      "data: [DONE]",
      "",
    ].join("\n")

    const out = await collect(openaiSseToAnthropicStream(chunked(sse, 43), "grok/m"))
    const tools = parseBlocks(out).filter((b) => b.type === "tool_use")
    expect(tools).toHaveLength(1)
    expect(tools[0]!.name).toBe("Ping")
    expect(tools[0]!.body).toBe("")
    expect(out).toContain('"stop_reason":"tool_use"')
  })

  it("keeps parallel calls intact when their fragments alternate", async () => {
    // Fragments arriving 0,1,0,1,0,1 — the live call streams, the second is
    // held back and emitted whole, so neither JSON gets interleaved.
    const frag = (index: number, patch: string) =>
      `data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":${index},${patch}}]}}]}`
    const sse = [
      frag(0, '"id":"call_a","type":"function","function":{"name":"A","arguments":""}'),
      "",
      frag(1, '"id":"call_b","type":"function","function":{"name":"B","arguments":""}'),
      "",
      frag(0, '"function":{"arguments":"{\\"a\\":"}'),
      "",
      frag(1, '"function":{"arguments":"{\\"b\\":"}'),
      "",
      frag(0, '"function":{"arguments":"1}"}'),
      "",
      frag(1, '"function":{"arguments":"2}"}'),
      "",
      'data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}',
      "",
      "data: [DONE]",
      "",
    ].join("\n")

    const tools = parseBlocks(
      await collect(openaiSseToAnthropicStream(chunked(sse, 47), "grok/m")),
    ).filter((b) => b.type === "tool_use")
    expect(tools.map((t) => t.id)).toEqual(["call_a", "call_b"])
    expect(tools.map((t) => t.name)).toEqual(["A", "B"])
    expect(tools.map((t) => JSON.parse(t.body))).toEqual([{ a: 1 }, { b: 2 }])
  })

  it("reports real upstream usage when the provider sends it", async () => {
    const sse = [
      'data: {"choices":[{"index":0,"delta":{"content":"hi"}}]}',
      "",
      'data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}',
      "",
      'data: {"choices":[],"usage":{"prompt_tokens":1234,"completion_tokens":56}}',
      "",
      "data: [DONE]",
      "",
    ].join("\n")

    const out = await collect(openaiSseToAnthropicStream(chunked(sse, 53), "grok/m"))
    parseBlocks(out)
    expect(out).toContain('"input_tokens":1234')
    expect(out).toContain('"output_tokens":56')
  })

  it("treats a re-sent function name as the same call, not a new one", async () => {
    const sse = [
      'data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"Read","arguments":"{\\"file_path\\":"}}]}}]}',
      "",
      'data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"Read","arguments":"\\"/a\\"}"}}]}}]}',
      "",
      'data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}',
      "",
      "data: [DONE]",
      "",
    ].join("\n")

    const tools = parseBlocks(
      await collect(openaiSseToAnthropicStream(chunked(sse, 59), "grok/m")),
    ).filter((b) => b.type === "tool_use")
    expect(tools).toHaveLength(1)
    expect(tools[0]!.id).toBe("call_1")
    expect(JSON.parse(tools[0]!.body)).toEqual({ file_path: "/a" })
  })

  it("adopts a late id for a call that opened on arguments alone", async () => {
    const sse = [
      'data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"Read","arguments":"{\\"file_path\\":\\"/a\\"}"}}]}}]}',
      "",
      'data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_late","type":"function"}]}}]}',
      "",
      'data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}',
      "",
      "data: [DONE]",
      "",
    ].join("\n")

    const tools = parseBlocks(
      await collect(openaiSseToAnthropicStream(chunked(sse, 61), "grok/m")),
    ).filter((b) => b.type === "tool_use")
    expect(tools).toHaveLength(1)
    expect(tools[0]!.name).toBe("Read")
    expect(JSON.parse(tools[0]!.body)).toEqual({ file_path: "/a" })
  })

  it("emits a well-formed message for a zero-byte upstream body", async () => {
    // No [DONE], no events at all — the shape that makes clients retry silently.
    const out = await collect(openaiSseToAnthropicStream(chunked("", 8), "grok/m"))
    const blocks = parseBlocks(out)
    expect(blocks.map((b) => b.type)).toEqual(["text"])
    expect(out).toContain("event: message_start")
    expect(out).toContain("event: message_stop")
  })

  it("reads usage that rides on the finish_reason chunk", async () => {
    const sse = [
      'data: {"choices":[{"index":0,"delta":{"content":"hi"}}]}',
      "",
      'data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":8}}',
      "",
      "data: [DONE]",
      "",
    ].join("\n")

    const out = await collect(openaiSseToAnthropicStream(chunked(sse, 67), "grok/m"))
    parseBlocks(out)
    expect(out).toContain('"input_tokens":7')
    expect(out).toContain('"output_tokens":8')
  })

  it("ignores content that arrives after finish_reason", async () => {
    const sse = [
      'data: {"choices":[{"index":0,"delta":{"content":"done"},"finish_reason":"stop"}]}',
      "",
      'data: {"choices":[{"index":0,"delta":{"content":"late"}}]}',
      "",
      "data: [DONE]",
      "",
    ].join("\n")

    const out = await collect(openaiSseToAnthropicStream(chunked(sse, 37), "grok/m"))
    const blocks = parseBlocks(out)
    expect(blocks.map((b) => b.body)).toEqual(["done"])
    expect(out).not.toContain("late")
  })

  it("splits a new tool id at the same index into its own block", async () => {
    const sse = [
      'data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"Read","arguments":"{\\"file_path\\":\\"/a\\"}"}}]}}]}',
      "",
      'data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_b","type":"function","function":{"name":"Read","arguments":"{\\"file_path\\":\\"/b\\"}"}}]}}]}',
      "",
      'data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}',
      "",
      "data: [DONE]",
      "",
    ].join("\n")

    const tools = parseBlocks(
      await collect(openaiSseToAnthropicStream(chunked(sse, 23), "grok/m")),
    ).filter((b) => b.type === "tool_use")
    expect(tools.map((t) => t.id)).toEqual(["call_a", "call_b"])
    expect(tools.map((t) => JSON.parse(t.body))).toEqual([
      { file_path: "/a" },
      { file_path: "/b" },
    ])
  })

  it("emits parallel tool calls as sequential, non-overlapping blocks", async () => {
    const tools = parseBlocks(
      await collect(
        openaiSseToAnthropicStream(
          chunked(OPENAI_PARALLEL_TOOLS_SSE, 19),
          "grok/m",
        ),
      ),
    ).filter((b) => b.type === "tool_use")
    expect(tools.map((t) => t.id)).toEqual(["call_a", "call_b"])
    expect(tools.map((t) => JSON.parse(t.body))).toEqual([{ a: 1 }, { b: 2 }])
  })
})

describe("anthropicSseToOpenAIStream tool_use", () => {
  const ANTHROPIC_TOOL_SSE = [
    "event: message_start",
    'data: {"type":"message_start","message":{"id":"msg_1"}}',
    "",
    "event: content_block_start",
    'data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"Read","input":{}}}',
    "",
    "event: content_block_delta",
    'data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\\"path\\":\\"x\\"}"}}',
    "",
    "event: content_block_stop",
    'data: {"type":"content_block_stop","index":0}',
    "",
    "event: message_delta",
    'data: {"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":5}}',
    "",
    "event: message_stop",
    'data: {"type":"message_stop"}',
    "",
  ].join("\n")

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

  it("maps tool_use stream to OpenAI tool_calls chunks", async () => {
    const bytes = new TextEncoder().encode(ANTHROPIC_TOOL_SSE)
    const stream = new ReadableStream<Uint8Array>({
      start(c) {
        c.enqueue(bytes)
        c.close()
      },
    })
    const out = await collect(anthropicSseToOpenAIStream(stream, "claude-code/m"))
    expect(out).toContain('"tool_calls"')
    expect(out).toContain('"id":"toolu_1"')
    expect(out).toContain('"name":"Read"')
    expect(out).toContain('"arguments":"{\\"path\\":\\"x\\"}"')
    expect(out).toContain('"finish_reason":"tool_calls"')
    expect(out).toContain("data: [DONE]")
  })
})

describe("anthropicSseToOpenAIStream usage + error", () => {
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

  it("attaches usage on the final chunk: message_start input (+ cache fields) and message_delta output", async () => {
    const sse = [
      "event: message_start",
      'data: {"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":10,"cache_read_input_tokens":2,"cache_creation_input_tokens":3}}}',
      "",
      "event: content_block_start",
      'data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}',
      "",
      "event: content_block_delta",
      'data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}',
      "",
      "event: content_block_stop",
      'data: {"type":"content_block_stop","index":0}',
      "",
      "event: message_delta",
      'data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}',
      "",
      "event: message_stop",
      'data: {"type":"message_stop"}',
      "",
    ].join("\n")
    const out = await collect(anthropicSseToOpenAIStream(chunked(sse, 17), "claude-code/m"))
    // 10 + 2 + 3 input-side tokens, same summation anthropicToOpenAIResponse uses.
    expect(out).toContain('"prompt_tokens":15')
    expect(out).toContain('"completion_tokens":7')
    expect(out).toContain('"total_tokens":22')
    expect(out).toContain('"finish_reason":"stop"')
    expect(out).toContain("data: [DONE]")
  })

  it("omits usage entirely when neither message_start nor message_delta reported any", async () => {
    const sse = [
      "event: message_start",
      'data: {"type":"message_start","message":{"id":"msg_1"}}',
      "",
      "event: message_stop",
      'data: {"type":"message_stop"}',
      "",
    ].join("\n")
    const out = await collect(anthropicSseToOpenAIStream(chunked(sse, 13), "claude-code/m"))
    expect(out).not.toContain('"usage"')
    expect(out).toContain("data: [DONE]")
  })

  it("converts a mid-stream event: error into an OpenAI error line — no finish chunk, no [DONE]", async () => {
    const sse = [
      "event: message_start",
      'data: {"type":"message_start","message":{"id":"msg_1"}}',
      "",
      "event: content_block_start",
      'data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}',
      "",
      "event: content_block_delta",
      'data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"partial"}}',
      "",
      "event: error",
      'data: {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}',
      "",
    ].join("\n")
    const out = await collect(anthropicSseToOpenAIStream(chunked(sse, 19), "claude-code/m"))
    expect(out).toContain('"content":"partial"')
    expect(out).toContain('data: {"error":{"message":"Overloaded","type":"overloaded_error"}}')
    expect(out).not.toContain("[DONE]")
    expect(out).not.toContain('"finish_reason":"stop"')
  })

  it("falls back to a generic message/type when the error event omits them", async () => {
    const sse = ["event: error", 'data: {"type":"error"}', ""].join("\n")
    const out = await collect(anthropicSseToOpenAIStream(chunked(sse, 5), "claude-code/m"))
    expect(out).toContain('data: {"error":{"message":"upstream error","type":"api_error"}}')
  })
})

describe("anthropicToOpenAIChatRequest: server-side tool dropping", () => {
  it("drops an Anthropic server-side tool (no input_schema, non-custom type)", () => {
    const out = anthropicToOpenAIChatRequest({
      model: "grok/m",
      messages: [{ role: "user", content: "hi" }],
      tools: [{ type: "web_search_20250305", name: "web_search", max_uses: 5 }],
    })
    expect(out.tools).toEqual([])
  })

  it("still converts a custom tool with input_schema", () => {
    const out = anthropicToOpenAIChatRequest({
      model: "grok/m",
      messages: [{ role: "user", content: "hi" }],
      tools: [
        {
          type: "custom",
          name: "lookup",
          description: "d",
          input_schema: { type: "object", properties: { q: { type: "string" } } },
        },
      ],
    })
    expect(out.tools).toEqual([
      {
        type: "function",
        function: {
          name: "lookup",
          description: "d",
          parameters: { type: "object", properties: { q: { type: "string" } } },
        },
      },
    ])
  })

  it("still converts a client tool with no type field at all", () => {
    const out = anthropicToOpenAIChatRequest({
      model: "grok/m",
      messages: [{ role: "user", content: "hi" }],
      tools: [{ name: "lookup", description: "d" }],
    })
    expect(out.tools).toEqual([
      {
        type: "function",
        function: { name: "lookup", description: "d", parameters: { type: "object", properties: {} } },
      },
    ])
  })

  it("passes an already-OpenAI-shaped tool through unchanged", () => {
    const openaiTool = {
      type: "function",
      function: { name: "lookup", parameters: { type: "object", properties: {} } },
    }
    const out = anthropicToOpenAIChatRequest({
      model: "grok/m",
      messages: [{ role: "user", content: "hi" }],
      tools: [openaiTool],
    })
    expect(out.tools).toEqual([openaiTool])
  })

  it("drops server tools alongside a client tool in the same request", () => {
    const out = anthropicToOpenAIChatRequest({
      model: "grok/m",
      messages: [{ role: "user", content: "hi" }],
      tools: [
        { type: "bash_20250124", name: "bash" },
        { name: "lookup", input_schema: { type: "object", properties: {} } },
      ],
    })
    expect(out.tools).toEqual([
      {
        type: "function",
        function: { name: "lookup", description: undefined, parameters: { type: "object", properties: {} } },
      },
    ])
  })
})

describe("anthropicToOpenAIChatRequest: tool_result images", () => {
  it("leaves a text-only tool_result unchanged (no follow-up user message)", () => {
    const out = anthropicToOpenAIChatRequest({
      model: "grok/m",
      messages: [
        {
          role: "user",
          content: [{ type: "tool_result", tool_use_id: "t1", content: "plain text result" }],
        },
      ],
    })
    expect(out.messages).toEqual([
      { role: "tool", tool_call_id: "t1", content: "plain text result" },
    ])
  })

  it("re-attaches a single image as a follow-up user message with a placeholder in the tool message", () => {
    const out = anthropicToOpenAIChatRequest({
      model: "grok/m",
      messages: [
        {
          role: "user",
          content: [
            {
              type: "tool_result",
              tool_use_id: "t1",
              content: [
                { type: "text", text: "here is a screenshot" },
                {
                  type: "image",
                  source: { type: "base64", media_type: "image/png", data: "abc123" },
                },
              ],
            },
          ],
        },
      ],
    })
    expect(out.messages).toEqual([
      {
        role: "tool",
        tool_call_id: "t1",
        content: "here is a screenshot\n[image attached below]",
      },
      {
        role: "user",
        content: [
          { type: "text", text: "[Image(s) from tool result t1]" },
          {
            type: "image_url",
            image_url: { url: "data:image/png;base64,abc123" },
          },
        ],
      },
    ])
  })

  it("counts multiple images in the placeholder and forwards every image_url part", () => {
    const out = anthropicToOpenAIChatRequest({
      model: "grok/m",
      messages: [
        {
          role: "user",
          content: [
            {
              type: "tool_result",
              tool_use_id: "t2",
              content: [
                { type: "image", source: { type: "url", url: "https://example.com/a.png" } },
                { type: "image", source: { type: "url", url: "https://example.com/b.png" } },
              ],
            },
          ],
        },
      ],
    })
    const [toolMessage, imageMessage] = out.messages as Array<Record<string, unknown>>
    expect(toolMessage).toEqual({
      role: "tool",
      tool_call_id: "t2",
      content: "[2 images attached below]",
    })
    expect(imageMessage!.content).toEqual([
      { type: "text", text: "[Image(s) from tool result t2]" },
      { type: "image_url", image_url: { url: "https://example.com/a.png" } },
      { type: "image_url", image_url: { url: "https://example.com/b.png" } },
    ])
  })
})

describe("anthropicToOpenAIChatRequest: reasoning_effort / output_config.effort", () => {
  it("falls back to output_config.effort when reasoning_effort is absent", () => {
    const out = anthropicToOpenAIChatRequest({
      model: "grok/m",
      messages: [{ role: "user", content: "hi" }],
      output_config: { effort: "high" },
    })
    expect(out.reasoning_effort).toBe("high")
  })

  it("prefers an explicit reasoning_effort over output_config.effort", () => {
    const out = anthropicToOpenAIChatRequest({
      model: "grok/m",
      messages: [{ role: "user", content: "hi" }],
      reasoning_effort: "low",
      output_config: { effort: "high" },
    })
    expect(out.reasoning_effort).toBe("low")
  })

  it("leaves reasoning_effort unset when neither field is present", () => {
    const out = anthropicToOpenAIChatRequest({
      model: "grok/m",
      messages: [{ role: "user", content: "hi" }],
    })
    expect(out.reasoning_effort).toBeUndefined()
  })

  it("ignores a non-string output_config.effort", () => {
    const out = anthropicToOpenAIChatRequest({
      model: "grok/m",
      messages: [{ role: "user", content: "hi" }],
      output_config: { effort: 5 },
    })
    expect(out.reasoning_effort).toBeUndefined()
  })

  it("never reads thinking — budget_tokens is not mapped to reasoning_effort", () => {
    const out = anthropicToOpenAIChatRequest({
      model: "grok/m",
      messages: [{ role: "user", content: "hi" }],
      thinking: { type: "enabled", budget_tokens: 4096 },
    })
    expect(out.reasoning_effort).toBeUndefined()
    expect(JSON.stringify(out)).not.toContain("budget_tokens")
  })
})

describe("anthropicToOpenAIChatRequest: output_format → json_schema", () => {
  it("includes name: response inside json_schema", () => {
    const out = anthropicToOpenAIChatRequest({
      model: "grok/m",
      messages: [{ role: "user", content: "hi" }],
      output_format: { type: "json_schema", schema: { type: "object", properties: {} } },
    })
    expect(out.response_format).toEqual({
      type: "json_schema",
      json_schema: { name: "response", schema: { type: "object", properties: {} } },
    })
  })
})
