import { describe, expect, it } from "vitest"
import { buildPkcePair, parseCodeHashState, parseCodexCallbackValue } from "../src/auth/pkce"

describe("buildPkcePair", () => {
  it("returns s256-compatible challenge", async () => {
    const { codeVerifier, codeChallenge } = await buildPkcePair()
    expect(codeVerifier.length).toBeGreaterThan(20)
    expect(codeChallenge.length).toBeGreaterThan(20)
    expect(codeChallenge).not.toContain("+")
    expect(codeChallenge).not.toContain("/")
    expect(codeChallenge).not.toContain("=")
  })
})

describe("parse helpers", () => {
  it("parses code#state", () => {
    expect(parseCodeHashState("abc#def")).toEqual({ code: "abc", state: "def" })
  })

  it("parses codex callback URL", () => {
    const u =
      "http://localhost:1455/auth/callback?code=thecode&state=thestate&session_state=x"
    expect(parseCodexCallbackValue(u)).toEqual({ code: "thecode", state: "thestate" })
  })
})
