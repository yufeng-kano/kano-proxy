import { describe, expect, it } from "vitest"
import {
  LOOP_THRESHOLD,
  detectAnthropicToolLoop,
  detectOpenAIToolLoop,
  loopDetectedMessage,
} from "../src/utils/loop_guard"

function anthropicUnit(id: string, name: string, input: unknown): unknown[] {
  return [
    { role: "assistant", content: [{ type: "tool_use", id, name, input }] },
    { role: "user", content: [{ type: "tool_result", tool_use_id: id, content: `result-${id}` }] },
  ]
}

function anthropicRun(n: number, name = "Read", input: unknown = { file_path: "/a" }): unknown[] {
  const out: unknown[] = []
  for (let i = 0; i < n; i++) out.push(...anthropicUnit(`toolu_${i}`, name, input))
  return out
}

function openaiUnit(id: string, name: string, args: string): unknown[] {
  return [
    {
      role: "assistant",
      content: null,
      tool_calls: [{ id, type: "function", function: { name, arguments: args } }],
    },
    { role: "tool", tool_call_id: id, content: `result-${id}` },
  ]
}

function openaiRun(n: number, name = "Read", args = '{"file_path":"/a"}'): unknown[] {
  const out: unknown[] = []
  for (let i = 0; i < n; i++) out.push(...openaiUnit(`call_${i}`, name, args))
  return out
}

describe("LOOP_THRESHOLD", () => {
  it("is 8", () => {
    expect(LOOP_THRESHOLD).toBe(8)
  })
})

describe("detectAnthropicToolLoop", () => {
  it("7 identical consecutive calls does not trip", () => {
    const result = detectAnthropicToolLoop(anthropicRun(7))
    expect(result.tripped).toBe(false)
    expect(result.count).toBe(7)
  })

  it("8 identical consecutive calls trips", () => {
    const result = detectAnthropicToolLoop(anthropicRun(8))
    expect(result.tripped).toBe(true)
    expect(result.count).toBe(8)
    expect(result.name).toBe("Read")
  })

  it("a differing input breaks the trailing run instead of merging into it", () => {
    // 5 + 3 = 8 if (incorrectly) merged across the input boundary — the
    // trailing run must stay bounded at 3 and NOT trip.
    const older = anthropicRun(5, "Read", { file_path: "/other" })
    const trailing = anthropicRun(3, "Read", { file_path: "/a" })
    const result = detectAnthropicToolLoop([...older, ...trailing])
    expect(result.count).toBe(3)
    expect(result.tripped).toBe(false)
  })

  it("a differing tool name also breaks the trailing run", () => {
    const older = anthropicRun(5, "Write", { file_path: "/a" })
    const trailing = anthropicRun(3, "Read", { file_path: "/a" })
    const result = detectAnthropicToolLoop([...older, ...trailing])
    expect(result.count).toBe(3)
  })

  it("an assistant message with two tool_use blocks breaks the run", () => {
    const trailing = anthropicRun(7)
    const malformed = [
      {
        role: "assistant",
        content: [
          { type: "tool_use", id: "a", name: "Read", input: { file_path: "/a" } },
          { type: "tool_use", id: "b", name: "Read", input: { file_path: "/a" } },
        ],
      },
      { role: "user", content: [{ type: "tool_result", tool_use_id: "a", content: "ok" }] },
    ]
    const result = detectAnthropicToolLoop([...malformed, ...trailing])
    expect(result.count).toBe(7)
    expect(result.tripped).toBe(false)
  })

  it("a trailing plain-text turn breaks the run entirely", () => {
    const trailing = anthropicRun(8)
    const result = detectAnthropicToolLoop([...trailing, { role: "user", content: "thanks!" }])
    expect(result.tripped).toBe(false)
    expect(result.count).toBe(0)
  })

  it("tool_result contents differing across repeats does not affect identity", () => {
    // anthropicUnit already gives each tool_result distinct content
    // (`result-${id}`) — this run only trips if content is correctly ignored.
    const result = detectAnthropicToolLoop(anthropicRun(8))
    expect(result.tripped).toBe(true)
  })

  it("accompanying text blocks alongside the single tool_use do not affect identity", () => {
    const messages: unknown[] = []
    for (let i = 0; i < 8; i++) {
      messages.push({
        role: "assistant",
        content: [
          { type: "text", text: `thinking ${i}` },
          { type: "tool_use", id: `t${i}`, name: "Read", input: { file_path: "/a" } },
        ],
      })
      messages.push({
        role: "user",
        content: [{ type: "tool_result", tool_use_id: `t${i}`, content: "ok" }],
      })
    }
    expect(detectAnthropicToolLoop(messages).tripped).toBe(true)
  })

  it("a tool_result for a different tool_use id does not complete the unit", () => {
    const messages = [
      ...anthropicRun(7),
      { role: "assistant", content: [{ type: "tool_use", id: "mismatched", name: "Read", input: { file_path: "/a" } }] },
      { role: "user", content: [{ type: "tool_result", tool_use_id: "someone-else", content: "ok" }] },
    ]
    const result = detectAnthropicToolLoop(messages)
    expect(result.count).toBe(0)
  })

  it("non-array input defaults to an empty object", () => {
    expect(detectAnthropicToolLoop([])).toEqual({ tripped: false, name: null, count: 0 })
  })
})

describe("detectOpenAIToolLoop", () => {
  it("7 identical consecutive calls does not trip", () => {
    const result = detectOpenAIToolLoop(openaiRun(7))
    expect(result.tripped).toBe(false)
    expect(result.count).toBe(7)
  })

  it("8 identical consecutive calls trips", () => {
    const result = detectOpenAIToolLoop(openaiRun(8))
    expect(result.tripped).toBe(true)
    expect(result.count).toBe(8)
    expect(result.name).toBe("Read")
  })

  it("differing arguments break the trailing run instead of merging into it", () => {
    const older = openaiRun(5, "Read", '{"file_path":"/other"}')
    const trailing = openaiRun(3, "Read", '{"file_path":"/a"}')
    const result = detectOpenAIToolLoop([...older, ...trailing])
    expect(result.count).toBe(3)
    expect(result.tripped).toBe(false)
  })

  it("an assistant message with two tool_calls entries breaks the run", () => {
    const trailing = openaiRun(7)
    const malformed = [
      {
        role: "assistant",
        content: null,
        tool_calls: [
          { id: "a", type: "function", function: { name: "Read", arguments: "{}" } },
          { id: "b", type: "function", function: { name: "Read", arguments: "{}" } },
        ],
      },
      { role: "tool", tool_call_id: "a", content: "ok" },
    ]
    const result = detectOpenAIToolLoop([...malformed, ...trailing])
    expect(result.count).toBe(7)
    expect(result.tripped).toBe(false)
  })

  it("a trailing plain-text turn breaks the run entirely", () => {
    const trailing = openaiRun(8)
    const result = detectOpenAIToolLoop([...trailing, { role: "user", content: "thanks!" }])
    expect(result.tripped).toBe(false)
    expect(result.count).toBe(0)
  })

  it("tool result content differing across repeats does not affect identity", () => {
    const result = detectOpenAIToolLoop(openaiRun(8))
    expect(result.tripped).toBe(true)
  })
})

describe("loopDetectedMessage", () => {
  it("formats the shared 400 message text", () => {
    expect(loopDetectedMessage({ tripped: true, name: "Read", count: 8 })).toBe(
      "degenerate tool-call loop detected: Read repeated 8 times with identical input; aborting so the client can recover",
    )
  })
})
