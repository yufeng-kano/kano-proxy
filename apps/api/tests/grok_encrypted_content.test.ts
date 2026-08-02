import { describe, expect, it } from "vitest"
import {
  inspectGrokEncryptedContent,
  isValidGrokEncryptedContent,
} from "../src/providers/grok_encrypted_content"
import { fakeGrokEncryptedContent } from "./helpers/grok_sig"

function bytesToUnpaddedB64(bytes: Uint8Array): string {
  let bin = ""
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]!)
  return btoa(bin).replace(/=+$/, "")
}

/** High-entropy payload whose base64 starts with E but decoded[0] != 0x12. */
function fakeEPrefixedGrokCiphertext(): string {
  // Start from a known-valid Grok blob, then force an E-prefix without the
  // Claude 0x12 magic (first base64 char = bits 7..2 of byte0; index 4 = 'E').
  const seed = fakeGrokEncryptedContent(77)
  const pad = seed.length % 4 === 0 ? "" : "=".repeat(4 - (seed.length % 4))
  const bin = atob(seed + pad)
  const bytes = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i)
  bytes[0] = 0x11 // 00010001 → base64 'E', not Claude magic 0x12
  const b64 = bytesToUnpaddedB64(bytes)
  if (!b64.startsWith("E")) {
    throw new Error(`expected E prefix, got ${b64[0]}`)
  }
  if (!isValidGrokEncryptedContent(b64)) {
    throw new Error(`E-prefixed Grok fixture rejected: ${inspectGrokEncryptedContent(b64)}`)
  }
  return b64
}

/** Classic Claude E-form: decoded payload starts with protobuf magic 0x12. */
function fakeClaudeEFormThinkingSignature(): string {
  const bytes = new Uint8Array(48)
  bytes[0] = 0x12
  for (let i = 1; i < bytes.length; i++) {
    bytes[i] = (Math.imul(i + 9, 1664525) + 1013904223) >>> 0 & 0xff
  }
  const b64 = bytesToUnpaddedB64(bytes)
  if (!b64.startsWith("E")) {
    throw new Error(`expected Claude E-form to start with E, got ${b64[0]}`)
  }
  return b64
}

describe("isValidGrokEncryptedContent", () => {
  it("accepts high-entropy unpadded base64", () => {
    expect(isValidGrokEncryptedContent(fakeGrokEncryptedContent(9))).toBe(true)
  })

  it("accepts E-prefixed high-entropy Grok ciphertext that is not a Claude envelope", () => {
    const sig = fakeEPrefixedGrokCiphertext()
    expect(sig.startsWith("E")).toBe(true)
    expect(inspectGrokEncryptedContent(sig)).toBeNull()
    expect(isValidGrokEncryptedContent(sig)).toBe(true)
  })

  it("rejects GPT gAAAA envelopes", () => {
    expect(inspectGrokEncryptedContent("gAAAAABopenai-encrypted-content-blob")).toBe(
      "gpt_envelope",
    )
  })

  it("rejects provider cache prefixes", () => {
    const body = fakeGrokEncryptedContent(2)
    expect(inspectGrokEncryptedContent(`claude#${body}`)).toBe("provider_prefix")
    expect(inspectGrokEncryptedContent(`gemini#${body}`)).toBe("provider_prefix")
    expect(inspectGrokEncryptedContent(`openai#${body}`)).toBe("provider_prefix")
  })

  it("rejects Claude E-form thinking signatures (0x12 magic)", () => {
    const eForm = fakeClaudeEFormThinkingSignature()
    expect(inspectGrokEncryptedContent(eForm)).toBe("claude_thinking")
    expect(isValidGrokEncryptedContent(eForm)).toBe(false)
  })

  it("rejects empty, whitespace, and padded base64", () => {
    expect(inspectGrokEncryptedContent("")).toBe("empty")
    expect(inspectGrokEncryptedContent(" abc")).toBe("whitespace")
    expect(inspectGrokEncryptedContent("YWI=")).toBe("padded_base64")
  })

  it("rejects short / low-entropy blobs", () => {
    expect(inspectGrokEncryptedContent("AAAA")).toBe("too_short")
    // Long but low entropy (all 'A's).
    const low = "A".repeat(64)
    expect(inspectGrokEncryptedContent(low)).toBe("low_entropy")
  })
})
