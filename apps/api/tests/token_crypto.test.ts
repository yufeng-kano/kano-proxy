import { describe, expect, it } from "vitest"
import { decryptJson, encryptJson } from "../src/crypto/token_crypto"

/** Fake 32-byte key as standard base64 (not a real secret). */
const FAKE_KEY_B64 = btoa(String.fromCharCode(...Array.from({ length: 32 }, (_, i) => i + 1)))

/** Fake non-base64 utf-8 string; importKey SHA-256 hashes it to 32 bytes. */
const FAKE_KEY_STRING = "test-token-encryption-key-not-secret"

describe("encryptJson / decryptJson", () => {
  it("roundtrips objects with a base64 key", async () => {
    const value = { access_token: "at_fake", refresh_token: "rt_fake", exp: 123 }
    const blob = await encryptJson(FAKE_KEY_B64, value)
    expect(typeof blob).toBe("string")
    expect(blob.length).toBeGreaterThan(16)
    const out = await decryptJson<typeof value>(FAKE_KEY_B64, blob)
    expect(out).toEqual(value)
  })

  it("roundtrips with a raw utf-8 fake key string", async () => {
    const value = { nested: { a: 1 }, list: ["x", "y"] }
    const blob = await encryptJson(FAKE_KEY_STRING, value)
    const out = await decryptJson<typeof value>(FAKE_KEY_STRING, blob)
    expect(out).toEqual(value)
  })

  it("produces different ciphertext for same plaintext (random IV)", async () => {
    const value = { n: 1 }
    const a = await encryptJson(FAKE_KEY_STRING, value)
    const b = await encryptJson(FAKE_KEY_STRING, value)
    expect(a).not.toBe(b)
    expect(await decryptJson(FAKE_KEY_STRING, a)).toEqual(value)
    expect(await decryptJson(FAKE_KEY_STRING, b)).toEqual(value)
  })

  it("throws when encryption key is missing", async () => {
    await expect(encryptJson(undefined, { x: 1 })).rejects.toThrow(
      "TOKEN_ENCRYPTION_KEY is not configured",
    )
    await expect(decryptJson(undefined, "irrelevant")).rejects.toThrow(
      "TOKEN_ENCRYPTION_KEY is not configured",
    )
  })

  it("fails decrypt with wrong key", async () => {
    const blob = await encryptJson(FAKE_KEY_STRING, { ok: true })
    await expect(decryptJson("other-fake-key-not-secret", blob)).rejects.toThrow()
  })
})
