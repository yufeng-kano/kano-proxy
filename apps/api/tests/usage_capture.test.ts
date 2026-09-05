import { describe, expect, it } from "vitest"
import {
  createAnthropicSseUsageSniffer,
  createOpenAISseUsageSniffer,
  fromAnthropicUsage,
  fromOpenAIUsage,
} from "../src/logging/usage_capture"

describe("fromOpenAIUsage — completion_tokens_details.reasoning_tokens", () => {
  it("keeps completion_tokens as-is without double-adding reasoning_tokens", () => {
    expect(
      fromOpenAIUsage({
        prompt_tokens: 100,
        completion_tokens: 40,
        completion_tokens_details: { reasoning_tokens: 15 },
      }),
    ).toMatchObject({ promptTokens: 100, completionTokens: 40 })
  })

  it("leaves completionTokens unchanged when reasoning_tokens is absent", () => {
    expect(fromOpenAIUsage({ prompt_tokens: 10, completion_tokens: 4 }).completionTokens).toBe(4)
  })

  it("does not fabricate completionTokens from reasoning_tokens alone when completion_tokens is absent", () => {
    expect(
      fromOpenAIUsage({
        prompt_tokens: 10,
        completion_tokens_details: { reasoning_tokens: 15 },
      }).completionTokens,
    ).toBeNull()
  })
})

function byteChunks(text: string, size: number): Uint8Array[] {
  const bytes = new TextEncoder().encode(text)
  const out: Uint8Array[] = []
  for (let i = 0; i < bytes.length; i += size) out.push(bytes.subarray(i, i + size))
  return out
}

const d = (obj: unknown) => `data: ${JSON.stringify(obj)}`

describe("fromOpenAIUsage", () => {
  it("stores prompt_tokens as-is (already cache-inclusive) and completion_tokens", () => {
    expect(fromOpenAIUsage({ prompt_tokens: 100, completion_tokens: 40 })).toEqual({
      promptTokens: 100,
      completionTokens: 40,
      cacheReadInputTokens: null,
      cacheCreationInputTokens: null,
    })
  })

  it("absent prompt_tokens_details / cache_creation_input_tokens => NULL, never 0", () => {
    const out = fromOpenAIUsage({ prompt_tokens: 5, completion_tokens: 1 })
    expect(out.cacheReadInputTokens).toBeNull()
    expect(out.cacheCreationInputTokens).toBeNull()
  })

  it("reads cache read/write fields from prompt_tokens_details", () => {
    expect(
      fromOpenAIUsage({
        prompt_tokens: 100,
        completion_tokens: 40,
        prompt_tokens_details: { cached_tokens: 20, cache_write_tokens: 6 },
      }),
    ).toEqual({
      promptTokens: 100,
      completionTokens: 40,
      cacheReadInputTokens: 20,
      cacheCreationInputTokens: 6,
    })
  })

  it("uses the proxy cache-creation extension only when no upstream cache-write value exists", () => {
    expect(
      fromOpenAIUsage({
        prompt_tokens: 100,
        completion_tokens: 40,
        prompt_tokens_details: { cache_write_tokens: 6 },
        cache_creation_input_tokens: 9,
      }).cacheCreationInputTokens,
    ).toBe(6)
    expect(
      fromOpenAIUsage({ prompt_tokens: 100, completion_tokens: 40, cache_creation_input_tokens: 9 })
        .cacheCreationInputTokens,
    ).toBe(9)
  })

  it("cached_tokens: 0 is a real reported value, not treated as absent", () => {
    const out = fromOpenAIUsage({
      prompt_tokens: 10,
      completion_tokens: 2,
      prompt_tokens_details: { cached_tokens: 0 },
    })
    expect(out.cacheReadInputTokens).toBe(0)
  })

  it("returns all-null for a missing usage object", () => {
    expect(fromOpenAIUsage(undefined)).toEqual({
      promptTokens: null,
      completionTokens: null,
      cacheReadInputTokens: null,
      cacheCreationInputTokens: null,
    })
    expect(fromOpenAIUsage(null)).toEqual({
      promptTokens: null,
      completionTokens: null,
      cacheReadInputTokens: null,
      cacheCreationInputTokens: null,
    })
  })
})

describe("fromAnthropicUsage", () => {
  it("sums input_tokens + cache_read + cache_creation into promptTokens (total)", () => {
    expect(
      fromAnthropicUsage({
        input_tokens: 10,
        output_tokens: 5,
        cache_read_input_tokens: 2,
        cache_creation_input_tokens: 3,
      }),
    ).toEqual({
      promptTokens: 15,
      completionTokens: 5,
      cacheReadInputTokens: 2,
      cacheCreationInputTokens: 3,
    })
  })

  it("absent cache fields default to 0 when a usage object exists — Anthropic defines them", () => {
    expect(fromAnthropicUsage({ input_tokens: 10, output_tokens: 5 })).toEqual({
      promptTokens: 10,
      completionTokens: 5,
      cacheReadInputTokens: 0,
      cacheCreationInputTokens: 0,
    })
  })

  it("a wholly absent usage object is NULL, not zero — unlike OpenAI-absent cache details, still NULL either way", () => {
    expect(fromAnthropicUsage(undefined)).toEqual({
      promptTokens: null,
      completionTokens: null,
      cacheReadInputTokens: null,
      cacheCreationInputTokens: null,
    })
    expect(fromAnthropicUsage(null)).toEqual({
      promptTokens: null,
      completionTokens: null,
      cacheReadInputTokens: null,
      cacheCreationInputTokens: null,
    })
  })

  it("missing output_tokens is NULL, not zero, even when a usage object exists", () => {
    expect(fromAnthropicUsage({ input_tokens: 10 }).completionTokens).toBeNull()
  })
})

describe("createOpenAISseUsageSniffer", () => {
  it("captures usage from the final chunk, split across arbitrary chunk boundaries", () => {
    const sse = [
      d({ choices: [{ index: 0, delta: { content: "hi" } }] }),
      "",
      d({ choices: [{ index: 0, delta: {}, finish_reason: "stop" }] }),
      "",
      d({ choices: [], usage: { prompt_tokens: 100, completion_tokens: 50 } }),
      "",
      "data: [DONE]",
      "",
    ].join("\n")
    const sniffer = createOpenAISseUsageSniffer()
    for (const chunk of byteChunks(sse, 7)) sniffer.feed(chunk)
    expect(sniffer.finish()).toEqual({
      promptTokens: 100,
      completionTokens: 50,
      cacheReadInputTokens: null,
      cacheCreationInputTokens: null,
    })
  })

  it("captures cache fields (prompt_tokens_details + cache_creation_input_tokens extension)", () => {
    const sse =
      d({
        choices: [],
        usage: {
          prompt_tokens: 100,
          completion_tokens: 50,
          prompt_tokens_details: { cached_tokens: 20 },
          cache_creation_input_tokens: 5,
        },
      }) + "\n"
    const sniffer = createOpenAISseUsageSniffer()
    for (const chunk of byteChunks(sse, 11)) sniffer.feed(chunk)
    expect(sniffer.finish()).toEqual({
      promptTokens: 100,
      completionTokens: 50,
      cacheReadInputTokens: 20,
      cacheCreationInputTokens: 5,
    })
  })

  it("reassembles a usage line split mid-line, with nothing to report until the newline lands", () => {
    const line = d({ choices: [], usage: { prompt_tokens: 42, completion_tokens: 7 } })
    const sniffer = createOpenAISseUsageSniffer()
    const bytes = new TextEncoder().encode(line)
    const mid = Math.floor(bytes.length / 2)
    sniffer.feed(bytes.subarray(0, mid))
    sniffer.feed(bytes.subarray(mid))
    expect(sniffer.finish()).toBeNull()
    sniffer.feed(new TextEncoder().encode("\n"))
    expect(sniffer.finish()).toEqual({
      promptTokens: 42,
      completionTokens: 7,
      cacheReadInputTokens: null,
      cacheCreationInputTokens: null,
    })
  })

  it("returns null when no chunk ever carried usage", () => {
    const sse = [d({ choices: [{ index: 0, delta: { content: "hi" } }] }), "", "data: [DONE]", ""].join(
      "\n",
    )
    const sniffer = createOpenAISseUsageSniffer()
    for (const chunk of byteChunks(sse, 9)) sniffer.feed(chunk)
    expect(sniffer.finish()).toBeNull()
  })

  it("tolerates a malformed data line and still captures a later good one", () => {
    const sniffer = createOpenAISseUsageSniffer()
    sniffer.feed(new TextEncoder().encode('data: {"usage": not valid json\n'))
    sniffer.feed(
      new TextEncoder().encode(
        d({ choices: [], usage: { prompt_tokens: 3, completion_tokens: 1 } }) + "\n",
      ),
    )
    expect(sniffer.finish()).toEqual({
      promptTokens: 3,
      completionTokens: 1,
      cacheReadInputTokens: null,
      cacheCreationInputTokens: null,
    })
  })

  it("skips an over-long non-terminal line and keeps capturing from the next one", () => {
    const sniffer = createOpenAISseUsageSniffer()
    const huge = "data: " + "x".repeat(300 * 1024) // one line, no newline — well past the 256 KiB cap
    expect(() => sniffer.feed(new TextEncoder().encode(huge))).not.toThrow()
    expect(sniffer.finish()).toBeNull()
    sniffer.feed(
      new TextEncoder().encode(
        "\n" + d({ choices: [], usage: { prompt_tokens: 9, completion_tokens: 9 } }) + "\n",
      ),
    )
    expect(sniffer.finish()).toMatchObject({ promptTokens: 9, completionTokens: 9 })
  })
})

describe("createOpenAISseUsageSniffer — over-long Responses terminal event (docs/logging.md, native codex path)", () => {
  const usage = {
    input_tokens: 82665,
    input_tokens_details: { cached_tokens: 82432 },
    output_tokens: 14,
    output_tokens_details: { reasoning_tokens: 0 },
    total_tokens: 82679,
  }
  /** A `response.completed` event the size of a real Codex turn: the request is echoed back ahead of `usage`. */
  const terminal = (type: string, filler: number, extra: Record<string, unknown> = {}) =>
    `event: ${type}\n` +
    d({
      type,
      sequence_number: 42,
      response: {
        id: "resp_1",
        object: "response",
        status: type === "response.completed" ? "completed" : "incomplete",
        instructions: "i".repeat(filler),
        tools: [{ type: "function", name: "shell", parameters: { properties: { usage: { type: "string" } } } }],
        output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "hi" }] }],
        ...extra,
        usage,
        user: null,
        metadata: {},
      },
    }) +
    "\n\n"

  it("captures usage and completion from a response.completed line far past the carry cap, fed in 16 KiB chunks", () => {
    const sniffer = createOpenAISseUsageSniffer()
    for (const chunk of byteChunks(terminal("response.completed", 600 * 1024), 16 * 1024)) sniffer.feed(chunk)
    expect(sniffer.finish()).toEqual({
      promptTokens: 82665,
      completionTokens: 14,
      cacheReadInputTokens: 82432,
      cacheCreationInputTokens: null,
    })
    expect(sniffer.complete()).toBe(true)
  })

  it("still works when the usage marker and object straddle chunk boundaries (7-byte chunks)", () => {
    const sniffer = createOpenAISseUsageSniffer()
    for (const chunk of byteChunks(terminal("response.incomplete", 300 * 1024), 7)) sniffer.feed(chunk)
    expect(sniffer.finish()).toMatchObject({ promptTokens: 82665, completionTokens: 14, cacheReadInputTokens: 82432 })
    expect(sniffer.complete()).toBe(true)
  })

  it("identifies the terminal event by its leading \"type\" when no event: line precedes it", () => {
    const sniffer = createOpenAISseUsageSniffer()
    const line = terminal("response.completed", 300 * 1024).replace(/^event: [^\n]*\n/, "")
    for (const chunk of byteChunks(line, 32 * 1024)) sniffer.feed(chunk)
    expect(sniffer.finish()).toMatchObject({ promptTokens: 82665 })
    expect(sniffer.complete()).toBe(true)
  })

  it("identifies it by the event: line when the JSON's type key is not first", () => {
    const sniffer = createOpenAISseUsageSniffer()
    const line =
      "event: response.completed\n" +
      d({ sequence_number: 1, response: { instructions: "i".repeat(300 * 1024), usage }, type: "response.completed" }) +
      "\n\n"
    for (const chunk of byteChunks(line, 32 * 1024)) sniffer.feed(chunk)
    expect(sniffer.finish()).toMatchObject({ promptTokens: 82665, cacheReadInputTokens: 82432 })
    expect(sniffer.complete()).toBe(true)
  })

  it("ignores a tool schema's usage property and a null usage — only an object with numeric token counts counts", () => {
    const sniffer = createOpenAISseUsageSniffer()
    // An over-long response.created with "usage":null is skipped as a non-terminal line …
    const created = terminal("response.created", 300 * 1024, { usage: null }).replace(/"usage":\{[^}]*\{[^}]*\}[^}]*\}[^}]*\}/, '"usage":null')
    for (const chunk of byteChunks(created, 32 * 1024)) sniffer.feed(chunk)
    expect(sniffer.finish()).toBeNull()
    expect(sniffer.complete()).toBe(false)
    // … then the terminal line, whose tool schema mentions `usage` before the real object, is captured.
    for (const chunk of byteChunks(terminal("response.completed", 300 * 1024), 32 * 1024)) sniffer.feed(chunk)
    expect(sniffer.finish()).toMatchObject({ promptTokens: 82665, completionTokens: 14 })
    expect(sniffer.complete()).toBe(true)
  })

  it("a short response.completed still goes through the ordinary line parser", () => {
    const sniffer = createOpenAISseUsageSniffer()
    sniffer.feed(new TextEncoder().encode(terminal("response.completed", 10)))
    expect(sniffer.finish()).toMatchObject({ promptTokens: 82665, cacheReadInputTokens: 82432 })
    expect(sniffer.complete()).toBe(true)
  })
})

describe("createAnthropicSseUsageSniffer", () => {
  it("merges message_start (input + cache) with message_delta (output) field-wise", () => {
    const sse = [
      "event: message_start",
      d({
        type: "message_start",
        message: {
          id: "msg_1",
          usage: { input_tokens: 10, cache_read_input_tokens: 2, cache_creation_input_tokens: 3 },
        },
      }),
      "",
      "event: content_block_delta",
      d({ type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "hi" } }),
      "",
      "event: message_delta",
      d({ type: "message_delta", delta: { stop_reason: "end_turn" }, usage: { output_tokens: 7 } }),
      "",
      "event: message_stop",
      d({ type: "message_stop" }),
      "",
    ].join("\n")
    const sniffer = createAnthropicSseUsageSniffer()
    for (const chunk of byteChunks(sse, 17)) sniffer.feed(chunk)
    expect(sniffer.finish()).toEqual({
      promptTokens: 15,
      completionTokens: 7,
      cacheReadInputTokens: 2,
      cacheCreationInputTokens: 3,
    })
  })

  it("reassembles event:/data: lines split across arbitrary chunk boundaries", () => {
    const sse = [
      "event: message_start",
      d({ type: "message_start", message: { usage: { input_tokens: 5 } } }),
      "",
    ].join("\n")
    const sniffer = createAnthropicSseUsageSniffer()
    for (const chunk of byteChunks(sse, 3)) sniffer.feed(chunk)
    expect(sniffer.finish()).toEqual({
      promptTokens: 5,
      completionTokens: null,
      cacheReadInputTokens: 0,
      cacheCreationInputTokens: 0,
    })
  })

  it("returns null when neither message_start nor message_delta carried usage", () => {
    const sse = [
      "event: message_start",
      d({ type: "message_start", message: { id: "msg_1" } }),
      "",
      "event: message_stop",
      d({ type: "message_stop" }),
      "",
    ].join("\n")
    const sniffer = createAnthropicSseUsageSniffer()
    for (const chunk of byteChunks(sse, 13)) sniffer.feed(chunk)
    expect(sniffer.finish()).toBeNull()
  })

  it("a newer-API cumulative repeat on message_delta wins over message_start — last field-wise value wins", () => {
    const sse = [
      "event: message_start",
      d({ type: "message_start", message: { usage: { input_tokens: 10, cache_read_input_tokens: 2 } } }),
      "",
      "event: message_delta",
      d({
        type: "message_delta",
        delta: { stop_reason: "end_turn" },
        usage: { output_tokens: 7, input_tokens: 12, cache_read_input_tokens: 4 },
      }),
      "",
    ].join("\n")
    const sniffer = createAnthropicSseUsageSniffer()
    for (const chunk of byteChunks(sse, 23)) sniffer.feed(chunk)
    expect(sniffer.finish()).toEqual({
      promptTokens: 16, // input 12 (last-wins) + cache_read 4 (last-wins) + cache_creation 0 (never sent)
      completionTokens: 7,
      cacheReadInputTokens: 4,
      cacheCreationInputTokens: 0,
    })
  })

  it("tolerates a malformed data line and keeps listening for the next event", () => {
    const sniffer = createAnthropicSseUsageSniffer()
    sniffer.feed(new TextEncoder().encode('event: message_start\ndata: {"usage": broken\n'))
    sniffer.feed(
      new TextEncoder().encode(
        "event: message_delta\n" + d({ type: "message_delta", usage: { output_tokens: 4 } }) + "\n",
      ),
    )
    expect(sniffer.finish()).toEqual({
      promptTokens: 0,
      completionTokens: 4,
      cacheReadInputTokens: 0,
      cacheCreationInputTokens: 0,
    })
  })

  it("skips an over-long line without throwing and keeps listening for the next event", () => {
    const sniffer = createAnthropicSseUsageSniffer()
    const huge = "event: content_block_delta\ndata: " + "y".repeat(300 * 1024)
    expect(() => sniffer.feed(new TextEncoder().encode(huge))).not.toThrow()
    expect(sniffer.finish()).toBeNull()
    sniffer.feed(
      new TextEncoder().encode(
        "\nevent: message_delta\ndata: " + JSON.stringify({ usage: { output_tokens: 4 } }) + "\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
      ),
    )
    expect(sniffer.finish()).toMatchObject({ completionTokens: 4 })
    expect(sniffer.complete()).toBe(true)
  })
})

describe("createOpenAISseUsageSniffer — complete()", () => {
  it("is false before anything is fed", () => {
    expect(createOpenAISseUsageSniffer().complete()).toBe(false)
  })

  it("[DONE] marks the stream complete", () => {
    const sniffer = createOpenAISseUsageSniffer()
    sniffer.feed(new TextEncoder().encode('data: {"choices":[{"delta":{"content":"hi"}}]}\n\ndata: [DONE]\n\n'))
    expect(sniffer.complete()).toBe(true)
  })

  it("a chunk carrying a real finish_reason marks it complete even without a literal [DONE]", () => {
    const sniffer = createOpenAISseUsageSniffer()
    sniffer.feed(
      new TextEncoder().encode(
        d({ choices: [{ index: 0, delta: {}, finish_reason: "stop" }] }) + "\n\n",
      ),
    )
    expect(sniffer.complete()).toBe(true)
  })

  it("an intermediate chunk with finish_reason: null does not mark it complete", () => {
    const sniffer = createOpenAISseUsageSniffer()
    sniffer.feed(
      new TextEncoder().encode(
        d({ choices: [{ index: 0, delta: { content: "hi" }, finish_reason: null }] }) + "\n\n",
      ),
    )
    expect(sniffer.complete()).toBe(false)
  })

  it("no completion signal at all leaves it incomplete", () => {
    const sniffer = createOpenAISseUsageSniffer()
    sniffer.feed(new TextEncoder().encode(d({ choices: [{ delta: { content: "hi" } }] }) + "\n\n"))
    expect(sniffer.complete()).toBe(false)
  })

  it("an over-long non-terminal line neither completes the stream nor undoes a completion already seen", () => {
    const sniffer = createOpenAISseUsageSniffer()
    const huge = "data: " + "x".repeat(300 * 1024)
    sniffer.feed(new TextEncoder().encode(huge + "\n"))
    expect(sniffer.complete()).toBe(false)
    sniffer.feed(new TextEncoder().encode("data: [DONE]\n\n"))
    expect(sniffer.complete()).toBe(true)
    sniffer.feed(new TextEncoder().encode(huge))
    expect(sniffer.complete()).toBe(true)
  })
})

describe("createAnthropicSseUsageSniffer — complete()", () => {
  it("is false before anything is fed", () => {
    expect(createAnthropicSseUsageSniffer().complete()).toBe(false)
  })

  it("message_stop marks the stream complete, even with no usage anywhere", () => {
    const sniffer = createAnthropicSseUsageSniffer()
    sniffer.feed(
      new TextEncoder().encode(
        "event: message_stop\n" + d({ type: "message_stop" }) + "\n",
      ),
    )
    expect(sniffer.complete()).toBe(true)
    // No usage was ever seen, so finish() is still null — complete() is independent.
    expect(sniffer.finish()).toBeNull()
  })

  it("no message_stop leaves it incomplete even when usage was captured", () => {
    const sniffer = createAnthropicSseUsageSniffer()
    sniffer.feed(
      new TextEncoder().encode(
        "event: message_start\n" +
          d({ type: "message_start", message: { usage: { input_tokens: 5 } } }) +
          "\n",
      ),
    )
    expect(sniffer.complete()).toBe(false)
  })

  it("an over-long line neither completes the stream nor undoes a message_stop already seen", () => {
    const sniffer = createAnthropicSseUsageSniffer()
    const huge = "data: " + "z".repeat(300 * 1024)
    sniffer.feed(new TextEncoder().encode(huge + "\n"))
    expect(sniffer.complete()).toBe(false)
    sniffer.feed(new TextEncoder().encode("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"))
    expect(sniffer.complete()).toBe(true)
    sniffer.feed(new TextEncoder().encode(huge))
    expect(sniffer.complete()).toBe(true)
  })
})
