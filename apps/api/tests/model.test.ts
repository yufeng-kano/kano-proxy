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
  it("allows bare id as claude-code", () => {
    expect(parseAnthropicModel("claude-opus-5")).toEqual({
      provider: "claude-code",
      upstreamModel: "claude-opus-5",
      raw: "claude-code/claude-opus-5",
    })
  })
})
