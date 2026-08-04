import { afterEach, describe, expect, it } from "vitest"
import {
  claudeCodeAdapter,
  betaHeaders,
  CLAUDE_CLIENT_FINGERPRINT,
  resolveBetaHeader,
} from "../src/providers/claude-code"
import type { Env } from "../src/env"
import type { AcquiredAccount } from "../src/pool/acquire"

// The fixed pair the Claude Code OAuth upstream requires to accept a
// request at all — always first, on both the native passthrough and the
// /openai/v1 conversion path.
const BASE = ["oauth-2025-04-20", "claude-code-20250219"]

// Feature betas the conversion path (chatCompletions) still opts into
// unconditionally. The native passthrough (resolveBetaHeader) no longer
// force-adds these — see the "resolveBetaHeader" suite below.
const CONVERSION_EXTRA = ["interleaved-thinking-2025-05-14", "fine-grained-tool-streaming-2025-05-14"]

describe("betaHeaders", () => {
  it("returns the 2 required flags with no extra", () => {
    expect(betaHeaders().split(",")).toEqual(BASE)
    expect(betaHeaders(null).split(",")).toEqual(BASE)
    expect(betaHeaders("").split(",")).toEqual(BASE)
  })

  it("appends a client extra not already in the base set", () => {
    expect(betaHeaders("context-management-2025-06-27").split(",")).toEqual([
      ...BASE,
      "context-management-2025-06-27",
    ])
  })

  it("does not double an extra that duplicates a base flag", () => {
    expect(betaHeaders("oauth-2025-04-20").split(",")).toEqual(BASE)
  })

  it("dedups repeated extras within the same string", () => {
    expect(betaHeaders("foo,foo,bar").split(",")).toEqual([...BASE, "foo", "bar"])
  })

  it("preserves extra order verbatim, including feature betas passed explicitly", () => {
    expect(betaHeaders(CONVERSION_EXTRA.join(",")).split(",")).toEqual([...BASE, ...CONVERSION_EXTRA])
  })
})

describe("resolveBetaHeader (native /anthropic passthrough)", () => {
  it("with no client betas and no output_config, emits exactly the 2 required flags — no feature betas force-added", () => {
    expect(resolveBetaHeader({ hasOutputConfig: false }).split(",")).toEqual(BASE)
    expect(resolveBetaHeader({ hasOutputConfig: false }).split(",")).not.toContain(
      "interleaved-thinking-2025-05-14",
    )
    expect(resolveBetaHeader({ hasOutputConfig: false }).split(",")).not.toContain(
      "fine-grained-tool-streaming-2025-05-14",
    )
  })

  it("adds effort-2025-11-24 when the (patched) body carries output_config", () => {
    const header = resolveBetaHeader({ hasOutputConfig: true })
    expect(header.split(",")).toEqual([...BASE, "effort-2025-11-24"])
  })

  it("does not add the effort beta when output_config is absent, even with other client betas", () => {
    const header = resolveBetaHeader({
      clientBeta: "context-management-2025-06-27",
      hasOutputConfig: false,
    })
    expect(header.split(",")).toEqual([...BASE, "context-management-2025-06-27"])
    expect(header).not.toContain("effort-2025-11-24")
  })

  it("does not double a client-supplied effort-2025-11-24 when output_config is present", () => {
    const header = resolveBetaHeader({
      clientBeta: "effort-2025-11-24",
      hasOutputConfig: true,
    })
    expect(header.split(",").filter((b) => b === "effort-2025-11-24")).toHaveLength(1)
    expect(header.split(",")).toEqual([...BASE, "effort-2025-11-24"])
  })

  it("keeps other client betas alongside the auto-added effort beta", () => {
    const header = resolveBetaHeader({
      clientBeta: "context-management-2025-06-27",
      hasOutputConfig: true,
    })
    expect(header.split(",")).toEqual([
      ...BASE,
      "context-management-2025-06-27",
      "effort-2025-11-24",
    ])
  })

  it("treats a null clientBeta the same as absent", () => {
    expect(resolveBetaHeader({ clientBeta: null, hasOutputConfig: true }).split(",")).toEqual([
      ...BASE,
      "effort-2025-11-24",
    ])
  })

  it("honors the client's own choice to send interleaved-thinking-2025-05-14 — present because the client asked, not force-added", () => {
    const header = resolveBetaHeader({
      clientBeta: "interleaved-thinking-2025-05-14",
      hasOutputConfig: false,
    })
    expect(header.split(",")).toEqual([...BASE, "interleaved-thinking-2025-05-14"])
  })

  it("preserves the client's full anthropic-beta list verbatim and in order after the required pair", () => {
    const clientList = [
      "fine-grained-tool-streaming-2025-05-14",
      "context-management-2025-06-27",
      "interleaved-thinking-2025-05-14",
    ]
    const header = resolveBetaHeader({ clientBeta: clientList.join(","), hasOutputConfig: false })
    expect(header.split(",")).toEqual([...BASE, ...clientList])
  })

  it("a client sending one of the fixed pair is not doubled", () => {
    const header = resolveBetaHeader({
      clientBeta: "claude-code-20250219,context-management-2025-06-27",
      hasOutputConfig: false,
    })
    expect(header.split(",")).toEqual([...BASE, "context-management-2025-06-27"])
  })
})

describe("claudeCodeAdapter.chatCompletions (/openai/v1 conversion path)", () => {
  const fakeAccount: AcquiredAccount = {
    row: {
      id: "acc_1",
      user_id: "user_1",
      provider: "claude-code",
      external_account_id: null,
      label: null,
      custom_label: null,
      priority: 1,
      encrypted_payload: "",
      account_meta_json: null,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
    credential: { access_token: "tok_test" },
  }

  const originalFetch = globalThis.fetch
  afterEach(() => {
    globalThis.fetch = originalFetch
  })

  it("sends the exact unchanged anthropic-beta header — required pair + both feature betas + effort, byte-identical to the pre-faithful-passthrough string", async () => {
    let capturedInit: RequestInit | undefined
    globalThis.fetch = (async (_url: string, init?: RequestInit) => {
      capturedInit = init
      return new Response(
        JSON.stringify({
          id: "msg_1",
          type: "message",
          role: "assistant",
          content: [{ type: "text", text: "hi" }],
          stop_reason: "end_turn",
          usage: { input_tokens: 1, output_tokens: 1 },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      )
    }) as typeof fetch

    const res = await claudeCodeAdapter.chatCompletions({} as unknown as Env, fakeAccount, {
      model: "claude-code/claude-opus-5",
      rawModel: "claude-code/claude-opus-5",
      upstreamModel: "claude-opus-5",
      messages: [{ role: "user", content: "hi" }],
      rawBody: { model: "claude-code/claude-opus-5", messages: [{ role: "user", content: "hi" }] },
    })

    const sentHeaders = capturedInit?.headers as Record<string, string>
    expect(sentHeaders["anthropic-beta"]).toBe(
      "oauth-2025-04-20,claude-code-20250219,interleaved-thinking-2025-05-14,fine-grained-tool-streaming-2025-05-14,effort-2025-11-24",
    )
    expect(res.status).toBe(200)
  })
})

describe("claudeCodeAdapter.countTokens", () => {
  const fakeAccount = {
    row: {
      id: "acc_1",
      user_id: "user_1",
      provider: "claude-code",
      external_account_id: null,
      label: null,
      custom_label: null,
      priority: 1,
      encrypted_payload: "",
      account_meta_json: null,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
    credential: { access_token: "tok_test" },
  }

  const originalFetch = globalThis.fetch
  afterEach(() => {
    globalThis.fetch = originalFetch
  })

  it("forwards to /v1/messages/count_tokens with the same header construction as messages()", async () => {
    let capturedUrl: string | undefined
    let capturedInit: RequestInit | undefined
    globalThis.fetch = (async (url: string, init?: RequestInit) => {
      capturedUrl = url
      capturedInit = init
      return new Response(JSON.stringify({ input_tokens: 42 }), {
        status: 200,
        headers: { "content-type": "application/json" },
      })
    }) as typeof fetch

    const headers = new Headers({ "anthropic-beta": "context-management-2025-06-27" })
    const res = await claudeCodeAdapter.countTokens!(
      {} as unknown as Env,
      fakeAccount,
      { model: "claude-opus-5", messages: [{ role: "user", content: "hi" }] },
      headers,
    )

    expect(capturedUrl).toBe("https://api.anthropic.com/v1/messages/count_tokens")
    expect(capturedInit?.method).toBe("POST")
    const sentHeaders = capturedInit?.headers as Record<string, string>
    expect(sentHeaders.authorization).toBe("Bearer tok_test")
    expect(sentHeaders["anthropic-version"]).toBe("2023-06-01")
    expect(sentHeaders["anthropic-beta"]).toBe(
      [...BASE, "context-management-2025-06-27"].join(","),
    )
    // Idempotent required-system prepend, same as the messages() adapter path.
    const sentBody = JSON.parse(String(capturedInit?.body))
    expect(sentBody.system).toEqual([
      { type: "text", text: "You are Claude Code, Anthropic's official CLI for Claude." },
    ])
    expect(sentBody.model).toBe("claude-opus-5")
    expect(res.status).toBe(200)
  })

  it("adds the effort beta when the (patched) body carries output_config, same as messages()", async () => {
    let capturedInit: RequestInit | undefined
    globalThis.fetch = (async (_url: string, init?: RequestInit) => {
      capturedInit = init
      return new Response(JSON.stringify({ input_tokens: 1 }), { status: 200 })
    }) as typeof fetch

    await claudeCodeAdapter.countTokens!(
      {} as unknown as Env,
      fakeAccount,
      { model: "claude-opus-5", messages: [], output_config: { effort: "high" } },
      new Headers(),
    )

    const sentHeaders = capturedInit?.headers as Record<string, string>
    expect(sentHeaders["anthropic-beta"]).toBe([...BASE, "effort-2025-11-24"].join(","))
  })

  it("never streams — returns the upstream response as-is on error too", async () => {
    globalThis.fetch = (async () =>
      new Response(JSON.stringify({ error: { message: "bad request" } }), {
        status: 400,
        headers: { "content-type": "application/json" },
      })) as typeof fetch

    const res = await claudeCodeAdapter.countTokens!(
      {} as unknown as Env,
      fakeAccount,
      { model: "claude-opus-5", messages: [] },
      new Headers(),
    )
    expect(res.status).toBe(400)
    expect(await res.json()).toEqual({ error: { message: "bad request" } })
  })
})

/**
 * Regression coverage for the `UsageWindow.utilization` scale contract
 * (percent 0–100, never a 0–1 fraction — see `UsageWindow` in
 * src/providers/types.ts). The admin UI once showed 100% for an account
 * actually at 1% because a frontend heuristic rescaled any value <= 1; that
 * heuristic is gone, so this locks the adapter's window-mapping in place.
 */
describe("claudeCodeAdapter.fetchUsage — window mapping and the utilization scale contract", () => {
  const usageAccount: AcquiredAccount = {
    row: {
      id: "acc_1",
      user_id: "user_1",
      provider: "claude-code",
      external_account_id: null,
      label: null,
      custom_label: null,
      priority: 1,
      encrypted_payload: "",
      account_meta_json: null,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
    credential: { access_token: "tok_test" },
  }

  const originalFetch = globalThis.fetch
  afterEach(() => {
    globalThis.fetch = originalFetch
  })

  /** Stubs the two concurrent GETs fetchUsage makes: /api/oauth/usage and /api/oauth/profile. */
  function stubFetch(
    usageBody: unknown,
    profileBody: unknown = {},
    opts?: { usageStatus?: number; profileStatus?: number },
  ): void {
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      const url = String(input)
      if (url.includes("/api/oauth/usage")) {
        return new Response(JSON.stringify(usageBody), {
          status: opts?.usageStatus ?? 200,
          headers: { "content-type": "application/json" },
        })
      }
      if (url.includes("/api/oauth/profile")) {
        return new Response(JSON.stringify(profileBody), {
          status: opts?.profileStatus ?? 200,
          headers: { "content-type": "application/json" },
        })
      }
      throw new Error(`unexpected fetch: ${url}`)
    }) as typeof fetch
  }

  it("REGRESSION: five_hour.utilization = 1 (meaning 1%) must produce utilization === 1 exactly — this is the exact value that a removed frontend heuristic once rescaled to 100%", async () => {
    stubFetch({ five_hour: { utilization: 1, resets_at: "2026-08-03T12:00:00Z" } })
    const result = await claudeCodeAdapter.fetchUsage!({} as unknown as Env, usageAccount)
    expect(result.windows).toEqual([
      { label: "5h", utilization: 1, resets_at: "2026-08-03T12:00:00Z" },
    ])
  })

  it("a mid-range percent (73) passes through unchanged", async () => {
    stubFetch({ five_hour: { utilization: 73, resets_at: null } })
    const result = await claudeCodeAdapter.fetchUsage!({} as unknown as Env, usageAccount)
    expect(result.windows[0]).toMatchObject({ utilization: 73 })
  })

  it("100 (fully used) passes through unchanged", async () => {
    stubFetch({ seven_day: { utilization: 100, resets_at: "2026-08-10T00:00:00Z" } })
    const result = await claudeCodeAdapter.fetchUsage!({} as unknown as Env, usageAccount)
    expect(result.windows).toEqual([
      { label: "Week", utilization: 100, resets_at: "2026-08-10T00:00:00Z" },
    ])
  })

  it("a present window with no utilization field maps to utilization: null, not 0", async () => {
    stubFetch({ five_hour: { resets_at: "2026-08-03T12:00:00Z" } })
    const result = await claudeCodeAdapter.fetchUsage!({} as unknown as Env, usageAccount)
    expect(result.windows).toEqual([
      { label: "5h", utilization: null, resets_at: "2026-08-03T12:00:00Z" },
    ])
  })

  it("both five_hour and seven_day map to their own labeled windows", async () => {
    stubFetch({
      five_hour: { utilization: 12, resets_at: "2026-08-03T12:00:00Z" },
      seven_day: { utilization: 34, resets_at: "2026-08-10T00:00:00Z" },
    })
    const result = await claudeCodeAdapter.fetchUsage!({} as unknown as Env, usageAccount)
    expect(result.windows).toEqual([
      { label: "5h", utilization: 12, resets_at: "2026-08-03T12:00:00Z" },
      { label: "Week", utilization: 34, resets_at: "2026-08-10T00:00:00Z" },
    ])
  })

  it("REGRESSION: a weekly_scoped limit carries its percent as `percent`, not `utilization` — reading the wrong field rendered the scoped row as '—' in the UI while its label and reset time showed correctly", async () => {
    // Shape copied from a real /api/oauth/usage response.
    stubFetch({
      limits: [
        {
          kind: "session",
          group: "session",
          percent: 22,
          resets_at: "2026-08-05T00:00:00Z",
          scope: null,
        },
        {
          kind: "weekly_all",
          group: "weekly",
          percent: 62,
          resets_at: "2026-08-05T00:00:00Z",
          scope: null,
        },
        {
          kind: "weekly_scoped",
          group: "weekly",
          percent: 59,
          severity: "normal",
          resets_at: "2026-08-05T00:00:00Z",
          scope: { model: { id: null, display_name: "Fable" } },
          is_active: false,
        },
      ],
    })
    const result = await claudeCodeAdapter.fetchUsage!({} as unknown as Env, usageAccount)
    expect(result.windows).toEqual([
      { label: "Fable", utilization: 59, resets_at: "2026-08-05T00:00:00Z" },
    ])
  })

  it("a weekly_scoped limit maps to a window labeled from scope.model.display_name; non-weekly_scoped entries are ignored", async () => {
    stubFetch({
      limits: [
        {
          kind: "weekly_scoped",
          percent: 42,
          resets_at: "2026-08-05T00:00:00Z",
          scope: { model: { display_name: "Claude Opus 5" } },
        },
        { kind: "something_else", percent: 99 },
      ],
    })
    const result = await claudeCodeAdapter.fetchUsage!({} as unknown as Env, usageAccount)
    expect(result.windows).toEqual([
      { label: "Claude Opus 5", utilization: 42, resets_at: "2026-08-05T00:00:00Z" },
    ])
  })

  it("a weekly_scoped limit with no scope.model.display_name falls back to the label 'scoped'", async () => {
    stubFetch({ limits: [{ kind: "weekly_scoped", percent: 5 }] })
    const result = await claudeCodeAdapter.fetchUsage!({} as unknown as Env, usageAccount)
    expect(result.windows).toEqual([{ label: "scoped", utilization: 5, resets_at: null }])
  })

  it("a weekly_scoped limit with neither percent nor utilization maps to null, not 0", async () => {
    stubFetch({ limits: [{ kind: "weekly_scoped", scope: { model: { display_name: "Fable" } } }] })
    const result = await claudeCodeAdapter.fetchUsage!({} as unknown as Env, usageAccount)
    expect(result.windows).toEqual([{ label: "Fable", utilization: null, resets_at: null }])
  })

  it("account meta comes from the profile response: email, plan_type from organization.organization_type, rate_limit_tier", async () => {
    stubFetch(
      { five_hour: { utilization: 10 } },
      {
        account: { email: "user@example.com" },
        organization: { organization_type: "claude_max", rate_limit_tier: "tier_4" },
      },
    )
    const result = await claudeCodeAdapter.fetchUsage!({} as unknown as Env, usageAccount)
    expect(result.account).toEqual({
      email: "user@example.com",
      plan_type: "claude_max",
      rate_limit_tier: "tier_4",
    })
  })

  it("account email falls back to the credential's email when the profile has none", async () => {
    const acctWithEmail: AcquiredAccount = {
      row: usageAccount.row,
      credential: { access_token: "tok_test", email: "cred@example.com" },
    }
    stubFetch({ five_hour: { utilization: 10 } }, {})
    const result = await claudeCodeAdapter.fetchUsage!({} as unknown as Env, acctWithEmail)
    expect((result.account as Record<string, unknown>).email).toBe("cred@example.com")
  })

  it("a non-OK usage response returns stale: true with the documented error string and empty windows/account", async () => {
    stubFetch({}, {}, { usageStatus: 429 })
    const result = await claudeCodeAdapter.fetchUsage!({} as unknown as Env, usageAccount)
    expect(result).toEqual({ windows: [], account: {}, stale: true, error: "usage 429" })
  })
})

describe("claude-code client fingerprint", () => {
  const account = {
    row: { id: "acc_1" },
    credential: { access_token: "tok" },
  } as never

  const originalFetch = globalThis.fetch
  afterEach(() => {
    globalThis.fetch = originalFetch
  })

  async function capture(run: () => Promise<unknown>) {
    let headers: Headers | undefined
    globalThis.fetch = (async (_u: string, init?: RequestInit) => {
      headers = new Headers(init?.headers)
      return new Response("{}", {
        status: 200,
        headers: { "content-type": "application/json" },
      })
    }) as typeof fetch
    await run()
    return headers!
  }

  it("sends the CLI fingerprint on the native /anthropic passthrough", async () => {
    const headers = await capture(() =>
      claudeCodeAdapter.messages!(
        {} as never,
        account,
        { model: "m", messages: [] },
        new Headers(),
      ),
    )
    expect(headers.get("user-agent")).toBe(CLAUDE_CLIENT_FINGERPRINT["user-agent"])
    expect(headers.get("x-stainless-os")).toBe("MacOS")
    expect(headers.get("x-stainless-arch")).toBe("arm64")
  })

  it("prefers the real client's own fingerprint when it sent one", async () => {
    const headers = await capture(() =>
      claudeCodeAdapter.messages!(
        {} as never,
        account,
        { model: "m", messages: [] },
        new Headers({
          "user-agent": "claude-cli/9.9.9 (external, vscode)",
          "x-stainless-os": "Linux",
        }),
      ),
    )
    expect(headers.get("user-agent")).toBe("claude-cli/9.9.9 (external, vscode)")
    expect(headers.get("x-stainless-os")).toBe("Linux")
    // Unsent fields still fall back to the baseline.
    expect(headers.get("x-stainless-arch")).toBe("arm64")
  })

  it("sends the fallback fingerprint on the /openai/v1 conversion surface", async () => {
    const headers = await capture(() =>
      claudeCodeAdapter.chatCompletions(
        {} as never,
        account,
        { upstreamModel: "m", messages: [], rawBody: {} } as never,
      ),
    )
    expect(headers.get("user-agent")).toBe(CLAUDE_CLIENT_FINGERPRINT["user-agent"])
  })
})
