import { describe, expect, it } from "vitest"
import { createApiKeyMaterial, hashApiKey } from "../src/crypto/keys"

describe("api keys", () => {
  it("hashes stably", async () => {
    const { plaintext, hash, prefix } = await createApiKeyMaterial()
    expect(plaintext.startsWith("sk-kano-proxy-")).toBe(true)
    expect(prefix.startsWith("sk-kano-proxy-")).toBe(true)
    expect(await hashApiKey(plaintext)).toBe(hash)
  })

  it("stores a 20-char display prefix — the constant plus 6 distinguishing chars", async () => {
    const { plaintext, prefix } = await createApiKeyMaterial()
    expect(prefix).toHaveLength(20)
    expect(prefix).toBe(plaintext.slice(0, 20))
    // The 6 chars beyond the "sk-kano-proxy-" constant actually distinguish it.
    expect(prefix.slice(14)).toHaveLength(6)
  })

  it("prefixes differ beyond the constant part across two generations", async () => {
    const a = await createApiKeyMaterial()
    const b = await createApiKeyMaterial()
    expect(a.prefix).not.toBe(b.prefix)
    // Both still share the fixed constant — only the distinguishing tail differs.
    expect(a.prefix.slice(0, 14)).toBe("sk-kano-proxy-")
    expect(b.prefix.slice(0, 14)).toBe("sk-kano-proxy-")
    expect(a.prefix.slice(14)).not.toBe(b.prefix.slice(14))
  })
})
