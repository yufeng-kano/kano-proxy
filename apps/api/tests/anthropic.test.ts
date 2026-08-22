import { describe, expect, it } from "vitest"
import { countTokensLocalMode } from "../src/routes/anthropic"

describe("countTokensLocalMode", () => {
  it("passes claude-code and antigravity through to their real upstream counts", () => {
    expect(countTokensLocalMode("claude-code")).toBeNull()
    expect(countTokensLocalMode("antigravity")).toBeNull()
  })

  it("routes codex to the relay tokenizer", () => {
    expect(countTokensLocalMode("codex")).toBe("relay")
  })

  it("answers grok with the sentinel stub — never a 400, which triggers client probe bursts", () => {
    expect(countTokensLocalMode("grok")).toBe("stub")
  })
})
