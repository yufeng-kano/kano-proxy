import { describe, expect, it } from "vitest"
import { PROVIDERS } from "../src/env"

describe("model listing policy", () => {
  it("providers are only the live subscription pools", () => {
    expect([...PROVIDERS].sort()).toEqual(["antigravity", "claude-code", "codex", "grok"])
  })

  it("client model ids use provider/upstream shape", () => {
    const id = `claude-code/claude-opus-4-6`
    expect(id).toMatch(/^[a-z0-9-]+\/[a-z0-9._-]+$/i)
  })
})
