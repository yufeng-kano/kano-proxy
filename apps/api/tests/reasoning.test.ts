import { describe, expect, it } from "vitest"
import {
  mapReasoning,
  nearestReasoningEffort,
  parseReasoningEffort,
} from "../src/utils/reasoning"

describe("parseReasoningEffort", () => {
  it("accepts ladder", () => {
    expect(parseReasoningEffort("high")).toBe("high")
    expect(parseReasoningEffort("none")).toBe("none")
    expect(parseReasoningEffort("low")).toBe("low")
    expect(parseReasoningEffort("medium")).toBe("medium")
    expect(parseReasoningEffort("xhigh")).toBe("xhigh")
    expect(parseReasoningEffort("max")).toBe("max")
  })

  it("normalizes case", () => {
    expect(parseReasoningEffort("HIGH")).toBe("high")
    expect(parseReasoningEffort("XHigh")).toBe("xhigh")
  })

  it("treats empty as undefined", () => {
    expect(parseReasoningEffort(undefined)).toBeUndefined()
    expect(parseReasoningEffort(null)).toBeUndefined()
    expect(parseReasoningEffort("")).toBeUndefined()
  })

  it("rejects garbage", () => {
    expect(parseReasoningEffort("ultra")).toBe("invalid")
    expect(parseReasoningEffort(" ")).toBe("invalid")
    expect(parseReasoningEffort(1)).toBe("invalid")
    expect(parseReasoningEffort(true)).toBe("invalid")
    expect(parseReasoningEffort({})).toBe("invalid")
  })
})

describe("mapReasoning", () => {
  it("returns empty when effort is undefined", () => {
    expect(mapReasoning("claude-code", undefined)).toEqual({})
    expect(mapReasoning("codex", undefined)).toEqual({})
    expect(mapReasoning("grok", undefined)).toEqual({})
  })

  it("maps claude none to disabled thinking", () => {
    expect(mapReasoning("claude-code", "none")).toEqual({
      thinking: { type: "disabled" },
      output_config: { effort: "low" },
    })
  })

  it("maps claude high to output_config only", () => {
    expect(mapReasoning("claude-code", "high")).toEqual({
      output_config: { effort: "high" },
    })
  })

  it("maps claude max and xhigh to output_config effort", () => {
    expect(mapReasoning("claude-code", "max")).toEqual({
      output_config: { effort: "max" },
    })
    expect(mapReasoning("claude-code", "xhigh")).toEqual({
      output_config: { effort: "xhigh" },
    })
  })

  it("maps codex none to empty", () => {
    expect(mapReasoning("codex", "none")).toEqual({})
  })

  it("maps codex efforts to reasoning summary auto", () => {
    expect(mapReasoning("codex", "low")).toEqual({
      reasoning: { effort: "low", summary: "auto" },
    })
    expect(mapReasoning("codex", "medium")).toEqual({
      reasoning: { effort: "medium", summary: "auto" },
    })
    expect(mapReasoning("codex", "high")).toEqual({
      reasoning: { effort: "high", summary: "auto" },
    })
  })

  it("clamps codex max to its ceiling xhigh", () => {
    expect(mapReasoning("codex", "max")).toEqual({
      reasoning: { effort: "xhigh", summary: "auto" },
    })
  })

  it("maps grok effort", () => {
    expect(mapReasoning("grok", "medium")).toEqual({ reasoning_effort: "medium" })
    expect(mapReasoning("grok", "none")).toEqual({ reasoning_effort: "none" })
    expect(mapReasoning("grok", "xhigh")).toEqual({ reasoning_effort: "xhigh" })
  })

  it("clamps grok max to its ceiling xhigh", () => {
    expect(mapReasoning("grok", "max")).toEqual({ reasoning_effort: "xhigh" })
  })

  it("maps antigravity effort to a Gemini thinking level", () => {
    expect(mapReasoning("antigravity", "low")).toEqual({
      thinkingConfig: { thinkingLevel: "low" },
    })
    expect(mapReasoning("antigravity", "high")).toEqual({
      thinkingConfig: { thinkingLevel: "high" },
    })
  })

  it("maps antigravity none to a zero budget — thinkingLevel has no off token", () => {
    expect(mapReasoning("antigravity", "none")).toEqual({
      thinkingConfig: { thinkingBudget: 0 },
    })
  })

  it("clamps antigravity above-ceiling efforts to high", () => {
    expect(mapReasoning("antigravity", "xhigh")).toEqual({
      thinkingConfig: { thinkingLevel: "high" },
    })
    expect(mapReasoning("antigravity", "max")).toEqual({
      thinkingConfig: { thinkingLevel: "high" },
    })
  })

  it("leaves efforts at or below the ceiling unclamped", () => {
    expect(mapReasoning("grok", "high")).toEqual({ reasoning_effort: "high" })
    expect(mapReasoning("codex", "xhigh")).toEqual({
      reasoning: { effort: "xhigh", summary: "auto" },
    })
    expect(mapReasoning("claude-code", "max")).toEqual({
      output_config: { effort: "max" },
    })
  })
})

describe("nearestReasoningEffort", () => {
  it("prefers the higher token on equal distance", () => {
    expect(nearestReasoningEffort("high", ["low", "medium", "xhigh"])).toBe("xhigh")
  })

  it("maps high to the only allowed medium", () => {
    expect(nearestReasoningEffort("high", ["medium"])).toBe("medium")
  })

  it("maps max down to xhigh", () => {
    expect(nearestReasoningEffort("max", ["xhigh"])).toBe("xhigh")
  })

  it("maps none up to the nearest low", () => {
    expect(nearestReasoningEffort("none", ["low", "medium"])).toBe("low")
  })

  it("returns identity when rejected is already allowed", () => {
    expect(nearestReasoningEffort("medium", ["medium", "high"])).toBe("medium")
  })

  it("returns undefined for an empty allowed set", () => {
    expect(nearestReasoningEffort("high", [])).toBeUndefined()
  })
})
