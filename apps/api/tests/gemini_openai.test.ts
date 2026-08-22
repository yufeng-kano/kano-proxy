import { describe, expect, it } from "vitest"
import {
  geminiResponseToOpenAI,
  geminiSseToOpenAIStream,
  openaiToGeminiRequest,
} from "../src/proxy/gemini_openai"
import type { ChatCompletionRequest } from "../src/providers/types"

function req(patch: Partial<ChatCompletionRequest>): ChatCompletionRequest {
  return {
    model: "antigravity/gemini-3-flash",
    rawModel: "antigravity/gemini-3-flash",
    upstreamModel: "gemini-3-flash",
    messages: [],
    rawBody: {},
    ...patch,
  }
}

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

function chunks(raw: string): Array<Record<string, unknown>> {
  return raw
    .split("\n\n")
    .map((block) => block.replace(/^data:\s*/, "").trim())
    .filter((data) => data && data !== "[DONE]")
    .map((data) => JSON.parse(data) as Record<string, unknown>)
}

describe("openaiToGeminiRequest", () => {
  it("lifts system messages into systemInstruction and keeps the rest as contents", () => {
    const out = openaiToGeminiRequest(
      req({
        messages: [
          { role: "system", content: "be terse" },
          { role: "user", content: "hi" },
          { role: "assistant", content: "hello" },
          { role: "user", content: "again" },
        ],
      }),
    )
    expect(out.systemInstruction).toEqual({ role: "user", parts: [{ text: "be terse" }] })
    expect(out.contents).toEqual([
      { role: "user", parts: [{ text: "hi" }] },
      { role: "model", parts: [{ text: "hello" }] },
      { role: "user", parts: [{ text: "again" }] },
    ])
  })

  it("round-trips a tool call and its result", () => {
    const out = openaiToGeminiRequest(
      req({
        messages: [
          { role: "user", content: "weather?" },
          {
            role: "assistant",
            content: null,
            tool_calls: [
              {
                id: "call_1",
                type: "function",
                function: { name: "get_weather", arguments: '{"city":"Taipei"}' },
              },
            ],
          },
          { role: "tool", tool_call_id: "call_1", content: '{"temp":30}' },
        ],
      }),
    )
    expect(out.contents[1]).toEqual({
      role: "model",
      parts: [{ functionCall: { id: "call_1", name: "get_weather", args: { city: "Taipei" } } }],
    })
    expect(out.contents[2]).toEqual({
      role: "user",
      parts: [
        {
          functionResponse: {
            id: "call_1",
            name: "get_weather",
            response: { result: { temp: 30 } },
          },
        },
      ],
    })
  })

  it("replays reasoning_content with its reasoning_signature as a signed thought part", () => {
    const out = openaiToGeminiRequest(
      req({
        messages: [
          { role: "user", content: "hi" },
          {
            role: "assistant",
            content: "answer",
            reasoning_content: "thought about it",
            reasoning_signature: "sig-1",
          } as never,
        ],
      }),
    )
    expect(out.contents[1]!.parts![0]).toEqual({
      text: "thought about it",
      thought: true,
      thoughtSignature: "sig-1",
    })
  })

  it("replays a signature-only assistant message as a signed empty thought part", () => {
    // The response side emits reasoning_signature without reasoning_content
    // for Gemini's signature-only thought parts; the echo must survive the
    // round trip or multi-turn thinking loses its required thoughtSignature.
    const out = openaiToGeminiRequest(
      req({
        messages: [
          { role: "user", content: "hi" },
          { role: "assistant", content: "answer", reasoning_signature: "sig-2" } as never,
        ],
      }),
    )
    expect(out.contents[1]!.parts![0]).toEqual({
      text: "",
      thought: true,
      thoughtSignature: "sig-2",
    })
  })

  it("merges consecutive same-role turns — Gemini rejects two user turns in a row", () => {
    const out = openaiToGeminiRequest(
      req({
        messages: [
          { role: "user", content: "a" },
          { role: "user", content: "b" },
        ],
      }),
    )
    expect(out.contents).toEqual([{ role: "user", parts: [{ text: "a" }, { text: "b" }] }])
  })

  it("turns a base64 image_url into an inline part and drops a remote URL", () => {
    const out = openaiToGeminiRequest(
      req({
        messages: [
          {
            role: "user",
            content: [
              { type: "text", text: "what is this" },
              { type: "image_url", image_url: { url: "data:image/png;base64,AAAB" } },
              { type: "image_url", image_url: { url: "https://example.com/a.png" } },
            ],
          },
        ],
      }),
    )
    expect(out.contents[0].parts).toEqual([
      { text: "what is this" },
      { inlineData: { mimeType: "image/png", data: "AAAB" } },
    ])
  })

  it("maps response_format json_schema to responseMimeType + responseSchema", () => {
    const out = openaiToGeminiRequest(
      req({
        response_format: {
          type: "json_schema",
          json_schema: {
            schema: {
              type: "object",
              additionalProperties: false,
              properties: { name: { type: "string", title: "Name" } },
            },
          },
        },
      }),
    )
    expect(out.generationConfig?.responseMimeType).toBe("application/json")
    // additionalProperties / title have no Gemini Schema field and are stripped.
    expect(out.generationConfig?.responseSchema).toEqual({
      type: "object",
      properties: { name: { type: "string" } },
    })
  })

  it("strips propertyNames and any other keyword Gemini's Schema has no field for", () => {
    // The measured production failure (2026-08-22): Claude Code sends
    // `propertyNames`, the backend 400s the whole request naming it, and every
    // tool call through Antigravity failed. The sanitizer is an allowlist for
    // exactly this reason — an unknown keyword must be dropped, not forwarded.
    const out = openaiToGeminiRequest(
      req({
        tools: [
          {
            type: "function",
            function: {
              name: "edit",
              parameters: {
                type: "object",
                propertyNames: { pattern: "^[a-z]+$" },
                uniqueItems: true,
                $comment: "ignore me",
                dependentRequired: { a: ["b"] },
                properties: { path: { type: "string", minLength: 1 } },
                required: ["path"],
              },
            },
          },
        ],
      }),
    )
    const declaration = (out.tools![0]!.functionDeclarations as Array<Record<string, unknown>>)[0]!
    expect(declaration.parameters).toEqual({
      type: "object",
      properties: { path: { type: "string", minLength: 1 } },
      required: ["path"],
    })
  })

  it("keeps the Schema fields Gemini does support", () => {
    const out = openaiToGeminiRequest(
      req({
        tools: [
          {
            type: "function",
            function: {
              name: "search",
              parameters: {
                type: "object",
                properties: {
                  q: { type: "string", pattern: "^.+$", maxLength: 40, example: "hi" },
                  n: { type: "integer", minimum: 1, maximum: 10, format: "int32" },
                  tags: { type: "array", items: { type: "string" }, minItems: 1, maxItems: 5 },
                  mode: { type: "string", enum: ["a", "b"], nullable: true },
                },
                required: ["q"],
                propertyOrdering: ["q", "n"],
                minProperties: 1,
                maxProperties: 4,
              },
            },
          },
        ],
      }),
    )
    const params = (out.tools![0]!.functionDeclarations as Array<Record<string, unknown>>)[0]!
      .parameters as Record<string, unknown>
    expect(Object.keys(params).sort()).toEqual([
      "maxProperties",
      "minProperties",
      "properties",
      "propertyOrdering",
      "required",
      "type",
    ])
    expect((params.properties as Record<string, unknown>).n).toEqual({
      type: "integer",
      minimum: 1,
      maximum: 10,
      format: "int32",
    })
  })

  it("keeps a tool property that shares its name with a stripped schema keyword", () => {
    const out = openaiToGeminiRequest(
      req({
        tools: [
          {
            type: "function",
            function: {
              name: "annotate",
              parameters: {
                type: "object",
                properties: {
                  // Property *names* are not schema keywords — a field the
                  // user happened to call "title" or "default" must survive.
                  title: { type: "string", title: "Label" },
                  default: { type: "boolean" },
                },
                required: ["title"],
              },
            },
          },
        ],
      }),
    )
    const declaration = (out.tools![0]!.functionDeclarations as Array<Record<string, unknown>>)[0]!
    expect(declaration.parameters).toEqual({
      type: "object",
      properties: {
        title: { type: "string" },
        default: { type: "boolean" },
      },
      required: ["title"],
    })
  })

  it("carries a tool call's thought_signature back as the part's thoughtSignature", () => {
    const out = openaiToGeminiRequest(
      req({
        messages: [
          { role: "user", content: "go" },
          {
            role: "assistant",
            content: null,
            tool_calls: [
              {
                id: "call_1",
                type: "function",
                function: { name: "search", arguments: "{}" },
                thought_signature: "sig-fc",
              },
            ],
          } as never,
          { role: "tool", tool_call_id: "call_1", content: "{}" },
        ],
      }),
    )
    expect(out.contents[1]!.parts![0]).toEqual({
      functionCall: { id: "call_1", name: "search", args: {} },
      thoughtSignature: "sig-fc",
    })
  })

  it("maps reasoning_effort to thinkingConfig and asks for the thoughts back", () => {
    expect(openaiToGeminiRequest(req({ reasoning_effort: "medium" })).generationConfig)
      .toMatchObject({ thinkingConfig: { thinkingLevel: "medium", includeThoughts: true } })
  })

  it("clamps an above-ceiling effort down to high", () => {
    expect(openaiToGeminiRequest(req({ reasoning_effort: "max" })).generationConfig)
      .toMatchObject({ thinkingConfig: { thinkingLevel: "high" } })
  })

  it("turns effort none into a zero budget with thoughts off", () => {
    expect(openaiToGeminiRequest(req({ reasoning_effort: "none" })).generationConfig)
      .toMatchObject({ thinkingConfig: { thinkingBudget: 0, includeThoughts: false } })
  })

  it("carries sampling, stop sequences and max_tokens through", () => {
    const out = openaiToGeminiRequest(
      req({ temperature: 0.4, top_p: 0.9, max_tokens: 128, stop: ["END"] }),
    )
    expect(out.generationConfig).toMatchObject({
      temperature: 0.4,
      topP: 0.9,
      maxOutputTokens: 128,
      stopSequences: ["END"],
    })
  })

  it("maps tools and a forced tool_choice", () => {
    const out = openaiToGeminiRequest(
      req({
        tools: [
          {
            type: "function",
            function: {
              name: "search",
              description: "look up",
              parameters: { type: "object", properties: { q: { type: "string" } } },
            },
          },
        ],
        tool_choice: { type: "function", function: { name: "search" } },
      }),
    )
    expect(out.tools).toEqual([
      {
        functionDeclarations: [
          {
            name: "search",
            description: "look up",
            parameters: { type: "object", properties: { q: { type: "string" } } },
          },
        ],
      },
    ])
    expect(out.toolConfig).toEqual({
      functionCallingConfig: { mode: "ANY", allowedFunctionNames: ["search"] },
    })
  })

  it("omits toolConfig entirely when there are no tools", () => {
    expect(openaiToGeminiRequest(req({ tool_choice: "auto" })).toolConfig).toBeUndefined()
  })
})

describe("geminiResponseToOpenAI", () => {
  it("splits thought parts into reasoning_content and maps usage", () => {
    const out = geminiResponseToOpenAI(
      {
        response: {
          candidates: [
            {
              content: {
                role: "model",
                parts: [
                  { text: "thinking about it", thought: true },
                  { text: "the answer" },
                ],
              },
              finishReason: "STOP",
            },
          ],
          usageMetadata: {
            promptTokenCount: 10,
            candidatesTokenCount: 4,
            thoughtsTokenCount: 6,
            cachedContentTokenCount: 3,
            totalTokenCount: 20,
          },
        },
      },
      "antigravity/gemini-3-flash",
    )
    const choice = (out.choices as Array<Record<string, unknown>>)[0]
    const message = choice.message as Record<string, unknown>
    expect(message.content).toBe("the answer")
    expect(message.reasoning_content).toBe("thinking about it")
    expect(choice.finish_reason).toBe("stop")
    expect(out.usage).toEqual({
      prompt_tokens: 10,
      // thoughts are billed output too, so both halves make up completion_tokens
      completion_tokens: 10,
      total_tokens: 20,
      prompt_tokens_details: { cached_tokens: 3 },
      completion_tokens_details: { reasoning_tokens: 6 },
    })
  })

  it("emits tool_calls and overrides finish_reason", () => {
    const out = geminiResponseToOpenAI(
      {
        response: {
          candidates: [
            {
              content: {
                parts: [{ functionCall: { id: "fc1", name: "search", args: { q: "x" } } }],
              },
              finishReason: "STOP",
            },
          ],
        },
      },
      "antigravity/gemini-3-flash",
    )
    const choice = (out.choices as Array<Record<string, unknown>>)[0]
    expect(choice.finish_reason).toBe("tool_calls")
    expect((choice.message as Record<string, unknown>).tool_calls).toEqual([
      { id: "fc1", index: 0, type: "function", function: { name: "search", arguments: '{"q":"x"}' } },
    ])
  })

  it("surfaces the thought signature as reasoning_signature on the message", () => {
    const out = geminiResponseToOpenAI(
      {
        response: {
          candidates: [
            {
              content: {
                parts: [
                  { text: "hmm", thought: true, thoughtSignature: "sig-9" },
                  { text: "done" },
                ],
              },
              finishReason: "STOP",
            },
          ],
        },
      },
      "m",
    )
    const message = (out.choices as Array<Record<string, unknown>>)[0].message as Record<
      string,
      unknown
    >
    expect(message.reasoning_content).toBe("hmm")
    // The opaque signature must be exposed so the client can echo it back —
    // docs/providers.md says it round-trips on both surfaces.
    expect(message.reasoning_signature).toBe("sig-9")
  })

  it("maps a candidate-less safety block to content_filter, not an empty success", () => {
    const out = geminiResponseToOpenAI(
      { response: { promptFeedback: { blockReason: "SAFETY" } } },
      "m",
    )
    const choice = (out.choices as Array<Record<string, unknown>>)[0]
    expect(choice.finish_reason).toBe("content_filter")
    expect((choice.message as Record<string, unknown>).content).toBeNull()
  })

  it("exposes a signature attached to the functionCall part on the tool call", () => {
    const out = geminiResponseToOpenAI(
      {
        response: {
          candidates: [
            {
              content: {
                parts: [
                  {
                    functionCall: { id: "fc1", name: "search", args: {} },
                    thoughtSignature: "sig-fc",
                  },
                ],
              },
              finishReason: "STOP",
            },
          ],
        },
      },
      "m",
    )
    const call = ((out.choices as Array<Record<string, unknown>>)[0].message as {
      tool_calls: Array<Record<string, unknown>>
    }).tool_calls[0]!
    expect(call.thought_signature).toBe("sig-fc")
  })

  it("maps a SAFETY finish to content_filter, not a successful stop", () => {
    const out = geminiResponseToOpenAI(
      { response: { candidates: [{ content: { parts: [] }, finishReason: "SAFETY" }] } },
      "m",
    )
    expect((out.choices as Array<Record<string, unknown>>)[0].finish_reason).toBe("content_filter")
  })

  it("maps MAX_TOKENS to the OpenAI length token", () => {
    const out = geminiResponseToOpenAI(
      { response: { candidates: [{ content: { parts: [{ text: "…" }] }, finishReason: "MAX_TOKENS" }] } },
      "m",
    )
    expect((out.choices as Array<Record<string, unknown>>)[0].finish_reason).toBe("length")
  })
})

describe("geminiSseToOpenAIStream", () => {
  it("assembles text, reasoning and a terminal usage chunk", async () => {
    const raw = await readSse(
      geminiSseToOpenAIStream(
        sse(
          { response: { candidates: [{ content: { parts: [{ text: "think", thought: true }] } }] } },
          { response: { candidates: [{ content: { parts: [{ text: "Hel" }] } }] } },
          { response: { candidates: [{ content: { parts: [{ text: "lo" }] } }] } },
          {
            response: {
              candidates: [{ finishReason: "STOP" }],
              usageMetadata: { promptTokenCount: 5, candidatesTokenCount: 2, totalTokenCount: 7 },
            },
          },
        ),
        "antigravity/gemini-3-flash",
      ),
    )
    const parsed = chunks(raw)
    expect(raw.endsWith("data: [DONE]\n\n")).toBe(true)
    const deltas = parsed.map((c) => (c.choices as Array<Record<string, unknown>>)[0].delta)
    expect(deltas[0]).toMatchObject({ role: "assistant", reasoning_content: "think" })
    expect(deltas[1]).toMatchObject({ content: "Hel" })
    expect(deltas[2]).toMatchObject({ content: "lo" })
    const final = parsed.at(-1)!
    expect((final.choices as Array<Record<string, unknown>>)[0].finish_reason).toBe("stop")
    expect(final.usage).toMatchObject({ prompt_tokens: 5, completion_tokens: 2 })
  })

  it("finishes as tool_calls once a functionCall streamed", async () => {
    const raw = await readSse(
      geminiSseToOpenAIStream(
        sse(
          {
            response: {
              candidates: [
                { content: { parts: [{ functionCall: { name: "search", args: { q: "x" } } }] } },
              ],
            },
          },
          { response: { candidates: [{ finishReason: "STOP" }] } },
        ),
        "m",
      ),
    )
    const parsed = chunks(raw)
    const toolDelta = (parsed[0].choices as Array<Record<string, unknown>>)[0]
      .delta as Record<string, unknown>
    expect((toolDelta.tool_calls as Array<Record<string, unknown>>)[0]).toMatchObject({
      index: 0,
      function: { name: "search", arguments: '{"q":"x"}' },
    })
    expect((parsed.at(-1)!.choices as Array<Record<string, unknown>>)[0].finish_reason).toBe(
      "tool_calls",
    )
  })

  it("streams the thought signature as a reasoning_signature delta", async () => {
    const raw = await readSse(
      geminiSseToOpenAIStream(
        sse(
          {
            response: {
              candidates: [
                { content: { parts: [{ text: "think", thought: true, thoughtSignature: "sig-1" }] } },
              ],
            },
          },
          { response: { candidates: [{ content: { parts: [{ text: "hi" }] }, finishReason: "STOP" }] } },
        ),
        "m",
      ),
    )
    const deltas = chunks(raw).map((c) => (c.choices as Array<Record<string, unknown>>)[0]?.delta)
    expect(deltas[0]).toMatchObject({ reasoning_content: "think", reasoning_signature: "sig-1" })
  })

  it("gives each streamed image its own monotonic index", async () => {
    const imageFrame = (data: string) => ({
      response: {
        candidates: [
          { content: { parts: [{ inlineData: { mimeType: "image/png", data } }] } },
        ],
      },
    })
    const raw = await readSse(
      geminiSseToOpenAIStream(
        sse(imageFrame("AAA1"), imageFrame("AAA2"), {
          response: { candidates: [{ finishReason: "STOP" }] },
        }),
        "m",
      ),
    )
    const indexes = chunks(raw)
      .map((c) => (c.choices as Array<Record<string, unknown>>)[0]?.delta as Record<string, unknown>)
      .filter((d) => Array.isArray(d?.images))
      .map((d) => (d.images as Array<Record<string, unknown>>)[0].index)
    // Clients assembling streamed images by index must not see them all
    // collapse onto slot 0.
    expect(indexes).toEqual([0, 1])
  })

  it("ends an unterminated stream with an error line, never a fabricated stop", async () => {
    // A clean EOF before any candidate reported a finishReason is a truncated
    // response — the catch block never sees it, so the converter must track
    // the terminal frame itself.
    const raw = await readSse(
      geminiSseToOpenAIStream(
        sse({ response: { candidates: [{ content: { parts: [{ text: "par" }] } }] } }),
        "m",
      ),
    )
    expect(raw).not.toContain("[DONE]")
    const last = chunks(raw).at(-1)!
    expect(last.error).toMatchObject({ type: "upstream_error" })
  })

  it("finishes a candidate-less safety block as content_filter", async () => {
    const raw = await readSse(
      geminiSseToOpenAIStream(sse({ response: { promptFeedback: { blockReason: "SAFETY" } } }), "m"),
    )
    expect(raw.endsWith("data: [DONE]\n\n")).toBe(true)
    const last = chunks(raw).at(-1)!
    expect((last.choices as Array<Record<string, unknown>>)[0].finish_reason).toBe("content_filter")
  })

  it("cancels the upstream body when the client cancels the converted stream", async () => {
    let upstreamCancelled = false
    const upstream = new ReadableStream<Uint8Array>({
      // Never closes on its own — only cancellation can end it.
      cancel() {
        upstreamCancelled = true
      },
    })
    const converted = geminiSseToOpenAIStream(upstream, "m")
    await converted.cancel("client went away")
    expect(upstreamCancelled).toBe(true)
  })

  it("does not drain upstream ahead of client demand", async () => {
    const encoder = new TextEncoder()
    let served = 0
    const upstream = new ReadableStream<Uint8Array>({
      pull(controller) {
        if (served >= 20) {
          controller.close()
          return
        }
        const frame = {
          response: { candidates: [{ content: { parts: [{ text: `chunk-${served}` }] } }] },
        }
        served++
        controller.enqueue(encoder.encode(`data: ${JSON.stringify(frame)}\n\n`))
      },
    })
    const reader = geminiSseToOpenAIStream(upstream, "m").getReader()
    await reader.read()
    // Let any stray eager pumping run — the pull-driven pump must be waiting
    // on downstream demand, not buffering the remaining generation.
    await new Promise((resolve) => setTimeout(resolve, 10))
    expect(served).toBeLessThan(6)
    await reader.cancel()
  })
})
