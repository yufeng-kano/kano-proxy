import { describe, expect, it } from "vitest"
import {
  ACCESS_TOKEN_TTL_S,
  mintAccessToken,
  newPairingCode,
  newRefreshToken,
  normalizePairingCode,
  sha256Hex,
  verifyAccessToken,
} from "../src/auth/cli_tokens"

const SECRET = "test-cli-token-secret-not-real"

describe("CLI access tokens", () => {
  it("mints and verifies a round trip", async () => {
    const { token, expiresIn } = await mintAccessToken(SECRET, { userId: "user_1", deviceId: "dev_1" })
    expect(expiresIn).toBe(ACCESS_TOKEN_TTL_S)
    const claims = await verifyAccessToken(SECRET, token)
    expect(claims).toMatchObject({ user_id: "user_1", device_id: "dev_1" })
  })

  it("rejects a tampered payload", async () => {
    const { token } = await mintAccessToken(SECRET, { userId: "user_1", deviceId: "dev_1" })
    const [payload, sig] = token.split(".")
    const forged = btoa(JSON.stringify({ user_id: "user_2", device_id: "dev_1", exp: 9999999999 }))
      .replace(/\+/g, "-")
      .replace(/\//g, "_")
      .replace(/=+$/, "")
    expect(await verifyAccessToken(SECRET, `${forged}.${sig}`)).toBeNull()
    expect(await verifyAccessToken("other-secret", `${payload}.${sig}`)).toBeNull()
  })

  it("rejects an expired token", async () => {
    const past = Date.now() - (ACCESS_TOKEN_TTL_S + 10) * 1000
    const { token } = await mintAccessToken(SECRET, { userId: "user_1", deviceId: "dev_1" }, past)
    expect(await verifyAccessToken(SECRET, token)).toBeNull()
  })

  it("rejects garbage shapes", async () => {
    expect(await verifyAccessToken(SECRET, "")).toBeNull()
    expect(await verifyAccessToken(SECRET, "no-dot")).toBeNull()
    expect(await verifyAccessToken(SECRET, "a.b")).toBeNull()
  })
})

describe("refresh tokens and pairing codes", () => {
  it("refresh tokens are unique and prefixed", () => {
    const a = newRefreshToken()
    const b = newRefreshToken()
    expect(a).toMatch(/^kpr_[A-Za-z0-9_-]{40,}$/)
    expect(a).not.toBe(b)
  })

  it("sha256Hex is deterministic hex", async () => {
    const h = await sha256Hex("abc")
    expect(h).toBe("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
  })

  it("pairing codes format as XXXX-XXXX from the confusion-free alphabet", () => {
    const code = newPairingCode()
    expect(code).toMatch(/^[A-HJ-KM-NP-TV-Z2-9]{4}-[A-HJ-KM-NP-TV-Z2-9]{4}$/)
  })

  it("normalizePairingCode strips separators and uppercases", () => {
    expect(normalizePairingCode(" ab2k-9xyz ")).toBe("AB2K9XYZ")
    expect(normalizePairingCode("AB2K9XYZ")).toBe("AB2K9XYZ")
  })
})
