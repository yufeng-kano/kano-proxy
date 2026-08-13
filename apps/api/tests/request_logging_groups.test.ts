/**
 * Model groups, driven through the real dispatch path with stubbed fetch
 * (docs/providers.md warns this repo has been burned before by asserting on
 * builders instead of the wire — see the v2.7.2 note there). Two contracts:
 * client-visible `model` keeps echoing the group name the client sent, while
 * `request_logs` stores the expanded canonical target plus `group_name`
 * (docs/api.md "Model routing", docs/database.md `request_logs.group_name`).
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

function seedGroup(
  db: FakeD1,
  opts: { userId: string; name: string; targets: string[] },
): void {
  db.seed("model_groups", [
    {
      id: `mgrp_${opts.name}`,
      user_id: opts.userId,
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

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

describe("/openai/v1/chat/completions — model group dispatch", () => {
  it("claude-code target: response echoes the group name, request_logs stores the expanded target + group_name", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "claude-code" })
    seedGroup(db, { userId: "user_1", name: "opus", targets: ["claude-code/claude-opus-5"] })

    globalThis.fetch = (async () =>
      new Response(
        JSON.stringify({
          id: "msg_1",
          content: [{ type: "text", text: "hi" }],
          stop_reason: "end_turn",
          usage: { input_tokens: 10, output_tokens: 5 },
        }),
        { status: 200 },
      )) as typeof fetch

    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({ model: "opus", messages: [{ role: "user", content: "hi" }] }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)
    const json = (await res.json()) as { model: string }
    // Client-visible echo: the bare group name it sent, never the expanded target.
    expect(json.model).toBe("opus")

    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      provider: "claude-code",
      model: "claude-code/claude-opus-5",
      group_name: "opus",
    })
  })

  it("grok target (pure passthrough on /openai/v1): request_logs still stores the expanded target + group_name", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "grok" })
    seedGroup(db, { userId: "user_1", name: "fast", targets: ["grok/grok-4.5"] })

    globalThis.fetch = (async () =>
      new Response(
        JSON.stringify({
          id: "x",
          model: "grok-4.5",
          choices: [{ message: { role: "assistant", content: "hi" }, finish_reason: "stop" }],
          usage: { prompt_tokens: 10, completion_tokens: 5 },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      )) as typeof fetch

    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({ model: "fast", messages: [{ role: "user", content: "hi" }] }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)

    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      provider: "grok",
      model: "grok/grok-4.5",
      group_name: "fast",
    })
  })

  it("a direct provider/model request (no group) logs group_name NULL — no regression", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "grok" })

    globalThis.fetch = (async () =>
      new Response(
        JSON.stringify({
          id: "x",
          choices: [{ message: { role: "assistant", content: "hi" }, finish_reason: "stop" }],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      )) as typeof fetch

    const res = await app.request(
      "/openai/v1/chat/completions",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({ model: "grok/grok-4.5", messages: [{ role: "user", content: "hi" }] }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)
    const rows = db.rows("request_logs")
    expect(rows[0]!.group_name).toBeNull()
  })
})

describe("/anthropic/v1/messages — model group dispatch", () => {
  it("claude-code target (native passthrough): request_logs stores the expanded target + group_name", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "claude-code" })
    seedGroup(db, { userId: "user_1", name: "opus", targets: ["claude-code/claude-opus-5"] })

    globalThis.fetch = (async () =>
      new Response(
        JSON.stringify({
          id: "msg_1",
          content: [{ type: "text", text: "hi" }],
          stop_reason: "end_turn",
          usage: { input_tokens: 10, output_tokens: 5 },
        }),
        { status: 200 },
      )) as typeof fetch

    const res = await app.request(
      "/anthropic/v1/messages",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "opus",
          max_tokens: 100,
          messages: [{ role: "user", content: "hi" }],
        }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      provider: "claude-code",
      model: "claude-code/claude-opus-5",
      group_name: "opus",
    })
  })

  it("grok target (Anthropic ↔ Responses): response model echoes the group name via x-kano-raw-model, request_logs stores the expanded target + group_name", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "grok" })
    seedGroup(db, { userId: "user_1", name: "fast", targets: ["grok/grok-4.5"] })

    globalThis.fetch = (async () =>
      new Response(
        [
          'data: {"type":"response.output_text.delta","delta":"hi"}',
          "",
          'data: {"type":"response.completed","response":{"usage":{"input_tokens":50,"output_tokens":10}}}',
          "",
        ].join("\n"),
        { status: 200, headers: { "content-type": "text/event-stream" } },
      )) as typeof fetch

    const res = await app.request(
      "/anthropic/v1/messages",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "fast",
          max_tokens: 100,
          messages: [{ role: "user", content: "hi" }],
        }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)
    const json = (await res.json()) as { model: string }
    // Echo mechanism for grok's Anthropic path: x-kano-raw-model header,
    // set from resolved.raw — the bare group name on a group hit.
    expect(json.model).toBe("fast")

    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      provider: "grok",
      model: "grok/grok-4.5",
      group_name: "fast",
    })
  })

  it("codex target (Anthropic → OpenAI conversion): response echoes the group name, request_logs stores the expanded target + group_name", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "codex" })
    seedGroup(db, { userId: "user_1", name: "gpt-4o", targets: ["codex/gpt-5.2"] })

    globalThis.fetch = (async () =>
      new Response(
        [
          'data: {"type":"response.output_text.delta","delta":"hi"}',
          "",
          'data: {"type":"response.completed","response":{"usage":{"input_tokens":10,"output_tokens":5}}}',
          "",
        ].join("\n"),
        { status: 200, headers: { "content-type": "text/event-stream" } },
      )) as typeof fetch

    const res = await app.request(
      "/anthropic/v1/messages",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "gpt-4o",
          max_tokens: 100,
          messages: [{ role: "user", content: "hi" }],
        }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)
    const json = (await res.json()) as { model: string }
    expect(json.model).toBe("gpt-4o")

    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      provider: "codex",
      model: "codex/gpt-5.2",
      group_name: "gpt-4o",
    })
  })

  it("a miss (unknown group name) is invalid_model, logged with the raw string and no group_name", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")

    const res = await app.request(
      "/anthropic/v1/messages",
      {
        method: "POST",
        headers: authHeaders(),
        body: JSON.stringify({
          model: "no-such-group",
          max_tokens: 10,
          messages: [{ role: "user", content: "hi" }],
        }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(400)
    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      model: "no-such-group",
      status_code: 400,
      error_code: "invalid_model",
      group_name: null,
    })
  })
})
