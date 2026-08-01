import { describe, expect, it } from "vitest"
import { createApiKeyMaterial, hashApiKey } from "../src/crypto/keys"

describe("api keys", () => {
  it("hashes stably", async () => {
    const { plaintext, hash, prefix } = await createApiKeyMaterial()
    expect(plaintext.startsWith("sk-kano-proxy-")).toBe(true)
    expect(prefix.startsWith("sk-kano-proxy-")).toBe(true)
    expect(await hashApiKey(plaintext)).toBe(hash)
  })
})
