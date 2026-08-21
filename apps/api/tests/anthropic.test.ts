import { describe, expect, it } from "vitest"
import { countTokensProviderError } from "../src/routes/anthropic"

describe("countTokensProviderError", () => {
  it("allows claude-code — no rejection, no upstream call needed at this layer", () => {
    expect(countTokensProviderError("claude-code")).toBeNull()
  })

  it("rejects grok with a 400 Anthropic envelope", () => {
    expect(countTokensProviderError("grok")).toEqual({
      type: "error",
      error: {
        type: "invalid_request_error",
        message: "count_tokens is only supported for claude-code and antigravity models",
      },
    })
  })

  it("rejects codex the same way", () => {
    expect(countTokensProviderError("codex")).toEqual({
      type: "error",
      error: {
        type: "invalid_request_error",
        message: "count_tokens is only supported for claude-code and antigravity models",
      },
    })
  })
})
