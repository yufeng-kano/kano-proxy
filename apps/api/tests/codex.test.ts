import { afterEach, describe, expect, it } from "vitest"
import {
  buildCodexRequestBody,
  CODEX_ORIGINATOR,
  CODEX_USER_AGENT,
  codexAdapter,
  codexSessionId,
  mergeCodexReplayItems,
} from "../src/providers/codex"
import type { Env } from "../src/env"
import type { AcquiredAccount } from "../src/pool/acquire"
import { encryptJson, decryptJson } from "../src/crypto/token_crypto"
import { FakeD1 } from "./helpers/fake_d1"
import { fetchCodexUsageJson, windowsFromCodexPayload } from "../src/providers/codex_usage"
import { anthropicToOpenAIChatRequest } from "../src/proxy/openai_anthropic"

const codexAccount: AcquiredAccount = {
  row: {
    id: "acc_1",
    user_id: "user_1",
    provider: "codex",
    external_account_id: null,
    label: null,
    custom_label: null,
    priority: 1,
    encrypted_payload: "",
    account_meta_json: null,
    usage_snapshot_json: null,
    usage_fetched_at: null,
    usage_fetching_at: null,
    created_at: "2026-01-01T00:00:00.000Z",
    updated_at: "2026-01-01T00:00:00.000Z",
  },
  credential: { access_token: "tok_test" },
}

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

function mockEnv(): Env {
  return {} as Env
}

async function captureCodexRequest(req: Parameters<typeof codexAdapter.chatCompletions>[2]) {
  let captured: { headers: Headers; body: Record<string, unknown> } | undefined
  globalThis.fetch = (async (_url: string, init?: RequestInit) => {
    captured = {
      headers: new Headers(init?.headers),
      body: JSON.parse(String(init?.body)),
    }
    return new Response("", { status: 200 })
  }) as typeof fetch
  await codexAdapter.chatCompletions(mockEnv(), codexAccount, req)
  return captured!
}

describe("codex OAuth refresh single-flight", () => {
  it("refreshes one expired account once and gives the loser the winner's persisted credential", async () => {
    const db = new FakeD1()
    const tokenKey = "refresh-single-flight-test-key"
    const expired = {
      access_token: "old-access",
      refresh_token: "old-refresh",
      expires_at: new Date(Date.now() - 60_000).toISOString(),
    }
    const encrypted = await encryptJson(tokenKey, expired)
    const row = { ...codexAccount.row, encrypted_payload: encrypted, refreshing_at: null }
    db.seed("upstream_accounts", [row])
    const env = { DB: db as unknown as D1Database, TOKEN_ENCRYPTION_KEY: tokenKey } as Env
    let refreshCalls = 0
    let releaseRefresh: (() => void) | undefined
    const refreshStarted = new Promise<void>((resolve) => {
      releaseRefresh = resolve
    })
    let refreshEntered: (() => void) | undefined
    const entered = new Promise<void>((resolve) => {
      refreshEntered = resolve
    })
    globalThis.fetch = (async () => {
      refreshCalls++
      refreshEntered!()
      await refreshStarted
      return Response.json({ access_token: "winner-access", refresh_token: "winner-refresh", expires_in: 3600 })
    }) as typeof fetch

    const staleAccount = { row, credential: expired }
    const winner = codexAdapter.refreshIfNeeded!(env, staleAccount)
    await entered
    const loser = codexAdapter.refreshIfNeeded!(env, staleAccount)
    releaseRefresh!()
    const [winnerAccount, loserAccount] = await Promise.all([winner, loser])

    expect(refreshCalls).toBe(1)
    expect(winnerAccount.credential.access_token).toBe("winner-access")
    expect(loserAccount.credential.access_token).toBe("winner-access")
    expect((await decryptJson<typeof expired>(tokenKey, db.rows("upstream_accounts")[0]!.encrypted_payload as string)).access_token).toBe("winner-access")
  })
})

describe("codex upstream request headers", () => {
  const baseReq = {
    model: "codex/gpt-5.2",
    rawModel: "codex/gpt-5.2",
    upstreamModel: "gpt-5.2",
    messages: [{ role: "user", content: "hi" }],
    rawBody: {},
  }

  it("uses the CLI identity headers and omits OpenAI-Beta", async () => {
    const captured = await captureCodexRequest(baseReq)
    expect(captured.headers.get("user-agent")).toBe(CODEX_USER_AGENT)
    expect(captured.headers.get("originator")).toBe(CODEX_ORIGINATOR)
    expect(captured.headers.get("connection")).toBe("Keep-Alive")
    expect(captured.headers.has("openai-beta")).toBe(false)
    expect(captured.headers.get("session_id")).toBeTruthy()
  })

  it("uses session affinity first, then conversation affinity", async () => {
    const session = await captureCodexRequest({ ...baseReq, affinity: { sessionId: "session-1", convId: "conv-1" } })
    expect(session.headers.get("session_id")).toBe("session-1")
    const conversation = await captureCodexRequest({ ...baseReq, affinity: { convId: "conv-2" } })
    expect(conversation.headers.get("session_id")).toBe("conv-2")
  })

  it("derives session_id from prompt_cache_key when no affinity headers exist", async () => {
    const uuid = "0e35a1af-fe45-49c8-b0cc-fb1c58b1b06e"
    const claudeCode = await captureCodexRequest({
      ...baseReq,
      prompt_cache_key: `user_ab_account_11111111-2222-3333-4444-555555555555_session_${uuid}`,
    })
    expect(claudeCode.headers.get("session_id")).toBe(uuid)

    const opaque = await captureCodexRequest({ ...baseReq, prompt_cache_key: "conv-key-1" })
    expect(opaque.headers.get("session_id")).toBe("conv-key-1")

    const affinityWins = await captureCodexRequest({
      ...baseReq,
      prompt_cache_key: "conv-key-1",
      affinity: { sessionId: "session-9" },
    })
    expect(affinityWins.headers.get("session_id")).toBe("session-9")
  })

  it("fits an over-long prompt_cache_key in the session_id header too, not just the body", async () => {
    // The backend validates this header under the `prompt_cache_key` name,
    // so an unfitted header 400s the turn even with a fitted body field.
    const key = "q".repeat(200)
    const captured = await captureCodexRequest({ ...baseReq, prompt_cache_key: key })
    expect(captured.headers.get("session_id")).toMatch(/^[0-9a-f]{64}$/)
    expect(captured.body.prompt_cache_key).toMatch(/^[0-9a-f]{64}$/)
  })

  it("fits an over-long client affinity id in the session_id header", async () => {
    const captured = await captureCodexRequest({
      ...baseReq,
      affinity: { sessionId: "s".repeat(120) },
    })
    expect(captured.headers.get("session_id")).toMatch(/^[0-9a-f]{64}$/)
  })

  it("omits an empty ChatGPT account id", async () => {
    const captured = await captureCodexRequest(baseReq)
    expect(captured.headers.has("chatgpt-account-id")).toBe(false)
  })
})

describe("buildCodexRequestBody", () => {
  it("always includes encrypted reasoning and only enables parallel tools when tools exist", async () => {
    const noTools = await buildCodexRequestBody({
      upstreamModel: "m",
      messages: [{ role: "user", content: "hi" }],
    })
    expect(noTools.include).toEqual(["reasoning.encrypted_content"])
    expect("parallel_tool_calls" in noTools).toBe(false)

    const withTools = await buildCodexRequestBody({
      upstreamModel: "m",
      messages: [{ role: "user", content: "hi" }],
      tools: [{ type: "function", function: { name: "lookup", parameters: {} } }],
    })
    expect(withTools.parallel_tool_calls).toBe(true)
  })

  it("strips rejected fields and keeps only priority service tier", async () => {
    const rejected = [
      "max_output_tokens",
      "max_completion_tokens",
      "temperature",
      "top_p",
      "truncation",
      "user",
      "previous_response_id",
      "generate",
      "prompt_cache_retention",
      "safety_identifier",
      "stream_options",
    ]
    const body = await buildCodexRequestBody({
      upstreamModel: "m",
      messages: [{ role: "user", content: "hi" }],
      rawBody: Object.fromEntries(rejected.map((field) => [field, "reject"]).concat([["service_tier", "auto"]])),
    })
    for (const field of rejected) expect(field in body).toBe(false)
    expect("service_tier" in body).toBe(false)

    const priority = await buildCodexRequestBody({
      upstreamModel: "m",
      messages: [{ role: "user", content: "hi" }],
      rawBody: { service_tier: "priority" },
    })
    expect(priority.service_tier).toBe("priority")
  })

  it("rewrites a system item that reaches input to developer", async () => {
    const body = await buildCodexRequestBody({
      upstreamModel: "m",
      messages: [{ role: "system", content: "rules" }],
    })
    expect(body.instructions).toBe("rules")
    expect((body.input as Array<Record<string, unknown>>).some((item) => item.role === "system")).toBe(false)
  })

  it("shortens long call ids consistently and preserves short ids", async () => {
    const longId = "a".repeat(80)
    const expected = `${"a".repeat(47)}_0f45e858fbc4176c`
    const body = await buildCodexRequestBody({
      upstreamModel: "m",
      messages: [
        {
          role: "assistant",
          content: null,
          tool_calls: [{ id: longId, function: { name: "lookup", arguments: "{}" } }],
        },
        { role: "tool", tool_call_id: longId, content: "ok" },
        { role: "assistant", content: null, tool_calls: [{ id: "short", function: { name: "x" } }] },
      ],
    })
    const input = body.input as Array<Record<string, unknown>>
    expect(input[0]!.call_id).toBe(expected)
    expect(input[1]!.call_id).toBe(expected)
    expect(input[2]!.call_id).toBe("short")
    expect(expected).toHaveLength(64)
  })

  it("matches the reference SHA-256 suffix for a long call id", async () => {
    const id = "x".repeat(65)
    const body = await buildCodexRequestBody({
      upstreamModel: "m",
      messages: [{ role: "assistant", content: null, tool_calls: [{ id, function: { name: "x" } }] }],
    })
    expect((body.input as Array<Record<string, unknown>>)[0]!.call_id).toBe(
      `${"x".repeat(47)}_9537c5fdf120482f`,
    )
  })

  it("leaves exactly 64-character call ids untouched", async () => {
    const id = "z".repeat(64)
    const body = await buildCodexRequestBody({
      upstreamModel: "m",
      messages: [{ role: "assistant", content: null, tool_calls: [{ id, function: { name: "x" } }] }],
    })
    expect((body.input as Array<Record<string, unknown>>)[0]!.call_id).toBe(id)
  })

  it("sends store:false unconditionally", async () => {
    const body = await buildCodexRequestBody({
      upstreamModel: "gpt-5.2",
      messages: [{ role: "user", content: "hi" }],
    })
    expect(body.store).toBe(false)
  })

  describe("system → instructions", () => {
    it("folds a single system message into instructions and drops it from input", async () => {
      const body = await buildCodexRequestBody({
        upstreamModel: "gpt-5.2",
        messages: [
          { role: "system", content: "Be terse." },
          { role: "user", content: "hi" },
        ],
      })
      expect(body.instructions).toBe("Be terse.")
      const input = body.input as Array<Record<string, unknown>>
      expect(input).toHaveLength(1)
      expect(input.some((m) => m.role === "system")).toBe(false)
      expect(input[0]).toMatchObject({ role: "user" })
      // No trace of the old "[system]\n" fake-user-message wrapping.
      expect(JSON.stringify(input)).not.toContain("[system]")
    })

    it("joins multiple system messages with a blank line, in order, and drops both from input", async () => {
      const body = await buildCodexRequestBody({
        upstreamModel: "gpt-5.2",
        messages: [
          { role: "system", content: "First." },
          { role: "user", content: "hi" },
          { role: "system", content: "Second." },
        ],
      })
      expect(body.instructions).toBe("First.\n\nSecond.")
      const input = body.input as Array<Record<string, unknown>>
      expect(input).toHaveLength(1)
      expect(input.some((m) => m.role === "system")).toBe(false)
    })

    it("omits instructions entirely when there are no system messages", async () => {
      const body = await buildCodexRequestBody({
        upstreamModel: "gpt-5.2",
        messages: [{ role: "user", content: "hi" }],
      })
      expect("instructions" in body).toBe(false)
    })
  })

  describe("tool_choice mapping", () => {
    const tools = [{ type: "function", function: { name: "lookup", parameters: {} } }]

    it("passes auto/none/required through as the same string", async () => {
      for (const choice of ["auto", "none", "required"]) {
        const body = await buildCodexRequestBody({
          upstreamModel: "m",
          messages: [{ role: "user", content: "hi" }],
          tools,
          tool_choice: choice,
        })
        expect(body.tool_choice).toBe(choice)
      }
    })

    it("maps a named function tool_choice to the Responses flattened shape", async () => {
      const body = await buildCodexRequestBody({
        upstreamModel: "m",
        messages: [{ role: "user", content: "hi" }],
        tools,
        tool_choice: { type: "function", function: { name: "lookup" } },
      })
      expect(body.tool_choice).toEqual({ type: "function", name: "lookup" })
    })

    it("defaults to auto when tools are present but tool_choice is absent", async () => {
      const body = await buildCodexRequestBody({
        upstreamModel: "m",
        messages: [{ role: "user", content: "hi" }],
        tools,
      })
      expect(body.tool_choice).toBe("auto")
    })

    it("omits tool_choice (and tools) entirely when there are no tools", async () => {
      const body = await buildCodexRequestBody({
        upstreamModel: "m",
        messages: [{ role: "user", content: "hi" }],
        tool_choice: "auto",
      })
      expect("tool_choice" in body).toBe(false)
      expect("tools" in body).toBe(false)
    })

    it("treats an empty tools array as no tools", async () => {
      const body = await buildCodexRequestBody({
        upstreamModel: "m",
        messages: [{ role: "user", content: "hi" }],
        tools: [],
        tool_choice: "auto",
      })
      expect("tool_choice" in body).toBe(false)
      expect("tools" in body).toBe(false)
    })
  })

  describe("prompt_cache_key", () => {
    it("forwards prompt_cache_key when set", async () => {
      const body = await buildCodexRequestBody({
        upstreamModel: "m",
        messages: [{ role: "user", content: "hi" }],
        prompt_cache_key: "conv-123",
      })
      expect(body.prompt_cache_key).toBe("conv-123")
    })

    it("omits prompt_cache_key when not set", async () => {
      const body = await buildCodexRequestBody({
        upstreamModel: "m",
        messages: [{ role: "user", content: "hi" }],
      })
      expect("prompt_cache_key" in body).toBe(false)
    })

    // Upstream rejects anything over 64 chars with
    // `Invalid 'prompt_cache_key': string too long`.
    it("sends the bare session uuid for a Claude Code metadata.user_id", async () => {
      const uuid = "0e35a1af-fe45-49c8-b0cc-fb1c58b1b06e"
      const key = `user_${"a".repeat(64)}_account_11111111-2222-3333-4444-555555555555_session_${uuid}`
      expect(key.length).toBeGreaterThan(140)

      const body = await buildCodexRequestBody({
        upstreamModel: "m",
        messages: [{ role: "user", content: "hi" }],
        prompt_cache_key: key,
      })
      expect(body.prompt_cache_key).toBe(uuid)
      expect(String(body.prompt_cache_key).length).toBeLessThanOrEqual(64)
    })

    it("hashes an over-long key with no session suffix, stably", async () => {
      const key = "x".repeat(200)
      const build = async () =>
        buildCodexRequestBody({
          upstreamModel: "m",
          messages: [{ role: "user", content: "hi" }],
          prompt_cache_key: key,
        })

      const first = (await build()).prompt_cache_key
      const second = (await build()).prompt_cache_key
      expect(first).toBe(second)
      expect(first).toMatch(/^[0-9a-f]{64}$/)
    })

    it("keeps two long keys sharing a prefix on distinct cache shards", async () => {
      const prefix = "user_shared_account_prefix".padEnd(100, "p")
      const build = async (suffix: string) =>
        (
          await buildCodexRequestBody({
            upstreamModel: "m",
            messages: [{ role: "user", content: "hi" }],
            prompt_cache_key: prefix + suffix,
          })
        ).prompt_cache_key

      expect(await build("-alpha")).not.toBe(await build("-beta"))
    })

    it("never emits a key over the upstream limit", async () => {
      const keys = [
        "short",
        "y".repeat(64),
        "y".repeat(65),
        "z".repeat(300),
        `user_ab_account_1_session_0e35a1af-fe45-49c8-b0cc-fb1c58b1b06e`,
      ]
      for (const prompt_cache_key of keys) {
        const body = await buildCodexRequestBody({
          upstreamModel: "m",
          messages: [{ role: "user", content: "hi" }],
          prompt_cache_key,
        })
        expect(String(body.prompt_cache_key).length).toBeLessThanOrEqual(64)
      }
    })
  })

  describe("reasoning", () => {
    it("sets reasoning when passed, omits it otherwise", async () => {
      const withReasoning = await buildCodexRequestBody(
        { upstreamModel: "m", messages: [{ role: "user", content: "hi" }] },
        { effort: "high", summary: "auto" },
      )
      expect(withReasoning.reasoning).toEqual({ effort: "high", summary: "auto" })

      const withoutReasoning = await buildCodexRequestBody({
        upstreamModel: "m",
        messages: [{ role: "user", content: "hi" }],
      })
      expect("reasoning" in withoutReasoning).toBe(false)
    })
  })

  describe("assistant message ordering", () => {
    it("emits the assistant text message before its function_call items", async () => {
      const body = await buildCodexRequestBody({
        upstreamModel: "gpt-5.2",
        messages: [
          {
            role: "assistant",
            content: "Let me check that file.",
            tool_calls: [
              {
                id: "call_1",
                type: "function",
                function: { name: "Read", arguments: '{"file_path":"/a"}' },
              },
            ],
          },
        ],
      })
      const input = body.input as Array<Record<string, unknown>>
      expect(input).toHaveLength(2)
      expect(input[0]).toEqual({
        role: "assistant",
        content: [{ type: "output_text", text: "Let me check that file." }],
      })
      expect(input[1]).toEqual({
        type: "function_call",
        call_id: "call_1",
        name: "Read",
        arguments: '{"file_path":"/a"}',
      })
    })

    it("keeps only function_call items when there is no assistant text", async () => {
      const body = await buildCodexRequestBody({
        upstreamModel: "gpt-5.2",
        messages: [
          {
            role: "assistant",
            content: null,
            tool_calls: [
              { id: "call_1", type: "function", function: { name: "A", arguments: "{}" } },
              { id: "call_2", type: "function", function: { name: "B", arguments: "{}" } },
            ],
          },
        ],
      })
      const input = body.input as Array<Record<string, unknown>>
      expect(input).toHaveLength(2)
      expect(input.every((i) => i.type === "function_call")).toBe(true)
    })

    it("emits only the text message when there are no tool_calls", async () => {
      const body = await buildCodexRequestBody({
        upstreamModel: "gpt-5.2",
        messages: [{ role: "assistant", content: "just text" }],
      })
      const input = body.input as Array<Record<string, unknown>>
      expect(input).toEqual([
        { role: "assistant", content: [{ type: "output_text", text: "just text" }] },
      ])
    })
  })
})

describe("Anthropic system → codex instructions (via anthropicToOpenAIChatRequest)", () => {
  it("ends up in the Responses instructions field, not input", async () => {
    const converted = anthropicToOpenAIChatRequest({
      system: "You are a helpful assistant.",
      messages: [{ role: "user", content: "hi" }],
    })
    const body = await buildCodexRequestBody({
      upstreamModel: "gpt-5.2",
      messages: converted.messages,
    })
    expect(body.instructions).toBe("You are a helpful assistant.")
    const input = body.input as Array<Record<string, unknown>>
    expect(input.some((m) => m.role === "system")).toBe(false)
  })

  it("joins multi-block Anthropic system content the same way", async () => {
    const converted = anthropicToOpenAIChatRequest({
      system: [
        { type: "text", text: "Part one." },
        { type: "text", text: "Part two." },
      ],
      messages: [{ role: "user", content: "hi" }],
    })
    const body = await buildCodexRequestBody({
      upstreamModel: "gpt-5.2",
      messages: converted.messages,
    })
    expect(body.instructions).toBe("Part one.\n\nPart two.")
  })
})

describe("codex ignores reasoning_content on replayed history", () => {
  it("a history thinking block converts to reasoning_content, which the Responses input builder silently ignores", async () => {
    const converted = anthropicToOpenAIChatRequest({
      messages: [
        {
          role: "assistant",
          content: [
            { type: "thinking", thinking: "reasoning from a prior turn" },
            { type: "text", text: "here is the answer" },
          ],
        },
        { role: "user", content: "thanks" },
      ],
    })
    const assistantMsg = converted.messages[0] as Record<string, unknown>
    expect(assistantMsg.reasoning_content).toBe("reasoning from a prior turn")

    const body = await buildCodexRequestBody({
      upstreamModel: "gpt-5.2",
      messages: converted.messages,
    })
    const serialized = JSON.stringify(body)
    expect(serialized).not.toContain("reasoning_content")
    expect(serialized).not.toContain("reasoning from a prior turn")
    // The rest of that assistant turn is still built normally.
    const input = body.input as Array<Record<string, unknown>>
    expect(input[0]).toEqual({
      role: "assistant",
      content: [{ type: "output_text", text: "here is the answer" }],
    })
  })
})

/**
 * `/codex/usage` is `403` + HTML for everyone, independent of the CF-Worker
 * wall, while the `/wham/usage` alias passes (docs/providers.md § The
 * chatgpt.com wall). An earlier version returned on the first challenge, so
 * the working alias was never reached and every codex account rendered with
 * no usage bars. Order and fallthrough are the contract here.
 */
describe("fetchCodexUsageJson usage aliases", () => {
  const validUsage = {
    rate_limit: { primary_window: { used_percent: 17, limit_window_seconds: 18000 } },
  }

  it("tries /wham/usage first and succeeds there even when /codex/usage would challenge", async () => {
    const calls: string[] = []
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      const url = String(input)
      calls.push(url)
      if (url.endsWith("/wham/usage")) {
        return new Response(JSON.stringify(validUsage), { status: 200 })
      }
      if (url.endsWith("/codex/usage")) {
        return new Response("<!doctype html><title>Just a moment</title>", { status: 403 })
      }
      throw new Error(`unexpected fetch: ${url}`)
    }) as typeof fetch

    const result = await fetchCodexUsageJson("tok_test", "acct_test")

    expect(result).toMatchObject({ ok: true, payload: validUsage })
    expect(calls).toEqual(["https://chatgpt.com/backend-api/wham/usage"])
  })

  it("continues from a challenged /wham/usage alias to /codex/usage", async () => {
    const calls: string[] = []
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      const url = String(input)
      calls.push(url)
      if (url.endsWith("/wham/usage")) {
        return new Response("<!doctype html><title>Just a moment</title>", { status: 403 })
      }
      if (url.endsWith("/codex/usage")) {
        return new Response(JSON.stringify(validUsage), { status: 200 })
      }
      throw new Error(`unexpected fetch: ${url}`)
    }) as typeof fetch

    const result = await fetchCodexUsageJson("tok_test", "acct_test")

    expect(result).toMatchObject({ ok: true, payload: validUsage })
    expect(calls).toEqual([
      "https://chatgpt.com/backend-api/wham/usage",
      "https://chatgpt.com/backend-api/codex/usage",
    ])
  })

  it("reports edge blocked only after both aliases return HTML challenges", async () => {
    const calls: string[] = []
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      calls.push(String(input))
      return new Response("<!doctype html><title>Just a moment</title>", { status: 403 })
    }) as typeof fetch

    const result = await fetchCodexUsageJson("tok_test", "acct_test")

    expect(calls).toEqual([
      "https://chatgpt.com/backend-api/wham/usage",
      "https://chatgpt.com/backend-api/codex/usage",
    ])
    expect(result).toMatchObject({ ok: false, edgeBlocked: true })
    expect(result.error).toMatch(/edge blocked|403 bot challenge/i)
  })

  it("returns a successful-response JSON parse failure without trying the fallback alias", async () => {
    const calls: string[] = []
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      calls.push(String(input))
      return new Response("not JSON", { status: 200 })
    }) as typeof fetch

    const result = await fetchCodexUsageJson("tok_test", "acct_test")

    expect(result).toMatchObject({
      ok: false,
      status: 200,
      payload: null,
      edgeBlocked: false,
      error: "usage JSON parse failed",
    })
    expect(calls).toEqual(["https://chatgpt.com/backend-api/wham/usage"])
  })
})

/**
 * Regression coverage for the `UsageWindow.utilization` scale contract
 * (percent 0–100, never a 0–1 fraction — see `UsageWindow` in
 * src/providers/types.ts). The admin UI once showed 100% for an account
 * actually at 1% because a frontend heuristic rescaled any value <= 1; that
 * heuristic is gone, so this locks the adapter's window-mapping in place.
 * `windowsFromCodexPayload` is a pure function — no fetch stubbing needed.
 */
describe("windowsFromCodexPayload — window mapping and the utilization scale contract", () => {
  it("REGRESSION: used_percent = 1 (meaning 1%) must produce utilization === 1 exactly — this is the exact value that a removed frontend heuristic once rescaled to 100%", async () => {
    const windows = windowsFromCodexPayload({
      rate_limit: {
        primary_window: { used_percent: 1, limit_window_seconds: 18000, reset_at: 1_780_000_000 },
      },
    })
    expect(windows[0]!.utilization).toBe(1)
  })

  it("a mid-range percent (73) passes through unchanged", async () => {
    const windows = windowsFromCodexPayload({
      rate_limit: { primary_window: { used_percent: 73, limit_window_seconds: 18000 } },
    })
    expect(windows[0]!.utilization).toBe(73)
  })

  it("100 (fully used) passes through unchanged", async () => {
    const windows = windowsFromCodexPayload({
      rate_limit: { primary_window: { used_percent: 100, limit_window_seconds: 604800 } },
    })
    expect(windows[0]!.utilization).toBe(100)
  })

  it("a window with no used_percent maps to utilization: null, not 0", async () => {
    const windows = windowsFromCodexPayload({
      rate_limit: { primary_window: { limit_window_seconds: 18000 } },
    })
    expect(windows[0]!.utilization).toBeNull()
  })

  it("reset_at (unix seconds) converts to an ISO string; a window with no reset_at maps resets_at to null", async () => {
    const windows = windowsFromCodexPayload({
      rate_limit: {
        primary_window: { used_percent: 10, limit_window_seconds: 18000, reset_at: 1_735_689_600 },
        secondary_window: { used_percent: 20, limit_window_seconds: 604800 },
      },
    })
    expect(windows[0]!.resets_at).toBe(new Date(1_735_689_600 * 1000).toISOString())
    expect(windows[1]!.resets_at).toBeNull()
  })

  it("labels derive from limit_window_seconds: 604800 -> Week, 18000 -> 5h, whole hour/day values, unknown -> Ns, absent -> 'window'", async () => {
    expect(
      windowsFromCodexPayload({
        rate_limit: { primary_window: { used_percent: 1, limit_window_seconds: 604800 } },
      })[0]!.label,
    ).toBe("Week")
    expect(
      windowsFromCodexPayload({
        rate_limit: { primary_window: { used_percent: 1, limit_window_seconds: 18000 } },
      })[0]!.label,
    ).toBe("5h")
    expect(
      windowsFromCodexPayload({
        rate_limit: { primary_window: { used_percent: 1, limit_window_seconds: 3600 } },
      })[0]!.label,
    ).toBe("1h")
    expect(
      windowsFromCodexPayload({
        rate_limit: { primary_window: { used_percent: 1, limit_window_seconds: 172800 } },
      })[0]!.label,
    ).toBe("2d")
    expect(
      windowsFromCodexPayload({
        rate_limit: { primary_window: { used_percent: 1, limit_window_seconds: 7777 } },
      })[0]!.label,
    ).toBe("7777s")
    expect(
      windowsFromCodexPayload({ rate_limit: { primary_window: { used_percent: 1 } } })[0]!.label,
    ).toBe("window")
  })

  it("both primary_window and secondary_window map to two windows, in order", async () => {
    const windows = windowsFromCodexPayload({
      rate_limit: {
        primary_window: { used_percent: 5, limit_window_seconds: 18000 },
        secondary_window: { used_percent: 6, limit_window_seconds: 604800 },
      },
    })
    expect(windows).toHaveLength(2)
    expect(windows[0]).toMatchObject({ label: "5h", utilization: 5 })
    expect(windows[1]).toMatchObject({ label: "Week", utilization: 6 })
  })

  it("no rate_limit at all maps to an empty windows array", async () => {
    expect(windowsFromCodexPayload({})).toEqual([])
  })

  it("an explicit null window (e.g. secondary_window: null) is skipped, not crashed on", async () => {
    const windows = windowsFromCodexPayload({
      rate_limit: {
        primary_window: { used_percent: 1, limit_window_seconds: 18000 },
        secondary_window: null,
      },
    })
    expect(windows).toHaveLength(1)
  })
})

describe("buildCodexRequestBody: call_id shortening", () => {
  const build = (messages: unknown[]) =>
    buildCodexRequestBody({
      upstreamModel: "m",
      messages,
      rawBody: {},
    } as never)

  const itemsOf = (body: Record<string, unknown>) =>
    body.input as Array<Record<string, unknown>>

  it("leaves an id of exactly 64 chars untouched", async () => {
    const id = "a".repeat(64)
    const items = itemsOf(
      await build([{ role: "assistant", tool_calls: [{ id, function: { name: "f", arguments: "{}" } }] }]),
    )
    expect(items.find((i) => i.type === "function_call")!.call_id).toBe(id)
  })

  it("shortens a >64-char id to exactly 64 chars with a hash suffix", async () => {
    const id = "toolu_" + "x".repeat(100)
    const items = itemsOf(
      await build([{ role: "assistant", tool_calls: [{ id, function: { name: "f", arguments: "{}" } }] }]),
    )
    const got = String(items.find((i) => i.type === "function_call")!.call_id)
    expect(got).toHaveLength(64)
    expect(got.startsWith(id.slice(0, 47))).toBe(true)
    expect(got).toMatch(/_[0-9a-f]{16}$/)
  })

  it("gives a function_call and its function_call_output the SAME shortened id", async () => {
    // If these diverge the tool result no longer pairs with its call upstream.
    const id = "call_" + "9".repeat(120)
    const items = itemsOf(
      await build([
        { role: "assistant", tool_calls: [{ id, function: { name: "f", arguments: "{}" } }] },
        { role: "tool", tool_call_id: id, content: "ok" },
      ]),
    )
    const call = items.find((i) => i.type === "function_call")!
    const output = items.find((i) => i.type === "function_call_output")!
    expect(call.call_id).toBe(output.call_id)
    expect(String(call.call_id)).toHaveLength(64)
  })

  it("derives the suffix from a real SHA-256 (cross-checked against Web Crypto)", async () => {
    // Pin the suffix to the platform digest, including a multi-byte input,
    // so the id a client sends still resolves to the same shortened form.
    for (const id of ["z".repeat(65), "汉字".repeat(50), "a".repeat(120)]) {
      const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(id))
      const hex = [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("")
      const items = itemsOf(
        await build([
          { role: "assistant", tool_calls: [{ id, function: { name: "f", arguments: "{}" } }] },
        ]),
      )
      const suffix = "_" + hex.slice(0, 16)
      expect(items.find((i) => i.type === "function_call")!.call_id).toBe(
        id.slice(0, 64 - suffix.length) + suffix,
      )
    }
  })
})

describe("codex reasoning replay wiring", () => {
  type KV = { get: unknown; put: unknown; delete: unknown }
  function kvEnv(store: Map<string, string>) {
    const CACHE = {
      get: async (k: string, type?: string) => {
        const raw = store.get(k)
        if (raw === undefined) return null
        return type === "json" ? JSON.parse(raw) : raw
      },
      put: async (k: string, v: string) => void store.set(k, v),
      delete: async (k: string) => void store.delete(k),
    } as unknown as KV
    return { CACHE } as unknown as Env
  }

  const affinity = { sessionId: "sess_1" }
  const baseReq = {
    upstreamModel: "gpt-5.6-sol",
    messages: [{ role: "user", content: "hi" }],
    rawBody: {},
    affinity,
  }

  /** Drive one non-stream turn whose upstream SSE reports `output`. */
  async function runTurn(env: Env, req: Record<string, unknown>, output: unknown[]) {
    const sse =
      `data: ${JSON.stringify({ type: "response.output_text.delta", delta: "answer" })}\n\n` +
      `data: ${JSON.stringify({ type: "response.completed", response: { output } })}\n\n`
    let sent: Record<string, unknown> | undefined
    globalThis.fetch = (async (_u: string, init?: RequestInit) => {
      sent = JSON.parse(String(init?.body))
      return new Response(sse, { status: 200 })
    }) as typeof fetch
    const waits: Promise<unknown>[] = []
    await codexAdapter.chatCompletions(env, codexAccount, req as never, {
      apiKeyId: "key_1",
      waitUntil: (p) => void waits.push(p),
    })
    await Promise.all(waits)
    return sent!
  }

  const reasoningItem = { type: "reasoning", encrypted_content: "gpt_abc" }

  it("persists reasoning from a completed turn and replays it on the next", async () => {
    const store = new Map<string, string>()
    const env = kvEnv(store)
    await runTurn(env, baseReq, [reasoningItem, { type: "message", content: [] }])
    expect(store.size).toBe(1)

    // Next turn echoes the prior assistant text, so the hash matches.
    const sent = await runTurn(
      env,
      {
        ...baseReq,
        messages: [
          { role: "user", content: "hi" },
          { role: "assistant", content: "answer" },
          { role: "user", content: "again" },
        ],
      },
      [],
    )
    expect(sent.input).toContainEqual(reasoningItem)
  })

  it("does not replay when the trailing assistant text no longer matches", async () => {
    const store = new Map<string, string>()
    const env = kvEnv(store)
    await runTurn(env, baseReq, [reasoningItem])
    const sent = await runTurn(
      env,
      {
        ...baseReq,
        messages: [
          { role: "assistant", content: "a DIFFERENT answer" },
          { role: "user", content: "again" },
        ],
      },
      [],
    )
    expect(sent.input).not.toContainEqual(reasoningItem)
  })

  it("is a no-op without a session id or an api key id", async () => {
    const store = new Map<string, string>()
    const env = kvEnv(store)
    await runTurn(env, { ...baseReq, affinity: undefined }, [reasoningItem])
    expect(store.size).toBe(0)
  })

  it("scopes replay by prompt_cache_key when no affinity headers exist (the /anthropic Claude Code path)", async () => {
    const store = new Map<string, string>()
    const env = kvEnv(store)
    const keyed = {
      ...baseReq,
      affinity: undefined,
      prompt_cache_key: "user_ab_account_1_session_0e35a1af-fe45-49c8-b0cc-fb1c58b1b06e",
    }
    await runTurn(env, keyed, [reasoningItem, { type: "message", content: [] }])
    expect(store.size).toBe(1)
    const sent = await runTurn(
      env,
      {
        ...keyed,
        messages: [
          { role: "user", content: "hi" },
          { role: "assistant", content: "answer" },
          { role: "user", content: "again" },
        ],
      },
      [],
    )
    expect(sent.input).toContainEqual(reasoningItem)
  })

  it("clears a stale entry when a completed turn has nothing replayable", async () => {
    const store = new Map<string, string>()
    const env = kvEnv(store)
    await runTurn(env, baseReq, [reasoningItem])
    expect(store.size).toBe(1)
    await runTurn(env, baseReq, [{ type: "message", content: [] }])
    expect(store.size).toBe(0)
  })
})

describe("codexSessionId", () => {
  it("extracts the bare session uuid from a Claude Code metadata.user_id", () => {
    const uuid = "0e35a1af-fe45-49c8-b0cc-fb1c58b1b06e"
    expect(codexSessionId(`user_ab_account_x_session_${uuid}`)).toBe(uuid)
  })

  it("passes any other non-empty key through and yields null for blank input", () => {
    expect(codexSessionId("conv-key-1")).toBe("conv-key-1")
    expect(codexSessionId("ends_session_notauuid")).toBe("ends_session_notauuid")
    expect(codexSessionId("  ")).toBeNull()
    expect(codexSessionId(undefined)).toBeNull()
  })
})

describe("mergeCodexReplayItems", () => {
  const reasoning = { type: "reasoning", encrypted_content: "gpt_x" }

  it("inserts reasoning ahead of the tool results it produced", async () => {
    const input = [
      { role: "user", content: [] },
      { type: "function_call_output", call_id: "c1", output: "ok" },
    ]
    expect(mergeCodexReplayItems(input, [reasoning, { type: "function_call", call_id: "c1" }])).toEqual([
      input[0],
      reasoning,
      { type: "function_call", call_id: "c1" },
      input[1],
    ])
  })

  it("drops a function_call the client already replayed, avoiding a duplicate", async () => {
    const input = [
      { type: "function_call", call_id: "c1" },
      { type: "function_call_output", call_id: "c1", output: "ok" },
    ]
    const merged = mergeCodexReplayItems(input, [{ type: "function_call", call_id: "c1" }])
    expect(merged.filter((i) => (i as { type?: string }).type === "function_call")).toHaveLength(1)
  })

  it("skips cached reasoning when the input already carries its own", async () => {
    const input = [{ type: "reasoning", encrypted_content: "gpt_client" }]
    expect(mergeCodexReplayItems(input, [reasoning])).toEqual(input)
  })

  it("appends when there is no tool result to anchor against", async () => {
    const input = [{ role: "user", content: [] }]
    expect(mergeCodexReplayItems(input, [reasoning])).toEqual([input[0], reasoning])
  })
})
