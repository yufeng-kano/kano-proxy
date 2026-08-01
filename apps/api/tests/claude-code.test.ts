import { afterEach, describe, expect, it } from "vitest"
import { claudeCodeAdapter, betaHeaders, resolveBetaHeader } from "../src/providers/claude-code"
import type { Env } from "../src/env"

const BASE = [
  "oauth-2025-04-20",
  "claude-code-20250219",
  "interleaved-thinking-2025-05-14",
  "fine-grained-tool-streaming-2025-05-14",
]

describe("betaHeaders", () => {
  it("returns the 4 base flags with no extra", () => {
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
})

describe("resolveBetaHeader (native /anthropic passthrough)", () => {
  it("leaves the base 4 flags unchanged with no output_config and no client betas", () => {
    expect(resolveBetaHeader({ hasOutputConfig: false }).split(",")).toEqual(BASE)
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
})

describe("claudeCodeAdapter.countTokens", () => {
  const fakeAccount = {
    row: {
      id: "acc_1",
      user_id: "user_1",
      provider: "claude-code",
      external_account_id: null,
      label: null,
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
