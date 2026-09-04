import { describe, expect, it } from "vitest"
import {
  anthropicToGrokResponses,
  collectGrokResponsesSseToAnthropic,
  grokResponsesSseToAnthropicStream,
  resolveGrokThinkingEffort,
} from "../src/proxy/grok_anthropic"
import { fakeGrokEncryptedContent } from "./helpers/grok_sig"

function sse(...events: Array<{ type: string } & Record<string, unknown>>): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder()
  const text = events.map((e) => `data: ${JSON.stringify(e)}\n\n`).join("")
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

describe("resolveGrokThinkingEffort", () => {
  it("disables reasoning when thinking.type=disabled — effort ignored", () => {
    expect(
      resolveGrokThinkingEffort({
        thinking: { type: "disabled" },
        output_config: { effort: "high" },
        reasoning_effort: "xhigh",
      }),
    ).toEqual({ thinkingMode: "disabled", effort: undefined })
  })

  it("maps adaptive + output_config.effort", () => {
    expect(
      resolveGrokThinkingEffort({
        thinking: { type: "adaptive" },
        output_config: { effort: "high" },
      }),
    ).toEqual({ thinkingMode: "enabled", effort: "high" })
  })

  it("defaults adaptive without effort to medium", () => {
    expect(
      resolveGrokThinkingEffort({ thinking: { type: "adaptive" } }),
    ).toEqual({ thinkingMode: "enabled", effort: "medium" })
  })

  it("ignores budget_tokens — effort-only", () => {
    expect(
      resolveGrokThinkingEffort({
        thinking: { type: "enabled", budget_tokens: 4096 },
      }),
    ).toEqual({ thinkingMode: "enabled", effort: "medium" })
  })

  it("treats bare effort as enabled", () => {
    expect(
      resolveGrokThinkingEffort({ output_config: { effort: "low" } }),
    ).toEqual({ thinkingMode: "enabled", effort: "low" })
  })
})

describe("anthropicToGrokResponses", () => {
  it("sends include encrypted_content and reasoning.effort for adaptive", () => {
    const { body, thinkingMode } = anthropicToGrokResponses(
      {
        model: "grok-4.5",
        max_tokens: 100,
        thinking: { type: "adaptive" },
        output_config: { effort: "xhigh" },
        messages: [{ role: "user", content: "hi" }],
      },
      { upstreamModel: "grok-4.5" },
    )
    expect(thinkingMode).toBe("enabled")
    expect(body.include).toEqual(["reasoning.encrypted_content"])
    expect(body.reasoning).toEqual({ effort: "xhigh" })
    expect(body.store).toBe(false)
    expect(body.stream).toBe(true)
    expect(body.temperature).toBe(1)
  })

  it("clamps max effort to xhigh", () => {
    const { body } = anthropicToGrokResponses(
      {
        thinking: { type: "adaptive" },
        output_config: { effort: "max" },
        messages: [{ role: "user", content: "hi" }],
      },
      { upstreamModel: "grok-4.5" },
    )
    expect(body.reasoning).toEqual({ effort: "xhigh" })
  })

  it("disabled + explicit effort → no include, no reasoning object", () => {
    const { body, thinkingMode } = anthropicToGrokResponses(
      {
        thinking: { type: "disabled" },
        output_config: { effort: "high" },
        reasoning_effort: "xhigh",
        messages: [{ role: "user", content: "hi" }],
      },
      { upstreamModel: "grok-4.5" },
    )
    expect(thinkingMode).toBe("disabled")
    expect(body.include).toBeUndefined()
    expect(body.reasoning).toBeUndefined()
  })

  it("maps validated thinking.signature to a reasoning input item", () => {
    const sig = fakeGrokEncryptedContent(11)
    const { body } = anthropicToGrokResponses(
      {
        messages: [
          { role: "user", content: "hi" },
          {
            role: "assistant",
            content: [
              { type: "thinking", thinking: "secret plan", signature: sig },
              { type: "text", text: "hello" },
            ],
          },
          { role: "user", content: "again" },
        ],
      },
      { upstreamModel: "grok-4.5" },
    )
    const input = body.input as Array<Record<string, unknown>>
    const reasoning = input.find((i) => i.type === "reasoning")
    expect(reasoning).toMatchObject({
      type: "reasoning",
      encrypted_content: sig,
    })
  })

  it("drops foreign Claude/GPT thinking signatures — never forwards as encrypted_content", () => {
    const { body } = anthropicToGrokResponses(
      {
        messages: [
          {
            role: "assistant",
            content: [
              {
                type: "thinking",
                thinking: "claude",
                signature: "gAAAAABopenai-encrypted-content-blob",
              },
              { type: "text", text: "hi" },
            ],
          },
          { role: "user", content: "next" },
        ],
      },
      {
        upstreamModel: "grok-4.5",
        replayEncryptedContent: null,
      },
    )
    const input = body.input as Array<Record<string, unknown>>
    expect(input.some((i) => i.type === "reasoning")).toBe(false)
  })

  it("drops provider-prefixed signatures", () => {
    const sig = fakeGrokEncryptedContent(12)
    const { body } = anthropicToGrokResponses(
      {
        messages: [
          {
            role: "assistant",
            content: [
              { type: "thinking", thinking: "x", signature: `claude#${sig}` },
              { type: "text", text: "hi" },
            ],
          },
          { role: "user", content: "next" },
        ],
      },
      { upstreamModel: "grok-4.5" },
    )
    const input = body.input as Array<Record<string, unknown>>
    expect(input.some((i) => i.type === "reasoning")).toBe(false)
  })

  it("injects replay encrypted_content when client omitted signature", () => {
    const replay = fakeGrokEncryptedContent(13)
    const { body } = anthropicToGrokResponses(
      {
        messages: [
          { role: "user", content: "hi" },
          {
            role: "assistant",
            content: [{ type: "text", text: "hello" }],
          },
          { role: "user", content: "again" },
        ],
      },
      {
        upstreamModel: "grok-4.5",
        replayEncryptedContent: replay,
      },
    )
    const input = body.input as Array<Record<string, unknown>>
    expect(input.some((i) => i.encrypted_content === replay)).toBe(true)
  })

  it("does not invent a signature from unsigned thinking plaintext", () => {
    const { body } = anthropicToGrokResponses(
      {
        messages: [
          {
            role: "assistant",
            content: [
              { type: "thinking", thinking: "unsigned only" },
              { type: "text", text: "hi" },
            ],
          },
          { role: "user", content: "next" },
        ],
      },
      { upstreamModel: "grok-4.5" },
    )
    const input = body.input as Array<Record<string, unknown>>
    expect(input.some((i) => i.type === "reasoning")).toBe(false)
  })

  it("puts system text into instructions", () => {
    const { body } = anthropicToGrokResponses(
      {
        system: "be brief",
        messages: [{ role: "user", content: "hi" }],
      },
      { upstreamModel: "grok-4.5" },
    )
    expect(body.instructions).toBe("be brief")
  })

  it("drops stop_sequences and maps output_format json_schema", () => {
    const { body } = anthropicToGrokResponses(
      {
        messages: [{ role: "user", content: "hi" }],
        stop_sequences: ["END"],
        output_format: {
          type: "json_schema",
          schema: { type: "object", properties: { a: { type: "string" } } },
        },
      },
      { upstreamModel: "grok-4.5" },
    )
    expect(body).not.toHaveProperty("stop")
    expect(body).not.toHaveProperty("stop_sequences")
    expect(body.text).toEqual({
      format: {
        type: "json_schema",
        name: "response",
        schema: { type: "object", properties: { a: { type: "string" } } },
        strict: false,
      },
    })
  })

  it("converts tools and drops server-side Anthropic tools", () => {
    const { body } = anthropicToGrokResponses(
      {
        messages: [{ role: "user", content: "hi" }],
        tools: [
          {
            name: "lookup",
            description: "d",
            input_schema: { type: "object", properties: {} },
          },
          { type: "web_search_20250305", name: "web_search" },
        ],
      },
      { upstreamModel: "grok-4.5" },
    )
    expect(body.tools).toEqual([
      {
        type: "function",
        name: "lookup",
        description: "d",
        parameters: { type: "object", properties: {} },
      },
    ])
  })
})

describe("grokResponsesSseToAnthropicStream", () => {
  it("emits signature_delta from reasoning encrypted_content", async () => {
    const encPre = fakeGrokEncryptedContent(21)
    const encFinal = fakeGrokEncryptedContent(22)
    const stream = grokResponsesSseToAnthropicStream(
      sse(
        {
          type: "response.output_item.added",
          item: { type: "reasoning", encrypted_content: encPre },
        },
        {
          type: "response.reasoning_summary_text.delta",
          delta: "thinking ",
        },
        {
          type: "response.reasoning_summary_text.delta",
          delta: "hard",
        },
        {
          type: "response.output_item.done",
          item: {
            type: "reasoning",
            encrypted_content: encFinal,
            summary: [{ type: "summary_text", text: "" }],
          },
        },
        {
          type: "response.output_text.delta",
          delta: "answer",
        },
        {
          type: "response.completed",
          response: {
            usage: { input_tokens: 10, output_tokens: 5 },
          },
        },
      ),
      "grok/grok-4.5",
    )
    const out = await readSse(stream)
    expect(out).toContain('"type":"thinking"')
    expect(out).toContain('"type":"thinking_delta"')
    expect(out).toContain('"thinking":"thinking "')
    expect(out).toContain('"type":"signature_delta"')
    expect(out).toContain(`"signature":"${encFinal}"`)
    expect(out).not.toContain(encPre)
    expect(out).toContain('"type":"text_delta"')
    expect(out).toContain('"text":"answer"')
  })

  it("defers function_call until reasoning.done so the final signature is kept", async () => {
    // Reproduces Claude Code fork/subagent failure: cli-chat-proxy often emits
    // function_call before reasoning.output_item.done. Closing thinking early
    // either drops the signature or emits the preliminary blob; the child then
    // replays it and xAI returns "Could not decode the compaction blob".
    const encPre = fakeGrokEncryptedContent(31)
    const encFinal = fakeGrokEncryptedContent(32)
    const stream = grokResponsesSseToAnthropicStream(
      sse(
        {
          type: "response.output_item.added",
          item: { type: "reasoning", encrypted_content: encPre },
        },
        {
          type: "response.reasoning_summary_text.delta",
          delta: "spawn worker",
        },
        {
          type: "response.output_item.added",
          item: {
            type: "function_call",
            id: "fc_1",
            call_id: "call_1",
            name: "Agent",
          },
        },
        {
          type: "response.function_call_arguments.delta",
          item_id: "fc_1",
          delta: '{"prompt":"explore"}',
        },
        {
          type: "response.output_item.done",
          item: {
            type: "function_call",
            id: "fc_1",
            call_id: "call_1",
            name: "Agent",
            arguments: '{"prompt":"explore"}',
          },
        },
        {
          type: "response.output_item.done",
          item: {
            type: "reasoning",
            encrypted_content: encFinal,
            summary: [{ type: "summary_text", text: "" }],
          },
        },
        {
          type: "response.completed",
          response: { usage: { input_tokens: 10, output_tokens: 20 } },
        },
      ),
      "grok/grok-4.5",
    )
    const out = await readSse(stream)
    const sigIdx = out.indexOf(`"signature":"${encFinal}"`)
    const toolIdx = out.indexOf('"type":"tool_use"')
    expect(sigIdx).toBeGreaterThan(-1)
    expect(toolIdx).toBeGreaterThan(-1)
    expect(sigIdx).toBeLessThan(toolIdx)
    expect(out).not.toContain(encPre)
    expect(out).toContain('"name":"Agent"')
    expect(out).toContain('"stop_reason":"tool_use"')
  })

  it("suppresses thinking blocks when thinkingMode=disabled", async () => {
    const stream = grokResponsesSseToAnthropicStream(
      sse(
        {
          type: "response.reasoning_summary_text.delta",
          delta: "should hide",
        },
        { type: "response.output_text.delta", delta: "ok" },
        { type: "response.completed", response: {} },
      ),
      "grok/grok-4.5",
      { thinkingMode: "disabled" },
    )
    const out = await readSse(stream)
    expect(out).not.toContain("thinking")
    expect(out).toContain('"text":"ok"')
  })

  it("clears replay state on completed disabled turns", async () => {
    const outcomes: string[] = []
    const stream = grokResponsesSseToAnthropicStream(
      sse(
        { type: "response.output_text.delta", delta: "ok" },
        { type: "response.completed", response: {} },
      ),
      "grok/grok-4.5",
      {
        thinkingMode: "disabled",
        onTurnOutcome: (o) => outcomes.push(o.kind),
      },
    )
    await readSse(stream)
    expect(outcomes).toEqual(["clear"])
  })

  it("captures encrypted_content for the replay cache callback", async () => {
    const enc = fakeGrokEncryptedContent(23)
    let captured: { encrypted_content: string; assistant_text: string } | null =
      null
    const stream = grokResponsesSseToAnthropicStream(
      sse(
        {
          type: "response.output_item.done",
          item: { type: "reasoning", encrypted_content: enc },
        },
        { type: "response.output_text.delta", delta: "hi there" },
        { type: "response.completed", response: {} },
      ),
      "grok/grok-4.5",
      {
        onTurnOutcome: (o) => {
          if (o.kind === "replayable") captured = o
        },
      },
    )
    await readSse(stream)
    expect(captured).toEqual({
      kind: "replayable",
      encrypted_content: enc,
      assistant_text: "hi there",
    })
  })

  it("emits Anthropic error when upstream ends without response.completed", async () => {
    const stream = grokResponsesSseToAnthropicStream(
      sse({ type: "response.output_text.delta", delta: "partial" }),
      "grok/grok-4.5",
    )
    const out = await readSse(stream)
    expect(out).toContain("event: error")
    expect(out).toContain("stream ended before response.completed")
    expect(out).not.toContain("event: message_stop")
  })
})

describe("collectGrokResponsesSseToAnthropic", () => {
  it("builds a signed thinking block on the non-stream message", async () => {
    const enc = fakeGrokEncryptedContent(24)
    const msg = await collectGrokResponsesSseToAnthropic(
      sse(
        {
          type: "response.output_item.done",
          item: {
            type: "reasoning",
            encrypted_content: enc,
            summary: [{ type: "summary_text", text: "plan" }],
          },
        },
        { type: "response.output_text.delta", delta: "done" },
        {
          type: "response.completed",
          response: { usage: { input_tokens: 3, output_tokens: 2 } },
        },
      ),
      "grok/grok-4.5",
    )
    expect("error" in msg).toBe(false)
    if ("error" in msg) return
    expect(msg.content).toEqual([
      { type: "thinking", thinking: "plan", signature: enc },
      { type: "text", text: "done" },
    ])
  })

  it("returns error for truncated upstream SSE", async () => {
    const msg = await collectGrokResponsesSseToAnthropic(
      sse({ type: "response.output_text.delta", delta: "partial" }),
      "grok/grok-4.5",
    )
    expect(msg).toMatchObject({
      error: { type: "api_error" },
    })
  })
})

describe("anthropicToGrokResponses: output_config.format", () => {
  it("maps the current Anthropic structured-output spelling to Responses text.format", () => {
    const { body } = anthropicToGrokResponses(
      {
        messages: [{ role: "user", content: "hi" }],
        output_config: { format: { type: "json_schema", schema: { type: "object", properties: { b: {} } } } },
      },
      { upstreamModel: "grok-4.5" },
    )
    expect(body.text).toEqual({
      format: { type: "json_schema", name: "response", schema: { type: "object", properties: { b: {} } }, strict: false },
    })
  })
})
