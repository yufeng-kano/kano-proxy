import { describe, expect, it } from "vitest"
import { parseAnthropicModel, parseModelId } from "../src/utils/model"

describe("parseModelId", () => {
  it("parses provider/model", () => {
    expect(parseModelId("claude-code/claude-opus-5")).toEqual({
      provider: "claude-code",
      upstreamModel: "claude-opus-5",
      raw: "claude-code/claude-opus-5",
    })
  })

  it("rejects bare model", () => {
    expect(parseModelId("claude-opus-5")).toBeNull()
  })

  it("rejects unknown provider", () => {
    expect(parseModelId("openai/gpt-4")).toBeNull()
  })
})

describe("parseAnthropicModel", () => {
  it("requires provider/model like OpenAI surface", () => {
    expect(parseAnthropicModel("claude-code/claude-opus-5")).toEqual({
      provider: "claude-code",
      upstreamModel: "claude-opus-5",
      raw: "claude-code/claude-opus-5",
    })
    expect(parseAnthropicModel("grok/grok-4.5")).toEqual({
      provider: "grok",
      upstreamModel: "grok-4.5",
      raw: "grok/grok-4.5",
    })
  })

  it("rejects bare id", () => {
    expect(parseAnthropicModel("claude-opus-5")).toBeNull()
  })
})
