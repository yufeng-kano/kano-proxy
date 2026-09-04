/**
 * `POST /openai/v1/responses` end to end through the real Hono app:
 * apiKeyAuth, model resolution, the native codex passthrough versus the
 * Responses ↔ Chat conversion path, group mounts, and request logging —
 * every upstream call is a stubbed `fetch` (docs/testing.md).
 */
import { afterEach, describe, expect, it } from "vitest"
import { app } from "../src/index"
import { hashApiKey } from "../src/crypto/keys"
import { encryptJson } from "../src/crypto/token_crypto"
import type { Env } from "../src/env"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

const TOKEN_KEY = "test-token-encryption-key-not-secret"
const API_KEY_PLAINTEXT = "sk-kano-proxy-test-client-key-0001"

const execCtx = {
  waitUntil: (p: Promise<unknown>) => {
    p.catch(() => {})
  },
  passThroughOnException: () => {},
} as unknown as ExecutionContext

function buildEnv(db: FakeD1): Env {
  return {
    DB: db as unknown as D1Database,
    BENCH: fakeKV(),
    CACHE: fakeKV(),
    APP_URL: "https://app.example.com",
    TOKEN_ENCRYPTION_KEY: TOKEN_KEY,
  } as unknown as Env
}

async function seedApiKey(db: FakeD1, userId: string): Promise<void> {
  db.seed("api_keys", [
    {
      id: "key_1",
      user_id: userId,
      name: "test key",
      key_prefix: API_KEY_PLAINTEXT.slice(0, 20),
      key_hash: await hashApiKey(API_KEY_PLAINTEXT),
      created_at: "2026-01-01T00:00:00.000Z",
      last_used_at: null,
    },
  ])
}

async function seedAccount(db: FakeD1, opts: { userId: string; provider: string }): Promise<void> {
  const encrypted = await encryptJson(TOKEN_KEY, { access_token: "upstream-test-token" })
  db.seed("upstream_accounts", [
    {
      id: `acc_${opts.provider}`,
      user_id: opts.userId,
      provider: opts.provider,
      external_account_id: null,
      label: opts.provider,
      priority: 1,
      encrypted_payload: encrypted,
      account_meta_json: null,
      usage_snapshot_json: null,
      usage_fetched_at: null,
      usage_fetching_at: null,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
}

async function seedCustomOpenAI(db: FakeD1, userId: string, slug: string): Promise<void> {
  db.seed("custom_providers", [
    {
      id: `cprov_${slug}`,
      user_id: userId,
      slug,
      name: slug,
      format: "openai",
      base_url: "https://upstream.example.com/v1",
      count_tokens_url: null,
      models_mode: "auto",
      manual_models_json: null,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
  await seedAccount(db, { userId, provider: slug })
}

function seedGroup(db: FakeD1, opts: { userId: string; name: string; targets: string[] }): void {
  const id = `mgrp_${opts.name}`
  db.seed("model_groups", [
    {
      id,
      user_id: opts.userId,
      name: opts.name,
      slug: "team",
      strategy: "ordered",
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
  db.seed("model_group_models", [
    {
      id: `${id}_model_0`,
      user_id: opts.userId,
      group_id: id,
      name: opts.name,
      targets_json: JSON.stringify(opts.targets),
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
}

function authHeaders(): Record<string, string> {
  return { authorization: `Bearer ${API_KEY_PLAINTEXT}`, "content-type": "application/json" }
}

async function drain(body: ReadableStream<Uint8Array> | null): Promise<string> {
  if (!body) return ""
  const reader = body.getReader()
  let out = ""
  for (;;) {
    const { done, value } = await reader.read()
    if (done) break
    out += new TextDecoder().decode(value, { stream: true })
  }
  return out
}

function events(text: string): Array<Record<string, unknown> & { type: string }> {
  return text
    .split("\n")
    .filter((l) => l.startsWith("data:"))
    .map((l) => JSON.parse(l.slice(5).trim()))
}

async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0))
}

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

type Captured = { url: string; body: Record<string, unknown>; headers: Headers }

function stubUpstream(reply: () => Response): { calls: Captured[] } {
  const calls: Captured[] = []
  globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url
    calls.push({ url, body: init?.body ? JSON.parse(String(init.body)) : {}, headers: new Headers(init?.headers) })
    return reply()
  }) as typeof fetch
  return { calls }
}

/** The Codex CLI 0.150.1 request shape (captured 2026-09-04), model swapped per test. */
function codexCliBody(model: string, extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    model,
    instructions: "You are Codex.",
    input: [
      { type: "message", role: "developer", content: [{ type: "input_text", text: "env" }] },
      { type: "message", role: "user", content: [{ type: "input_text", text: "say hi" }] },
    ],
    tools: [
      { type: "function", name: "exec_command", strict: false, parameters: { type: "object", properties: {} } },
      {
        type: "namespace",
        name: "multi_agent_v1",
        description: "Tools in the multi_agent_v1 namespace.",
        tools: [{ type: "function", name: "spawn_agent", strict: false, parameters: { type: "object", properties: {} } }],
      },
      { type: "web_search", external_web_access: false },
    ],
    tool_choice: "auto",
    parallel_tool_calls: true,
    reasoning: { summary: "auto" },
    store: false,
    stream: true,
    include: ["reasoning.encrypted_content"],
    prompt_cache_key: "01a06ce4-68a8-7892-bcf5-b64013c7f279",
    client_metadata: { session_id: "01a06ce4-68a8-7892-bcf5-b64013c7f279" },
    ...extra,
  }
}

const CHAT_SSE = [
  'data: {"id":"c","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":"hello"},"finish_reason":null}]}',
  'data: {"id":"c","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":11,"completion_tokens":2}}',
  "data: [DONE]",
]
  .map((l) => `${l}\n\n`)
  .join("")

const RESPONSES_SSE = [
  'event: response.created\ndata: {"type":"response.created","sequence_number":0,"response":{"id":"resp_up","status":"in_progress","output":[]}}',
  'event: response.output_text.delta\ndata: {"type":"response.output_text.delta","sequence_number":1,"item_id":"msg_1","output_index":0,"content_index":0,"delta":"hi"}',
  'event: response.completed\ndata: {"type":"response.completed","sequence_number":2,"response":{"id":"resp_up","status":"completed","output":[],"usage":{"input_tokens":7,"output_tokens":1,"input_tokens_details":{"cached_tokens":3}}}}',
]
  .map((l) => `${l}\n\n`)
  .join("")

describe("POST /openai/v1/responses — conversion path", () => {
  it("converts a Codex CLI request for a custom openai provider and streams Responses events back", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedCustomOpenAI(db, "user_1", "mygw")
    const { calls } = stubUpstream(
      () => new Response(CHAT_SSE, { status: 200, headers: { "content-type": "text/event-stream" } }),
    )

    const res = await app.request(
      "/openai/v1/responses",
      { method: "POST", headers: authHeaders(), body: JSON.stringify(codexCliBody("mygw/local-model")) },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)
    expect(res.headers.get("content-type")).toContain("text/event-stream")
    const text = await drain(res.body)
    await settle()

    expect(calls).toHaveLength(1)
    expect(calls[0]!.url).toBe("https://upstream.example.com/v1/chat/completions")
    const sent = calls[0]!.body
    expect(sent.model).toBe("local-model")
    expect(sent.messages).toEqual([
      { role: "system", content: "You are Codex." },
      { role: "system", content: "env" },
      { role: "user", content: "say hi" },
    ])
    const toolNames = (sent.tools as Array<{ function: { name: string } }>).map((t) => t.function.name)
    expect(toolNames).toEqual(["exec_command", "multi_agent_v1__spawn_agent", "web_search"])
    expect(sent).not.toHaveProperty("client_metadata")
    expect(sent).not.toHaveProperty("include")
    expect(sent.stream).toBe(true)

    const evs = events(text)
    expect(evs[0]!.type).toBe("response.created")
    expect(evs.some((e) => e.type === "response.output_text.delta" && e.delta === "hello")).toBe(true)
    const completed = evs.at(-1)!
    expect(completed.type).toBe("response.completed")
    expect((completed.response as Record<string, unknown>).model).toBe("mygw/local-model")
    expect((completed.response as Record<string, unknown>).usage).toEqual({
      input_tokens: 11,
      output_tokens: 2,
      total_tokens: 13,
    })

    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ provider: "mygw", model: "mygw/local-model", prompt_tokens: 11, completion_tokens: 2 })
  })

  it("returns one Response object for a non-stream request", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedCustomOpenAI(db, "user_1", "mygw")
    stubUpstream(() =>
      Response.json({
        id: "chatcmpl_1",
        object: "chat.completion",
        created: 1700000000,
        choices: [
          {
            index: 0,
            message: {
              role: "assistant",
              content: null,
              tool_calls: [{ id: "call_1", type: "function", function: { name: "multi_agent_v1__spawn_agent", arguments: "{}" } }],
            },
            finish_reason: "tool_calls",
          },
        ],
        usage: { prompt_tokens: 5, completion_tokens: 6 },
      }),
    )

    const res = await app.request(
      "/openai/v1/responses",
      { method: "POST", headers: authHeaders(), body: JSON.stringify(codexCliBody("mygw/local-model", { stream: false })) },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)
    const json = (await res.json()) as Record<string, unknown>
    expect(json.object).toBe("response")
    expect(json.status).toBe("completed")
    expect(json.output).toEqual([
      expect.objectContaining({
        type: "function_call",
        call_id: "call_1",
        name: "spawn_agent",
        namespace: "multi_agent_v1",
        arguments: "{}",
      }),
    ])
    expect(json.usage).toEqual({ input_tokens: 5, output_tokens: 6, total_tokens: 11 })
  })

  it("rejects previous_response_id with 400 unsupported_field and no upstream call", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedCustomOpenAI(db, "user_1", "mygw")
    const { calls } = stubUpstream(() => new Response("", { status: 500 }))

    const res = await app.request(
      "/openai/v1/responses",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify(codexCliBody("mygw/local-model", { previous_response_id: "resp_old" })),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(400)
    expect(res.headers.get("x-should-retry")).toBe("false")
    const json = (await res.json()) as { error: { code: string; message: string } }
    expect(json.error.code).toBe("unsupported_field")
    expect(json.error.message).toContain("previous_response_id")
    expect(calls).toHaveLength(0)
    await settle()
    expect(db.rows("request_logs")[0]).toMatchObject({ status_code: 400, error_code: "unsupported_field" })
  })

  it("passes a pre-stream HTTP error through with the OpenAI envelope", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    const res = await app.request(
      "/openai/v1/responses",
      { method: "POST", headers: authHeaders(), body: JSON.stringify(codexCliBody("mygw/local-model", { stream: false })) },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(400)
    const json = (await res.json()) as { error: { code: string } }
    expect(json.error.code).toBe("invalid_model")
  })
})

describe("POST /openai/v1/responses — native codex path", () => {
  it("forwards the CLI body to /codex/responses with the fix-ups and relays the upstream SSE untouched", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "codex" })
    const { calls } = stubUpstream(
      () => new Response(RESPONSES_SSE, { status: 200, headers: { "content-type": "text/event-stream" } }),
    )

    const res = await app.request(
      "/openai/v1/responses",
      { method: "POST", headers: authHeaders(), body: JSON.stringify(codexCliBody("codex/gpt-5.4")) },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)
    const text = await drain(res.body)
    await settle()

    expect(calls).toHaveLength(1)
    expect(calls[0]!.url).toBe("https://chatgpt.com/backend-api/codex/responses")
    expect(calls[0]!.headers.get("session_id")).toBe("01a06ce4-68a8-7892-bcf5-b64013c7f279")
    const sent = calls[0]!.body
    expect(sent.model).toBe("gpt-5.4")
    expect(sent.store).toBe(false)
    expect(sent.stream).toBe(true)
    expect(sent.instructions).toBe("You are Codex.")
    // Hosted web_search, the namespace group, and client_metadata ride through as the CLI sent them.
    expect(sent.tools).toEqual(codexCliBody("codex/gpt-5.4").tools)
    expect(sent.client_metadata).toEqual({ session_id: "01a06ce4-68a8-7892-bcf5-b64013c7f279" })
    expect(sent.input).toEqual(codexCliBody("codex/gpt-5.4").input)
    expect(sent.reasoning).toEqual({ summary: "auto" })

    // Byte-for-byte relay: the upstream's own event lines, ids, and sequence numbers.
    expect(text).toContain(RESPONSES_SSE)

    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      provider: "codex",
      model: "codex/gpt-5.4",
      prompt_tokens: 7,
      completion_tokens: 1,
      cache_read_input_tokens: 3,
      error_code: null,
    })
  })

  it("rewrites dispatch's in-stream error frame into response.failed", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "codex" })
    stubUpstream(() => Response.json({ error: { message: "backend exploded", type: "server_error" } }, { status: 500 }))

    const res = await app.request(
      "/openai/v1/responses",
      { method: "POST", headers: authHeaders(), body: JSON.stringify(codexCliBody("codex/gpt-5.4")) },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)
    const evs = events(await drain(res.body))
    expect(evs).toHaveLength(1)
    expect(evs[0]!.type).toBe("response.failed")
    expect((evs[0]!.response as Record<string, unknown>).error).toEqual({
      code: "upstream_error",
      message: "backend exploded",
    })
  })

  it("collects a non-stream turn into the response.completed object", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "codex" })
    stubUpstream(
      () => new Response(RESPONSES_SSE, { status: 200, headers: { "content-type": "text/event-stream" } }),
    )

    const res = await app.request(
      "/openai/v1/responses",
      { method: "POST", headers: authHeaders(), body: JSON.stringify(codexCliBody("codex/gpt-5.4", { stream: false })) },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)
    const json = (await res.json()) as Record<string, unknown>
    expect(json.id).toBe("resp_up")
    expect(json.status).toBe("completed")
    expect(json.usage).toEqual({ input_tokens: 7, output_tokens: 1, input_tokens_details: { cached_tokens: 3 } })
    const rows = db.rows("request_logs")
    expect(rows[0]).toMatchObject({ prompt_tokens: 7, completion_tokens: 1, cache_read_input_tokens: 3 })
  })
})

describe("POST /g/<slug>/openai/v1/responses", () => {
  it("uses the native path for an all-codex group", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "codex" })
    seedGroup(db, { userId: "user_1", name: "coder", targets: ["codex/gpt-5.4"] })
    const { calls } = stubUpstream(
      () => new Response(RESPONSES_SSE, { status: 200, headers: { "content-type": "text/event-stream" } }),
    )

    const res = await app.request(
      "/g/team/openai/v1/responses",
      { method: "POST", headers: authHeaders(), body: JSON.stringify(codexCliBody("coder")) },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)
    await drain(res.body)
    await settle()
    expect(calls[0]!.url).toBe("https://chatgpt.com/backend-api/codex/responses")
    expect(calls[0]!.body.client_metadata).toBeDefined()
    expect(db.rows("request_logs")[0]).toMatchObject({ model: "codex/gpt-5.4", group_name: "team/coder" })
  })

  it("falls back to the conversion path for a group that mixes codex with another provider", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "codex" })
    await seedCustomOpenAI(db, "user_1", "mygw")
    seedGroup(db, { userId: "user_1", name: "mixed", targets: ["codex/gpt-5.4", "mygw/local-model"] })
    const { calls } = stubUpstream(
      () => new Response(RESPONSES_SSE, { status: 200, headers: { "content-type": "text/event-stream" } }),
    )

    const res = await app.request(
      "/g/team/openai/v1/responses",
      { method: "POST", headers: authHeaders(), body: JSON.stringify(codexCliBody("mixed")) },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)
    const text = await drain(res.body)
    // Codex still answered first, but through the Chat adapter: its body is
    // the proxy-built one (no client_metadata, chat-mapped tools), and the
    // client gets the proxy's converted Responses events, not the relay.
    expect(calls[0]!.url).toBe("https://chatgpt.com/backend-api/codex/responses")
    expect(calls[0]!.body).not.toHaveProperty("client_metadata")
    const sentTools = calls[0]!.body.tools as Array<{ type: string; name: string }>
    expect(sentTools.map((t) => t.name)).toEqual(["exec_command", "multi_agent_v1__spawn_agent", "web_search"])
    const evs = events(text)
    expect(evs[0]!.type).toBe("response.created")
    expect((evs.at(-1)!.response as Record<string, unknown>).model).toBe("mixed")
  })
})
