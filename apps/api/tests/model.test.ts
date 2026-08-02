import { describe, expect, it } from "vitest"
import { parseAnthropicModel, parseModelId, splitModelId } from "../src/utils/model"

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

describe("splitModelId", () => {
  it("splits a builtin-shaped id on the first slash", () => {
    expect(splitModelId("claude-code/claude-opus-5")).toEqual({
      prefix: "claude-code",
      upstreamModel: "claude-opus-5",
      raw: "claude-code/claude-opus-5",
    })
  })

  it("splits on the FIRST slash only — a custom slug fronting a namespaced upstream id", () => {
    expect(splitModelId("my-endpoint/org/model-name")).toEqual({
      prefix: "my-endpoint",
      upstreamModel: "org/model-name",
      raw: "my-endpoint/org/model-name",
    })
  })

  it("does not require the prefix to be a known provider", () => {
    expect(splitModelId("some-custom-slug/model")).toEqual({
      prefix: "some-custom-slug",
      upstreamModel: "model",
      raw: "some-custom-slug/model",
    })
  })

  it("rejects a bare id with no slash", () => {
    expect(splitModelId("model")).toBeNull()
  })

  it("rejects an empty upstream model after the slash", () => {
    expect(splitModelId("slug/")).toBeNull()
  })

  it("rejects a leading slash (empty prefix)", () => {
    expect(splitModelId("/model")).toBeNull()
  })

  it("trims surrounding whitespace before splitting", () => {
    expect(splitModelId("  slug/model  ")).toEqual({
      prefix: "slug",
      upstreamModel: "model",
      raw: "slug/model",
    })
  })
})
