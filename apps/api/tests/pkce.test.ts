import { describe, expect, it } from "vitest"
import { buildPkcePair, parseCodeHashState } from "../src/auth/pkce"

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
})
