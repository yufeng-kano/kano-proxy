import { describe, expect, it } from "vitest"
import {
  anthropicToGeminiRequest,
  geminiResponseToAnthropic,
  geminiSseToAnthropicStream,
  InvalidGeminiReasoningEffortError,
  resolveGeminiThinking,
} from "../src/proxy/gemini_anthropic"

function sse(...frames: unknown[]): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder()
  const text = frames.map((f) => `data: ${JSON.stringify(f)}\n\n`).join("")
  return new ReadableStream({
    start(controller) {
      controller.enqueue(encoder.encode(text))
      controller.close()
    },
  })
}

async function readSse(stream: ReadableStream<Uint8Array>): Promise<string> {
  const decoder = new TextDecoder()
  const reader = stream.getReader()
  let out = ""
  for (;;) {
    const { done, value } = await reader.read()
    if (done) break
    out += decoder.decode(value, { stream: true })
  }
  return out
}

/** `[{event, data}]` in emission order. */
function events(raw: string): Array<{ event: string; data: Record<string, unknown> }> {
  return raw
    .split("\n\n")
    .filter((block) => block.trim())
    .map((block) => {
      const event = /^event:\s*(.+)$/m.exec(block)?.[1] ?? ""
      const data = /^data:\s*(.+)$/m.exec(block)?.[1] ?? "{}"
      return { event, data: JSON.parse(data) as Record<string, unknown> }
    })
}

describe("resolveGeminiThinking", () => {
  it("disables thinking outright on thinking.type=disabled", () => {
    expect(resolveGeminiThinking({ thinking: { type: "disabled" } })).toEqual({
      mode: "disabled",
      thinkingConfig: { thinkingBudget: 0, includeThoughts: false },
    })
  })

  it("maps an explicit budget verbatim", () => {
    expect(
      resolveGeminiThinking({ thinking: { type: "enabled", budget_tokens: 4096 } }),
    ).toEqual({
      mode: "enabled",
      thinkingConfig: { thinkingBudget: 4096, includeThoughts: true },
    })
  })

  it("maps output_config.effort to a thinking level", () => {
    expect(resolveGeminiThinking({ output_config: { effort: "low" } })).toEqual({
      mode: "enabled",
      thinkingConfig: { thinkingLevel: "low", includeThoughts: true },
    })
  })

  it("clamps an above-ceiling effort down to high", () => {
    expect(
      resolveGeminiThinking({ output_config: { effort: "xhigh" } }).thinkingConfig,
    ).toMatchObject({ thinkingLevel: "high" })
  })

  it("asks for thoughts by default when nothing was requested", () => {
    expect(resolveGeminiThinking({})).toEqual({
      mode: "default",
      thinkingConfig: { includeThoughts: true },
    })
  })

  it("rejects a garbage effort rather than silently dropping it", () => {
    expect(() => resolveGeminiThinking({ reasoning_effort: "turbo" })).toThrow(
      InvalidGeminiReasoningEffortError,
    )
  })
})

describe("anthropicToGeminiRequest", () => {
  it("moves system to systemInstruction and maps roles", () => {
    const { request } = anthropicToGeminiRequest({
      system: "be terse",
      messages: [
        { role: "user", content: "hi" },
        { role: "assistant", content: [{ type: "text", text: "hello" }] },
      ],
      max_tokens: 64,
    })
    expect(request.systemInstruction).toEqual({ role: "user", parts: [{ text: "be terse" }] })
    expect(request.contents).toEqual([
      { role: "user", parts: [{ text: "hi" }] },
      { role: "model", parts: [{ text: "hello" }] },
    ])
    expect(request.generationConfig).toMatchObject({ maxOutputTokens: 64 })
  })

  it("names a tool_result from the tool_use it answers", () => {
    const { request } = anthropicToGeminiRequest({
      messages: [
        { role: "user", content: "weather?" },
        {
          role: "assistant",
          content: [{ type: "tool_use", id: "toolu_1", name: "get_weather", input: { city: "Taipei" } }],
        },
        {
          role: "user",
          content: [{ type: "tool_result", tool_use_id: "toolu_1", content: "30C" }],
        },
      ],
    })
    expect(request.contents[1].parts).toEqual([
      { functionCall: { id: "toolu_1", name: "get_weather", args: { city: "Taipei" } } },
    ])
    expect(request.contents[2].parts).toEqual([
      { functionResponse: { id: "toolu_1", name: "get_weather", response: { result: "30C" } } },
    ])
  })

  it("marks an errored tool_result as an error, not a result", () => {
    const { request } = anthropicToGeminiRequest({
      messages: [
        {
          role: "user",
          content: [
            { type: "tool_result", tool_use_id: "t1", is_error: true, content: "boom" },
          ],
        },
      ],
    })
    expect(request.contents[0].parts?.[0].functionResponse?.response).toEqual({ error: "boom" })
  })

  it("round-trips a thinking block with its signature", () => {
    const { request } = anthropicToGeminiRequest({
      messages: [
        {
          role: "assistant",
          content: [
            { type: "thinking", thinking: "hmm", signature: "sig-abc" },
            { type: "text", text: "done" },
          ],
        },
      ],
    })
    expect(request.contents[0].parts).toEqual([
      { text: "hmm", thought: true, thoughtSignature: "sig-abc" },
      { text: "done" },
    ])
  })

  it("turns a base64 image block into an inline part", () => {
    const { request } = anthropicToGeminiRequest({
      messages: [
        {
          role: "user",
          content: [
            { type: "image", source: { type: "base64", media_type: "image/jpeg", data: "QUJD" } },
          ],
        },
      ],
    })
    expect(request.contents[0].parts).toEqual([
      { inlineData: { mimeType: "image/jpeg", data: "QUJD" } },
    ])
  })

  it("maps tools, tool_choice and stop_sequences", () => {
    const { request } = anthropicToGeminiRequest({
      messages: [{ role: "user", content: "x" }],
      stop_sequences: ["STOP"],
      tools: [
        {
          name: "search",
          description: "look up",
          input_schema: { type: "object", $schema: "http://json-schema.org/draft-07/schema#" },
        },
        // A server-side tool has no input_schema and no Gemini equivalent.
        { type: "web_search_20250305", name: "web_search" },
      ],
      tool_choice: { type: "tool", name: "search" },
    })
    expect(request.tools).toEqual([
      {
        functionDeclarations: [
          { name: "search", description: "look up", parameters: { type: "object" } },
        ],
      },
    ])
    expect(request.toolConfig).toEqual({
      functionCallingConfig: { mode: "ANY", allowedFunctionNames: ["search"] },
    })
    expect(request.generationConfig).toMatchObject({ stopSequences: ["STOP"] })
  })
})

describe("geminiResponseToAnthropic", () => {
  it("builds thinking, text and tool_use blocks with an Anthropic usage object", () => {
    const out = geminiResponseToAnthropic(
      {
        response: {
          candidates: [
            {
              content: {
                parts: [
                  { text: "reasoning", thought: true, thoughtSignature: "sig" },
                  { text: "answer" },
                  { functionCall: { id: "fc1", name: "search", args: { q: "x" } } },
                ],
              },
              finishReason: "STOP",
            },
          ],
          usageMetadata: {
            promptTokenCount: 12,
            cachedContentTokenCount: 2,
            candidatesTokenCount: 3,
            thoughtsTokenCount: 5,
          },
        },
      },
      "antigravity/gemini-3-flash",
    )
    expect(out.content).toEqual([
      { type: "thinking", thinking: "reasoning", signature: "sig" },
      { type: "text", text: "answer" },
      { type: "tool_use", id: "fc1", name: "search", input: { q: "x" } },
    ])
    expect(out.stop_reason).toBe("tool_use")
    expect(out.usage).toEqual({
      // Gemini's promptTokenCount includes the cached half; Anthropic's does not
      input_tokens: 10,
      cache_read_input_tokens: 2,
      output_tokens: 8,
    })
  })

  it("drops thought parts when the caller disabled thinking", () => {
    const out = geminiResponseToAnthropic(
      {
        response: {
          candidates: [
            { content: { parts: [{ text: "hidden", thought: true }, { text: "shown" }] } },
          ],
        },
      },
      "m",
      { thinkingMode: "disabled" },
    )
    expect(out.content).toEqual([{ type: "text", text: "shown" }])
  })

  it("maps a candidate-less safety block to a refusal, not an empty end_turn", () => {
    const out = geminiResponseToAnthropic(
      { response: { promptFeedback: { blockReason: "SAFETY" } } },
      "m",
    )
    expect(out.stop_reason).toBe("refusal")
    expect(out.content).toEqual([])
  })

  it("maps MAX_TOKENS to the Anthropic stop reason", () => {
    const out = geminiResponseToAnthropic(
      { response: { candidates: [{ content: { parts: [{ text: "…" }] }, finishReason: "MAX_TOKENS" }] } },
      "m",
    )
    expect(out.stop_reason).toBe("max_tokens")
  })
})

describe("geminiSseToAnthropicStream", () => {
  it("emits a well-formed Messages stream for thinking then text", async () => {
    const raw = await readSse(
      geminiSseToAnthropicStream(
        sse(
          {
            response: {
              candidates: [
                { content: { parts: [{ text: "think", thought: true, thoughtSignature: "sig" }] } },
              ],
            },
          },
          { response: { candidates: [{ content: { parts: [{ text: "Hel" }] } }] } },
          { response: { candidates: [{ content: { parts: [{ text: "lo" }] } }] } },
          {
            response: {
              candidates: [{ finishReason: "STOP" }],
              usageMetadata: { promptTokenCount: 9, candidatesTokenCount: 2 },
            },
          },
        ),
        "antigravity/gemini-3-flash",
      ),
    )
    const seq = events(raw)
    expect(seq.map((e) => e.event)).toEqual([
      "message_start",
      "content_block_start",
      "content_block_delta",
      "content_block_delta", // signature_delta closing the thinking block
      "content_block_stop",
      "content_block_start",
      "content_block_delta",
      "content_block_delta",
      "content_block_stop",
      "message_delta",
      "message_stop",
    ])
    expect(seq[3].data.delta).toEqual({ type: "signature_delta", signature: "sig" })
    expect(seq[5].data.content_block).toEqual({ type: "text", text: "" })
    expect(seq.at(-2)!.data).toMatchObject({
      delta: { stop_reason: "end_turn" },
      usage: { input_tokens: 9, output_tokens: 2 },
    })
  })

  it("streams a tool call as a complete input_json_delta and stops as tool_use", async () => {
    const raw = await readSse(
      geminiSseToAnthropicStream(
        sse(
          {
            response: {
              candidates: [
                {
                  content: {
                    parts: [{ functionCall: { id: "t1", name: "search", args: { q: "x" } } }],
                  },
                },
              ],
            },
          },
          { response: { candidates: [{ finishReason: "STOP" }] } },
        ),
        "m",
      ),
    )
    const seq = events(raw)
    const start = seq.find((e) => e.event === "content_block_start")!
    expect(start.data.content_block).toMatchObject({ type: "tool_use", id: "t1", name: "search" })
    const delta = seq.find((e) => e.event === "content_block_delta")!
    expect(delta.data.delta).toEqual({ type: "input_json_delta", partial_json: '{"q":"x"}' })
    expect((seq.at(-2)!.data.delta as Record<string, unknown>).stop_reason).toBe("tool_use")
  })

  it("finishes a candidate-less safety block as a refusal", async () => {
    const raw = await readSse(
      geminiSseToAnthropicStream(sse({ response: { promptFeedback: { blockReason: "SAFETY" } } }), "m"),
    )
    const seq = events(raw)
    expect(seq.at(-1)!.event).toBe("message_stop")
    const delta = seq.find((e) => e.event === "message_delta")!
    expect((delta.data.delta as Record<string, unknown>).stop_reason).toBe("refusal")
  })

  it("cancels the upstream body when the client cancels the converted stream", async () => {
    let upstreamCancelled = false
    const upstream = new ReadableStream<Uint8Array>({
      // Never closes on its own — only cancellation can end it.
      cancel() {
        upstreamCancelled = true
      },
    })
    const converted = geminiSseToAnthropicStream(upstream, "m")
    await converted.cancel("client went away")
    expect(upstreamCancelled).toBe(true)
  })

  it("ends an unterminated stream with an error event, never a fabricated message_stop", async () => {
    // A clean EOF before any candidate reported a finishReason is a truncated
    // response — the catch block never sees it, so the converter must track
    // the terminal frame itself.
    const raw = await readSse(geminiSseToAnthropicStream(sse(), "m"))
    const seq = events(raw)
    expect(seq.map((e) => e.event)).toEqual(["message_start", "error"])
    expect(seq.at(-1)!.data.error).toMatchObject({ type: "api_error" })
  })
})
