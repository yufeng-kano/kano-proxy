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

/**
 * Seeds a group row plus its `model_group_aliases` rows. Defaults to a
 * single alias equal to `name`, so every existing test here (which sends
 * `name`'s value as `model`) keeps resolving unchanged; pass `aliases`
 * explicitly to exercise multi-alias dispatch.
 */
function seedGroup(
  db: FakeD1,
  opts: { userId: string; name: string; targets: unknown[]; aliases?: string[] },
): void {
  const id = `mgrp_${opts.name}`
  db.seed("model_groups", [
    {
      id,
      user_id: opts.userId,
      name: opts.name,
      targets_json: JSON.stringify(opts.targets),
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
  const aliases = opts.aliases ?? [opts.name]
  db.seed(
    "model_group_aliases",
    aliases.map((alias, i) => ({
      id: `${id}_alias_${i}`,
      user_id: opts.userId,
      group_id: id,
      alias,
      created_at: "2026-01-01T00:00:00.000Z",
    })),
  )
}

/** Distinguishable-credential account, for asserting exactly which account's token reached the wire. */
async function seedAccountWithToken(
  db: FakeD1,
  opts: { id: string; userId: string; provider: string; accessToken: string; priority?: number },
): Promise<void> {
  const encrypted = await encryptJson(TOKEN_KEY, { access_token: opts.accessToken })
  db.seed("upstream_accounts", [
    {
      id: opts.id,
      user_id: opts.userId,
      provider: opts.provider,
      external_account_id: null,
      label: opts.provider,
      priority: opts.priority ?? 1,
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

  it("multi-alias group: whichever alias the client sends is what's echoed and logged, never the display name or a sibling alias", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccount(db, { userId: "user_1", provider: "claude-code" })
    seedGroup(db, {
      userId: "user_1",
      name: "OpenAI GPT-4o family", // free-text display name — not callable
      aliases: ["gpt-4o", "gpt-4", "gpt-4-turbo"],
      targets: ["claude-code/claude-opus-5"],
    })

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
        body: JSON.stringify({ model: "gpt-4", messages: [{ role: "user", content: "hi" }] }),
      },
      buildEnv(db),
      execCtx,
    )
    expect(res.status).toBe(200)
    const json = (await res.json()) as { model: string }
    expect(json.model).toBe("gpt-4")

    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      provider: "claude-code",
      model: "claude-code/claude-opus-5",
      group_name: "gpt-4",
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

describe("account pinning — dispatch actually uses exactly the pinned account (docs/providers.md § Model groups \"Account pinning\")", () => {
  it("openai surface: the pinned account's credential reaches the wire, not a higher-priority sibling's", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    // Higher priority would normally win an unpinned pool acquire — pinning
    // must bypass that and use the low-priority account the group names.
    await seedAccountWithToken(db, {
      id: "acc_high",
      userId: "user_1",
      provider: "claude-code",
      accessToken: "token-high-priority",
      priority: 10,
    })
    await seedAccountWithToken(db, {
      id: "acc_low",
      userId: "user_1",
      provider: "claude-code",
      accessToken: "token-low-priority",
      priority: 1,
    })
    seedGroup(db, {
      userId: "user_1",
      name: "opus",
      targets: [{ model: "claude-code/claude-opus-5", account_id: "acc_low" }],
    })

    let capturedAuth: string | null = null
    let callCount = 0
    globalThis.fetch = (async (_input: unknown, init?: RequestInit) => {
      callCount++
      capturedAuth = (init?.headers as Record<string, string> | undefined)?.authorization ?? null
      return new Response(
        JSON.stringify({
          id: "msg_1",
          content: [{ type: "text", text: "hi" }],
          stop_reason: "end_turn",
        }),
        { status: 200 },
      )
    }) as typeof fetch

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
    expect(callCount).toBe(1)
    expect(capturedAuth).toBe("Bearer token-low-priority")
  })

  it("anthropic surface: same pinning — the pinned account's credential reaches the wire", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccountWithToken(db, {
      id: "acc_high",
      userId: "user_1",
      provider: "claude-code",
      accessToken: "token-high-priority",
      priority: 10,
    })
    await seedAccountWithToken(db, {
      id: "acc_low",
      userId: "user_1",
      provider: "claude-code",
      accessToken: "token-low-priority",
      priority: 1,
    })
    seedGroup(db, {
      userId: "user_1",
      name: "opus",
      targets: [{ model: "claude-code/claude-opus-5", account_id: "acc_low" }],
    })

    let capturedAuth: string | null = null
    globalThis.fetch = (async (_input: unknown, init?: RequestInit) => {
      capturedAuth = (init?.headers as Record<string, string> | undefined)?.authorization ?? null
      return new Response(
        JSON.stringify({
          id: "msg_1",
          content: [{ type: "text", text: "hi" }],
          stop_reason: "end_turn",
        }),
        { status: 200 },
      )
    }) as typeof fetch

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
    expect(capturedAuth).toBe("Bearer token-low-priority")
  })

  it("failover is disabled for a pinned target: a benched pinned account never falls over to a usable sibling — exactly one upstream call, the sibling's token never sent", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccountWithToken(db, {
      id: "acc_pinned",
      userId: "user_1",
      provider: "claude-code",
      accessToken: "token-pinned",
    })
    await seedAccountWithToken(db, {
      id: "acc_sibling",
      userId: "user_1",
      provider: "claude-code",
      accessToken: "token-sibling",
    })
    seedGroup(db, {
      userId: "user_1",
      name: "opus",
      targets: [{ model: "claude-code/claude-opus-5", account_id: "acc_pinned" }],
    })

    const seenAuth: string[] = []
    globalThis.fetch = (async (_input: unknown, init?: RequestInit) => {
      const auth = (init?.headers as Record<string, string> | undefined)?.authorization ?? ""
      seenAuth.push(auth)
      // Upstream rate-limits the pinned account — benchable, would normally
      // trigger failover to a sibling in an unpinned pool.
      return new Response(JSON.stringify({ error: { message: "rate limited" } }), { status: 429 })
    }) as typeof fetch

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
    // The pinned account's own 429 passes straight through — no synthesized
    // upstream_unavailable, and critically, no second attempt on the sibling.
    expect(res.status).toBe(429)
    expect(seenAuth).toEqual(["Bearer token-pinned"])

    const rows = db.rows("request_logs")
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({
      provider: "claude-code",
      model: "claude-code/claude-opus-5",
      group_name: "opus",
      status_code: 429,
      error_code: "upstream_error",
    })
  })

  it("an unpinned target in the same group still fails over normally within its own pool", async () => {
    const db = new FakeD1()
    await seedApiKey(db, "user_1")
    await seedAccountWithToken(db, {
      id: "acc_first",
      userId: "user_1",
      provider: "claude-code",
      accessToken: "token-first",
      priority: 10,
    })
    await seedAccountWithToken(db, {
      id: "acc_second",
      userId: "user_1",
      provider: "claude-code",
      accessToken: "token-second",
      priority: 1,
    })
    // Unpinned target — ordinary pool failover still applies.
    seedGroup(db, { userId: "user_1", name: "opus", targets: ["claude-code/claude-opus-5"] })

    const seenAuth: string[] = []
    globalThis.fetch = (async (_input: unknown, init?: RequestInit) => {
      const auth = (init?.headers as Record<string, string> | undefined)?.authorization ?? ""
      seenAuth.push(auth)
      if (auth === "Bearer token-first") {
        return new Response(JSON.stringify({ error: { message: "rate limited" } }), { status: 429 })
      }
      return new Response(
        JSON.stringify({
          id: "msg_1",
          content: [{ type: "text", text: "hi" }],
          stop_reason: "end_turn",
        }),
        { status: 200 },
      )
    }) as typeof fetch

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
    expect(seenAuth).toEqual(["Bearer token-first", "Bearer token-second"])
  })
})
