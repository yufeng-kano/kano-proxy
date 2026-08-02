import { describe, expect, it } from "vitest"
import {
  createAnthropicSseUsageSniffer,
  createOpenAISseUsageSniffer,
  fromAnthropicUsage,
  fromOpenAIUsage,
} from "../src/logging/usage_capture"

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

  it("reads prompt_tokens_details.cached_tokens and the cache_creation_input_tokens extension", () => {
    expect(
      fromOpenAIUsage({
        prompt_tokens: 100,
        completion_tokens: 40,
        prompt_tokens_details: { cached_tokens: 20 },
        cache_creation_input_tokens: 6,
      }),
    ).toEqual({
      promptTokens: 100,
      completionTokens: 40,
      cacheReadInputTokens: 20,
      cacheCreationInputTokens: 6,
    })
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

  it("abandons capture on carry overflow without throwing — further feeds become no-ops", () => {
    const sniffer = createOpenAISseUsageSniffer()
    const huge = "data: " + "x".repeat(300 * 1024) // one line, no newline — well past the 256 KiB cap
    expect(() => sniffer.feed(new TextEncoder().encode(huge))).not.toThrow()
    expect(
      () =>
        sniffer.feed(
          new TextEncoder().encode(
            "\n" + d({ choices: [], usage: { prompt_tokens: 9, completion_tokens: 9 } }) + "\n",
          ),
        ),
    ).not.toThrow()
    expect(sniffer.finish()).toBeNull()
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

  it("abandons capture on carry overflow without throwing", () => {
    const sniffer = createAnthropicSseUsageSniffer()
    const huge = "data: " + "y".repeat(300 * 1024)
    expect(() => sniffer.feed(new TextEncoder().encode(huge))).not.toThrow()
    expect(sniffer.finish()).toBeNull()
  })
})
