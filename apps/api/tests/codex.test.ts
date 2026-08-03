import { describe, expect, it } from "vitest"
import { buildCodexRequestBody } from "../src/providers/codex"
import { windowsFromCodexPayload } from "../src/providers/codex_usage"
import { anthropicToOpenAIChatRequest } from "../src/proxy/openai_anthropic"

describe("buildCodexRequestBody", () => {
  it("sends store:false unconditionally", () => {
    const body = buildCodexRequestBody({
      upstreamModel: "gpt-5.2",
      messages: [{ role: "user", content: "hi" }],
    })
    expect(body.store).toBe(false)
  })

  describe("system → instructions", () => {
    it("folds a single system message into instructions and drops it from input", () => {
      const body = buildCodexRequestBody({
        upstreamModel: "gpt-5.2",
        messages: [
          { role: "system", content: "Be terse." },
          { role: "user", content: "hi" },
        ],
      })
      expect(body.instructions).toBe("Be terse.")
      const input = body.input as Array<Record<string, unknown>>
      expect(input).toHaveLength(1)
      expect(input.some((m) => m.role === "system")).toBe(false)
      expect(input[0]).toMatchObject({ role: "user" })
      // No trace of the old "[system]\n" fake-user-message wrapping.
      expect(JSON.stringify(input)).not.toContain("[system]")
    })

    it("joins multiple system messages with a blank line, in order, and drops both from input", () => {
      const body = buildCodexRequestBody({
        upstreamModel: "gpt-5.2",
        messages: [
          { role: "system", content: "First." },
          { role: "user", content: "hi" },
          { role: "system", content: "Second." },
        ],
      })
      expect(body.instructions).toBe("First.\n\nSecond.")
      const input = body.input as Array<Record<string, unknown>>
      expect(input).toHaveLength(1)
      expect(input.some((m) => m.role === "system")).toBe(false)
    })

    it("omits instructions entirely when there are no system messages", () => {
      const body = buildCodexRequestBody({
        upstreamModel: "gpt-5.2",
        messages: [{ role: "user", content: "hi" }],
      })
      expect("instructions" in body).toBe(false)
    })
  })

  describe("tool_choice mapping", () => {
    const tools = [{ type: "function", function: { name: "lookup", parameters: {} } }]

    it("passes auto/none/required through as the same string", () => {
      for (const choice of ["auto", "none", "required"]) {
        const body = buildCodexRequestBody({
          upstreamModel: "m",
          messages: [{ role: "user", content: "hi" }],
          tools,
          tool_choice: choice,
        })
        expect(body.tool_choice).toBe(choice)
      }
    })

    it("maps a named function tool_choice to the Responses flattened shape", () => {
      const body = buildCodexRequestBody({
        upstreamModel: "m",
        messages: [{ role: "user", content: "hi" }],
        tools,
        tool_choice: { type: "function", function: { name: "lookup" } },
      })
      expect(body.tool_choice).toEqual({ type: "function", name: "lookup" })
    })

    it("defaults to auto when tools are present but tool_choice is absent", () => {
      const body = buildCodexRequestBody({
        upstreamModel: "m",
        messages: [{ role: "user", content: "hi" }],
        tools,
      })
      expect(body.tool_choice).toBe("auto")
    })

    it("omits tool_choice (and tools) entirely when there are no tools", () => {
      const body = buildCodexRequestBody({
        upstreamModel: "m",
        messages: [{ role: "user", content: "hi" }],
        tool_choice: "auto",
      })
      expect("tool_choice" in body).toBe(false)
      expect("tools" in body).toBe(false)
    })

    it("treats an empty tools array as no tools", () => {
      const body = buildCodexRequestBody({
        upstreamModel: "m",
        messages: [{ role: "user", content: "hi" }],
        tools: [],
        tool_choice: "auto",
      })
      expect("tool_choice" in body).toBe(false)
      expect("tools" in body).toBe(false)
    })
  })

  describe("prompt_cache_key", () => {
    it("forwards prompt_cache_key when set", () => {
      const body = buildCodexRequestBody({
        upstreamModel: "m",
        messages: [{ role: "user", content: "hi" }],
        prompt_cache_key: "conv-123",
      })
      expect(body.prompt_cache_key).toBe("conv-123")
    })

    it("omits prompt_cache_key when not set", () => {
      const body = buildCodexRequestBody({
        upstreamModel: "m",
        messages: [{ role: "user", content: "hi" }],
      })
      expect("prompt_cache_key" in body).toBe(false)
    })
  })

  describe("reasoning", () => {
    it("sets reasoning when passed, omits it otherwise", () => {
      const withReasoning = buildCodexRequestBody(
        { upstreamModel: "m", messages: [{ role: "user", content: "hi" }] },
        { effort: "high", summary: "auto" },
      )
      expect(withReasoning.reasoning).toEqual({ effort: "high", summary: "auto" })

      const withoutReasoning = buildCodexRequestBody({
        upstreamModel: "m",
        messages: [{ role: "user", content: "hi" }],
      })
      expect("reasoning" in withoutReasoning).toBe(false)
    })
  })

  describe("assistant message ordering", () => {
    it("emits the assistant text message before its function_call items", () => {
      const body = buildCodexRequestBody({
        upstreamModel: "gpt-5.2",
        messages: [
          {
            role: "assistant",
            content: "Let me check that file.",
            tool_calls: [
              {
                id: "call_1",
                type: "function",
                function: { name: "Read", arguments: '{"file_path":"/a"}' },
              },
            ],
          },
        ],
      })
      const input = body.input as Array<Record<string, unknown>>
      expect(input).toHaveLength(2)
      expect(input[0]).toEqual({
        role: "assistant",
        content: [{ type: "output_text", text: "Let me check that file." }],
      })
      expect(input[1]).toEqual({
        type: "function_call",
        call_id: "call_1",
        name: "Read",
        arguments: '{"file_path":"/a"}',
      })
    })

    it("keeps only function_call items when there is no assistant text", () => {
      const body = buildCodexRequestBody({
        upstreamModel: "gpt-5.2",
        messages: [
          {
            role: "assistant",
            content: null,
            tool_calls: [
              { id: "call_1", type: "function", function: { name: "A", arguments: "{}" } },
              { id: "call_2", type: "function", function: { name: "B", arguments: "{}" } },
            ],
          },
        ],
      })
      const input = body.input as Array<Record<string, unknown>>
      expect(input).toHaveLength(2)
      expect(input.every((i) => i.type === "function_call")).toBe(true)
    })

    it("emits only the text message when there are no tool_calls", () => {
      const body = buildCodexRequestBody({
        upstreamModel: "gpt-5.2",
        messages: [{ role: "assistant", content: "just text" }],
      })
      const input = body.input as Array<Record<string, unknown>>
      expect(input).toEqual([
        { role: "assistant", content: [{ type: "output_text", text: "just text" }] },
      ])
    })
  })
})

describe("Anthropic system → codex instructions (via anthropicToOpenAIChatRequest)", () => {
  it("ends up in the Responses instructions field, not input", () => {
    const converted = anthropicToOpenAIChatRequest({
      system: "You are a helpful assistant.",
      messages: [{ role: "user", content: "hi" }],
    })
    const body = buildCodexRequestBody({
      upstreamModel: "gpt-5.2",
      messages: converted.messages,
    })
    expect(body.instructions).toBe("You are a helpful assistant.")
    const input = body.input as Array<Record<string, unknown>>
    expect(input.some((m) => m.role === "system")).toBe(false)
  })

  it("joins multi-block Anthropic system content the same way", () => {
    const converted = anthropicToOpenAIChatRequest({
      system: [
        { type: "text", text: "Part one." },
        { type: "text", text: "Part two." },
      ],
      messages: [{ role: "user", content: "hi" }],
    })
    const body = buildCodexRequestBody({
      upstreamModel: "gpt-5.2",
      messages: converted.messages,
    })
    expect(body.instructions).toBe("Part one.\n\nPart two.")
  })
})

describe("codex ignores reasoning_content on replayed history", () => {
  it("a history thinking block converts to reasoning_content, which the Responses input builder silently ignores", () => {
    const converted = anthropicToOpenAIChatRequest({
      messages: [
        {
          role: "assistant",
          content: [
            { type: "thinking", thinking: "reasoning from a prior turn" },
            { type: "text", text: "here is the answer" },
          ],
        },
        { role: "user", content: "thanks" },
      ],
    })
    const assistantMsg = converted.messages[0] as Record<string, unknown>
    expect(assistantMsg.reasoning_content).toBe("reasoning from a prior turn")

    const body = buildCodexRequestBody({
      upstreamModel: "gpt-5.2",
      messages: converted.messages,
    })
    const serialized = JSON.stringify(body)
    expect(serialized).not.toContain("reasoning_content")
    expect(serialized).not.toContain("reasoning from a prior turn")
    // The rest of that assistant turn is still built normally.
    const input = body.input as Array<Record<string, unknown>>
    expect(input[0]).toEqual({
      role: "assistant",
      content: [{ type: "output_text", text: "here is the answer" }],
    })
  })
})

/**
 * Regression coverage for the `UsageWindow.utilization` scale contract
 * (percent 0–100, never a 0–1 fraction — see `UsageWindow` in
 * src/providers/types.ts). The admin UI once showed 100% for an account
 * actually at 1% because a frontend heuristic rescaled any value <= 1; that
 * heuristic is gone, so this locks the adapter's window-mapping in place.
 * `windowsFromCodexPayload` is a pure function — no fetch stubbing needed.
 */
describe("windowsFromCodexPayload — window mapping and the utilization scale contract", () => {
  it("REGRESSION: used_percent = 1 (meaning 1%) must produce utilization === 1 exactly — this is the exact value that a removed frontend heuristic once rescaled to 100%", () => {
    const windows = windowsFromCodexPayload({
      rate_limit: {
        primary_window: { used_percent: 1, limit_window_seconds: 18000, reset_at: 1_780_000_000 },
      },
    })
    expect(windows[0]!.utilization).toBe(1)
  })

  it("a mid-range percent (73) passes through unchanged", () => {
    const windows = windowsFromCodexPayload({
      rate_limit: { primary_window: { used_percent: 73, limit_window_seconds: 18000 } },
    })
    expect(windows[0]!.utilization).toBe(73)
  })

  it("100 (fully used) passes through unchanged", () => {
    const windows = windowsFromCodexPayload({
      rate_limit: { primary_window: { used_percent: 100, limit_window_seconds: 604800 } },
    })
    expect(windows[0]!.utilization).toBe(100)
  })

  it("a window with no used_percent maps to utilization: null, not 0", () => {
    const windows = windowsFromCodexPayload({
      rate_limit: { primary_window: { limit_window_seconds: 18000 } },
    })
    expect(windows[0]!.utilization).toBeNull()
  })

  it("reset_at (unix seconds) converts to an ISO string; a window with no reset_at maps resets_at to null", () => {
    const windows = windowsFromCodexPayload({
      rate_limit: {
        primary_window: { used_percent: 10, limit_window_seconds: 18000, reset_at: 1_735_689_600 },
        secondary_window: { used_percent: 20, limit_window_seconds: 604800 },
      },
    })
    expect(windows[0]!.resets_at).toBe(new Date(1_735_689_600 * 1000).toISOString())
    expect(windows[1]!.resets_at).toBeNull()
  })

  it("labels derive from limit_window_seconds: 604800 -> Week, 18000 -> 5h, whole hour/day values, unknown -> Ns, absent -> 'window'", () => {
    expect(
      windowsFromCodexPayload({
        rate_limit: { primary_window: { used_percent: 1, limit_window_seconds: 604800 } },
      })[0]!.label,
    ).toBe("Week")
    expect(
      windowsFromCodexPayload({
        rate_limit: { primary_window: { used_percent: 1, limit_window_seconds: 18000 } },
      })[0]!.label,
    ).toBe("5h")
    expect(
      windowsFromCodexPayload({
        rate_limit: { primary_window: { used_percent: 1, limit_window_seconds: 3600 } },
      })[0]!.label,
    ).toBe("1h")
    expect(
      windowsFromCodexPayload({
        rate_limit: { primary_window: { used_percent: 1, limit_window_seconds: 172800 } },
      })[0]!.label,
    ).toBe("2d")
    expect(
      windowsFromCodexPayload({
        rate_limit: { primary_window: { used_percent: 1, limit_window_seconds: 7777 } },
      })[0]!.label,
    ).toBe("7777s")
    expect(
      windowsFromCodexPayload({ rate_limit: { primary_window: { used_percent: 1 } } })[0]!.label,
    ).toBe("window")
  })

  it("both primary_window and secondary_window map to two windows, in order", () => {
    const windows = windowsFromCodexPayload({
      rate_limit: {
        primary_window: { used_percent: 5, limit_window_seconds: 18000 },
        secondary_window: { used_percent: 6, limit_window_seconds: 604800 },
      },
    })
    expect(windows).toHaveLength(2)
    expect(windows[0]).toMatchObject({ label: "5h", utilization: 5 })
    expect(windows[1]).toMatchObject({ label: "Week", utilization: 6 })
  })

  it("no rate_limit at all maps to an empty windows array", () => {
    expect(windowsFromCodexPayload({})).toEqual([])
  })

  it("an explicit null window (e.g. secondary_window: null) is skipped, not crashed on", () => {
    const windows = windowsFromCodexPayload({
      rate_limit: {
        primary_window: { used_percent: 1, limit_window_seconds: 18000 },
        secondary_window: null,
      },
    })
    expect(windows).toHaveLength(1)
  })
})
