import { describe, expect, it } from "vitest"
import {
  grokReasoningReplayCacheKeyForTest,
  grokReasoningReplaySessionKey,
  hashAssistantText,
  readGrokReasoningReplay,
  writeGrokReasoningReplay,
} from "../src/providers/grok_reasoning_cache"
import type { Env } from "../src/env"
import { fakeGrokEncryptedContent } from "./helpers/grok_sig"

describe("grokReasoningReplaySessionKey", () => {
  it("prefers conv-id over session-id", () => {
    expect(
      grokReasoningReplaySessionKey({
        convId: "conv",
        sessionId: "sess",
      }),
    ).toBe("conv")
  })

  it("falls back to session-id", () => {
    expect(grokReasoningReplaySessionKey({ sessionId: "sess" })).toBe("sess")
  })

  it("returns null when neither is set — never invent", () => {
    expect(grokReasoningReplaySessionKey({})).toBeNull()
    expect(grokReasoningReplaySessionKey()).toBeNull()
  })
})

describe("grok reasoning replay KV helpers", () => {
  function mockEnv(store: Map<string, string>): Env {
    return {
      CACHE: {
        async get(key: string, type?: string) {
          const v = store.get(key)
          if (v == null) return null
          return type === "json" ? JSON.parse(v) : v
        },
        async put(key: string, value: string) {
          store.set(key, value)
        },
        async delete(key: string) {
          store.delete(key)
        },
      },
    } as unknown as Env
  }

  it("round-trips an entry scoped by api key + model + session", async () => {
    const store = new Map<string, string>()
    const env = mockEnv(store)
    const enc = fakeGrokEncryptedContent(31)
    const hash = await hashAssistantText("hello")
    await writeGrokReasoningReplay(env, "key1", "grok-4.5", "sessA", {
      encrypted_content: enc,
      assistant_text_hash: hash,
    })
    const hit = await readGrokReasoningReplay(env, "key1", "grok-4.5", "sessA")
    expect(hit).toEqual({ encrypted_content: enc, assistant_text_hash: hash })
    expect(await readGrokReasoningReplay(env, "key2", "grok-4.5", "sessA")).toBeNull()
    expect(await readGrokReasoningReplay(env, "key1", "grok-4.5", "sessB")).toBeNull()
  })

  it("isolates cache keys by model — model change must not reuse ciphertext", async () => {
    const a = await grokReasoningReplayCacheKeyForTest("key1", "grok-4.5", "sess")
    const b = await grokReasoningReplayCacheKeyForTest("key1", "grok-4.20", "sess")
    expect(a).not.toBe(b)
    expect(a.startsWith("grok-reasoning-replay:v2:")).toBe(true)
  })

  it("refuses to store foreign encrypted_content", async () => {
    const store = new Map<string, string>()
    const env = mockEnv(store)
    await writeGrokReasoningReplay(env, "key1", "grok-4.5", "sessA", {
      encrypted_content: "gAAAAABnot-grok",
      assistant_text_hash: await hashAssistantText("x"),
    })
    expect(store.size).toBe(0)
  })
})
