import { describe, expect, it } from "vitest"
import {
  CODEX_REASONING_REPLAY_TTL_SECONDS,
  codexReasoningReplayCacheKeyForTest,
  codexReasoningReplaySessionKey,
  hashAssistantText,
  readCodexReasoningReplay,
  writeCodexReasoningReplay,
  deleteCodexReasoningReplay,
} from "../src/providers/codex_reasoning_cache"
import {
  codexSseToOpenAIStream,
  collectCodexSse,
  extractCodexReplayItems,
} from "../src/proxy/codex_openai"
import type { Env } from "../src/env"

function mockEnv(store: Map<string, string>): Env {
  return {
    CACHE: {
      async get(key: string, type?: string) {
        const value = store.get(key)
        if (value == null) return null
        return type === "json" ? JSON.parse(value) : value
      },
      async put(key: string, value: string, options?: { expirationTtl?: number }) {
        expect(options?.expirationTtl).toBe(CODEX_REASONING_REPLAY_TTL_SECONDS)
        store.set(key, value)
      },
      async delete(key: string) {
        store.delete(key)
      },
    },
  } as unknown as Env
}

function chunked(text: string, size: number): ReadableStream<Uint8Array> {
  const bytes = new TextEncoder().encode(text)
  return new ReadableStream({
    start(controller) {
      for (let i = 0; i < bytes.length; i += size) controller.enqueue(bytes.subarray(i, i + size))
      controller.close()
    },
  })
}

async function collect(stream: ReadableStream<Uint8Array>): Promise<string> {
  const reader = stream.getReader()
  let output = ""
  for (;;) {
    const { done, value } = await reader.read()
    if (done) break
    output += new TextDecoder().decode(value)
  }
  return output
}

const reasoning = {
  type: "reasoning",
  encrypted_content: "opaque-codex-content",
  summary: [],
}
const functionCall = {
  type: "function_call",
  call_id: "call_1",
  name: "Read",
  arguments: "{\"path\":\"/tmp/a\"}",
}
const customToolCall = {
  type: "custom_tool_call",
  call_id: "call_2",
  name: "custom",
  input: "value",
}

function completedSse(): string {
  return [
    `data: ${JSON.stringify({ type: "response.output_text.delta", delta: "answer" })}`,
    "",
    `data: ${JSON.stringify({
      type: "response.completed",
      response: {
        output: [
          { type: "message", role: "assistant", content: [{ type: "output_text", text: "answer" }] },
          reasoning,
          functionCall,
          { type: "message", role: "user", content: [{ type: "input_text", text: "ignore" }] },
          customToolCall,
        ],
      },
    })}`,
    "",
  ].join("\n")
}

describe("codexReasoningReplaySessionKey", () => {
  it("prefers conv-id over session-id", () => {
    expect(codexReasoningReplaySessionKey({ convId: "conv", sessionId: "sess" })).toBe("conv")
  })

  it("falls back to session-id and returns null without either", () => {
    expect(codexReasoningReplaySessionKey({ sessionId: "sess" })).toBe("sess")
    expect(codexReasoningReplaySessionKey()).toBeNull()
    expect(codexReasoningReplaySessionKey({})).toBeNull()
  })

  it("falls back to the prompt_cache_key after both affinity ids", () => {
    expect(codexReasoningReplaySessionKey({ sessionId: "sess" }, "pck")).toBe("sess")
    expect(codexReasoningReplaySessionKey({}, " pck ")).toBe("pck")
    expect(codexReasoningReplaySessionKey(undefined, "pck")).toBe("pck")
    expect(codexReasoningReplaySessionKey(undefined, "   ")).toBeNull()
  })
})

describe("codex reasoning replay KV helpers", () => {
  it("round-trips the ordered item array and fingerprint", async () => {
    const store = new Map<string, string>()
    const env = mockEnv(store)
    const entry = {
      items: [reasoning, functionCall, customToolCall],
      assistant_text_hash: await hashAssistantText("answer"),
    }
    await writeCodexReasoningReplay(env, "key1", "gpt-5.2", "sessA", entry)
    expect(await readCodexReasoningReplay(env, "key1", "gpt-5.2", "sessA")).toEqual(entry)
  })

  it("isolates by api key, model, and session", async () => {
    const store = new Map<string, string>()
    const env = mockEnv(store)
    const entry = { items: [reasoning], assistant_text_hash: await hashAssistantText("a") }
    await writeCodexReasoningReplay(env, "key1", "gpt-5.2", "sessA", entry)
    expect(await readCodexReasoningReplay(env, "key2", "gpt-5.2", "sessA")).toBeNull()
    expect(await readCodexReasoningReplay(env, "key1", "gpt-5.1", "sessA")).toBeNull()
    expect(await readCodexReasoningReplay(env, "key1", "gpt-5.2", "sessB")).toBeNull()
    const a = await codexReasoningReplayCacheKeyForTest("key1", "gpt-5.2", "sessA")
    const b = await codexReasoningReplayCacheKeyForTest("key2", "gpt-5.2", "sessA")
    expect(a).not.toBe(b)
    expect(a.startsWith("codex-reasoning-replay:v1:")).toBe(true)
  })

  it("treats a null session key as a no-op", async () => {
    const store = new Map<string, string>()
    const env = mockEnv(store)
    const entry = { items: [reasoning], assistant_text_hash: await hashAssistantText("a") }
    await writeCodexReasoningReplay(env, "key", "model", null, entry)
    expect(store.size).toBe(0)
    expect(await readCodexReasoningReplay(env, "key", "model", null)).toBeNull()
  })

  it("returns null for corrupt JSON and never throws", async () => {
    const store = new Map<string, string>()
    const env = mockEnv(store)
    const key = await codexReasoningReplayCacheKeyForTest("key", "model", "sess")
    store.set(key, "not json")
    await expect(readCodexReasoningReplay(env, "key", "model", "sess")).resolves.toBeNull()
    store.set(key, JSON.stringify({ items: [{ nope: true }], assistant_text_hash: "hash" }))
    await expect(readCodexReasoningReplay(env, "key", "model", "sess")).resolves.toBeNull()
  })

  it("skips an oversized entry without throwing", async () => {
    const store = new Map<string, string>()
    const env = mockEnv(store)
    const item = { type: "reasoning", encrypted_content: "x".repeat(300_000) }
    await writeCodexReasoningReplay(env, "key", "model", "sess", {
      items: [item],
      assistant_text_hash: "hash",
    })
    expect(store.size).toBe(0)
  })

  it("deletes an entry", async () => {
    const store = new Map<string, string>()
    const env = mockEnv(store)
    const entry = { items: [reasoning], assistant_text_hash: "hash" }
    await writeCodexReasoningReplay(env, "key", "model", "sess", entry)
    await deleteCodexReasoningReplay(env, "key", "model", "sess")
    expect(await readCodexReasoningReplay(env, "key", "model", "sess")).toBeNull()
  })
})

describe("Codex replay extraction", () => {
  it("keeps reasoning and tool items in response.output order", () => {
    const output = [
      { type: "message", role: "assistant", content: "answer" },
      functionCall,
      reasoning,
      { type: "message", role: "user", content: "ignore" },
      customToolCall,
    ]
    expect(extractCodexReplayItems(output)).toEqual([functionCall, reasoning, customToolCall])
  })

  it("fires the streaming callback without changing emitted bytes", async () => {
    const sse = completedSse()
    const withoutCallback = await collect(codexSseToOpenAIStream(chunked(sse, 7), "gpt-5.2"))
    const calls: Array<{ items: unknown[]; text: string }> = []
    const withCallback = await collect(
      codexSseToOpenAIStream(chunked(sse, 7), "gpt-5.2", {
        onReplayItems(items, text) {
          calls.push({ items, text })
        },
      }),
    )
    // IDs/timestamps are intentionally normalized because each stream gets a fresh id/time.
    const normalize = (value: string) =>
      value.replace(/chatcmpl_[a-z0-9]+/g, "chatcmpl_ID").replace(/"created":\d+/g, '"created":TIME')
    expect(normalize(withCallback)).toBe(normalize(withoutCallback))
    expect(calls).toEqual([{ items: [reasoning, functionCall, customToolCall], text: "answer" }])
  })

  it("surfaces items and trailing text on the non-stream path", async () => {
    const calls: Array<{ items: unknown[]; text: string }> = []
    const result = await collectCodexSse(chunked(completedSse(), 9), "gpt-5.2", {
      onReplayItems(items, text) {
        calls.push({ items, text })
      },
    })
    expect("error" in result).toBe(false)
    expect(calls).toEqual([{ items: [reasoning, functionCall, customToolCall], text: "answer" }])
  })
})
