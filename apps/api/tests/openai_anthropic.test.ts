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
})
