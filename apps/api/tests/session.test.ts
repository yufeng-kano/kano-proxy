import { describe, expect, it } from "vitest"
import { clearSessionCookie, createSession, timingSafeEqual } from "../src/auth/session"
import type { Env } from "../src/env"

describe("timingSafeEqual", () => {
  it("returns true for identical strings, including empty ones", () => {
    expect(timingSafeEqual("abc123", "abc123")).toBe(true)
    expect(timingSafeEqual("", "")).toBe(true)
  })

  it("returns false when lengths differ", () => {
    expect(timingSafeEqual("abc", "abcd")).toBe(false)
    expect(timingSafeEqual("abcd", "abc")).toBe(false)
  })

  it("returns false for same-length strings that differ anywhere", () => {
    expect(timingSafeEqual("abc123", "abc124")).toBe(false)
    // Mismatch in the first character, not just the last — the comparison
    // must not short-circuit on the first differing byte.
    expect(timingSafeEqual("xbc123", "abc123")).toBe(false)
  })

  it("is case-sensitive", () => {
    expect(timingSafeEqual("ABC", "abc")).toBe(false)
  })
})

describe("clearSessionCookie", () => {
  it("has no Secure attribute when secure is omitted or false", () => {
    expect(clearSessionCookie()).not.toContain("Secure")
    expect(clearSessionCookie(false)).not.toContain("Secure")
  })

  it("appends Secure when secure is true", () => {
    expect(clearSessionCookie(true)).toContain("; Secure")
  })

  it("still clears the cookie (Max-Age=0) regardless of secure", () => {
    expect(clearSessionCookie(true)).toContain("Max-Age=0")
    expect(clearSessionCookie()).toContain("Max-Age=0")
  })
})

describe("createSession cookie Secure flag", () => {
  /** Minimal D1 stub: createSession only ever does prepare().bind().run(). */
  function fakeEnv(): Env {
    const statement = {
      bind: () => statement,
      run: async () => ({ success: true, meta: {} }),
    }
    const db = { prepare: () => statement } as unknown as D1Database
    return { DB: db, SESSION_SECRET: "test-secret-not-real" } as unknown as Env
  }

  it("omits Secure when opts is not passed", async () => {
    const { cookie } = await createSession(fakeEnv(), "user_1")
    expect(cookie).not.toContain("Secure")
  })

  it("omits Secure when opts.secure is false", async () => {
    const { cookie } = await createSession(fakeEnv(), "user_1", { secure: false })
    expect(cookie).not.toContain("Secure")
  })

  it("appends Secure when opts.secure is true", async () => {
    const { cookie } = await createSession(fakeEnv(), "user_1", { secure: true })
    expect(cookie).toContain("; Secure")
    expect(cookie).toMatch(/HttpOnly; SameSite=Lax; Max-Age=\d+; Secure$/)
  })
})
