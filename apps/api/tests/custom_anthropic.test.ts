import { afterEach, describe, expect, it } from "vitest"
import { createCustomAnthropicAdapter } from "../src/providers/custom_anthropic"
import type { CustomProviderRow } from "../src/db/custom_providers"
import type { Env } from "../src/env"
import type { AcquiredAccount } from "../src/pool/acquire"

const row: CustomProviderRow = {
  id: "cprov_2",
  user_id: "user_1",
  slug: "my-claude",
  name: "My Claude-compatible",
  format: "anthropic",
  base_url: "https://upstream.example.com",
  models_mode: "auto",
  manual_models_json: null,
  created_at: "2026-01-01T00:00:00.000Z",
  updated_at: "2026-01-01T00:00:00.000Z",
}

const account: AcquiredAccount = {
  row: {
    id: "acc_2",
    user_id: "user_1",
    provider: "my-claude",
    external_account_id: null,
    label: null,
    priority: 1,
    encrypted_payload: "",
    account_meta_json: null,
    created_at: "2026-01-01T00:00:00.000Z",
    updated_at: "2026-01-01T00:00:00.000Z",
  },
  credential: { access_token: "sk-ant-test-key" },
}

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

describe("createCustomAnthropicAdapter", () => {
  it("has no fetchUsage/refreshIfNeeded — static key, no usage windows", () => {
    const adapter = createCustomAnthropicAdapter(row)
    expect(adapter.fetchUsage).toBeUndefined()
    expect(adapter.refreshIfNeeded).toBeUndefined()
  })

  describe("messages() — native passthrough", () => {
    it("posts to {base}/v1/messages with x-api-key auth", async () => {
      let capturedUrl: string | undefined
      let capturedInit: RequestInit | undefined
      globalThis.fetch = (async (url: string, init?: RequestInit) => {
        capturedUrl = url
        capturedInit = init
        return new Response("{}", { status: 200 })
      }) as typeof fetch

      const adapter = createCustomAnthropicAdapter(row)
      await adapter.messages!({} as Env, account, { model: "claude-3", messages: [] }, new Headers())

      expect(capturedUrl).toBe("https://upstream.example.com/v1/messages")
      const headers = capturedInit?.headers as Record<string, string>
      expect(headers["x-api-key"]).toBe("sk-ant-test-key")
      expect(headers.authorization).toBeUndefined()
    })

    it("preserves cache_control and thinking in the body verbatim", async () => {
      let sentBody: Record<string, unknown> | undefined
      globalThis.fetch = (async (_url: string, init?: RequestInit) => {
        sentBody = JSON.parse(String(init?.body))
        return new Response("{}", { status: 200 })
      }) as typeof fetch

      const clientBody = {
        model: "claude-3",
        messages: [
          {
            role: "user",
            content: [{ type: "text", text: "hi", cache_control: { type: "ephemeral" } }],
          },
        ],
        thinking: { type: "enabled", budget_tokens: 2048 },
        system: [{ type: "text", text: "sys", cache_control: { type: "ephemeral" } }],
      }
      const adapter = createCustomAnthropicAdapter(row)
      await adapter.messages!({} as Env, account, clientBody, new Headers())

      expect(sentBody).toEqual(clientBody)
    })

    it("defaults anthropic-version to 2023-06-01 when the client sends none", async () => {
      let capturedInit: RequestInit | undefined
      globalThis.fetch = (async (_url: string, init?: RequestInit) => {
        capturedInit = init
        return new Response("{}", { status: 200 })
      }) as typeof fetch

      const adapter = createCustomAnthropicAdapter(row)
      await adapter.messages!({} as Env, account, { model: "claude-3", messages: [] }, new Headers())

      const headers = capturedInit?.headers as Record<string, string>
      expect(headers["anthropic-version"]).toBe("2023-06-01")
    })

    it("forwards the client's anthropic-version when present", async () => {
      let capturedInit: RequestInit | undefined
      globalThis.fetch = (async (_url: string, init?: RequestInit) => {
        capturedInit = init
        return new Response("{}", { status: 200 })
      }) as typeof fetch

      const adapter = createCustomAnthropicAdapter(row)
      await adapter.messages!(
        {} as Env,
        account,
        { model: "claude-3", messages: [] },
        new Headers({ "anthropic-version": "2024-01-01" }),
      )

      const headers = capturedInit?.headers as Record<string, string>
      expect(headers["anthropic-version"]).toBe("2024-01-01")
    })

    it("forwards the client's anthropic-beta verbatim, with no base betas added", async () => {
      let capturedInit: RequestInit | undefined
      globalThis.fetch = (async (_url: string, init?: RequestInit) => {
        capturedInit = init
        return new Response("{}", { status: 200 })
      }) as typeof fetch

      const adapter = createCustomAnthropicAdapter(row)
      await adapter.messages!(
        {} as Env,
        account,
        { model: "claude-3", messages: [] },
        new Headers({ "anthropic-beta": "some-client-beta-2026" }),
      )

      const headers = capturedInit?.headers as Record<string, string>
      expect(headers["anthropic-beta"]).toBe("some-client-beta-2026")
    })

    it("omits anthropic-beta entirely when the client sends none — never injects OAuth betas", async () => {
      let capturedInit: RequestInit | undefined
      globalThis.fetch = (async (_url: string, init?: RequestInit) => {
        capturedInit = init
        return new Response("{}", { status: 200 })
      }) as typeof fetch

      const adapter = createCustomAnthropicAdapter(row)
      await adapter.messages!({} as Env, account, { model: "claude-3", messages: [] }, new Headers())

      const headers = capturedInit?.headers as Record<string, string>
      expect(headers["anthropic-beta"]).toBeUndefined()
      expect(JSON.stringify(headers)).not.toContain("oauth-")
      expect(JSON.stringify(headers)).not.toContain("claude-code-")
    })

    it("does not prepend a Claude Code system line", async () => {
      let sentBody: Record<string, unknown> | undefined
      globalThis.fetch = (async (_url: string, init?: RequestInit) => {
        sentBody = JSON.parse(String(init?.body))
        return new Response("{}", { status: 200 })
      }) as typeof fetch

      const adapter = createCustomAnthropicAdapter(row)
      await adapter.messages!(
        {} as Env,
        account,
        { model: "claude-3", messages: [], system: "be terse" },
        new Headers(),
      )

      expect(sentBody?.system).toBe("be terse")
    })
  })

  describe("countTokens()", () => {
    it("posts to {base}/v1/messages/count_tokens with the same auth construction as messages()", async () => {
      let capturedUrl: string | undefined
      let capturedInit: RequestInit | undefined
      globalThis.fetch = (async (url: string, init?: RequestInit) => {
        capturedUrl = url
        capturedInit = init
        return new Response(JSON.stringify({ input_tokens: 12 }), { status: 200 })
      }) as typeof fetch

      const adapter = createCustomAnthropicAdapter(row)
      const res = await adapter.countTokens!(
        {} as Env,
        account,
        { model: "claude-3", messages: [{ role: "user", content: "hi" }] },
        new Headers({ "anthropic-beta": "x-beta" }),
      )

      expect(capturedUrl).toBe("https://upstream.example.com/v1/messages/count_tokens")
      const headers = capturedInit?.headers as Record<string, string>
      expect(headers["x-api-key"]).toBe("sk-ant-test-key")
      expect(headers["anthropic-beta"]).toBe("x-beta")
      expect(res.status).toBe(200)
    })
  })

  describe("chatCompletions() — /openai/v1 surface via the shared converters", () => {
    it("converts the OpenAI request into an Anthropic Messages body", async () => {
      let sentBody: Record<string, unknown> | undefined
      globalThis.fetch = (async (_url: string, init?: RequestInit) => {
        sentBody = JSON.parse(String(init?.body))
        return new Response(
          JSON.stringify({
            id: "msg_1",
            content: [{ type: "text", text: "hello" }],
            stop_reason: "end_turn",
            usage: { input_tokens: 3, output_tokens: 2 },
          }),
          { status: 200 },
        )
      }) as typeof fetch

      const adapter = createCustomAnthropicAdapter(row)
      const res = await adapter.chatCompletions({} as Env, account, {
        model: "my-claude/claude-3",
        rawModel: "my-claude/claude-3",
        upstreamModel: "claude-3",
        messages: [{ role: "user", content: "hi" }],
        max_tokens: 512,
        reasoning_effort: "high",
        rawBody: {},
      })

      expect(sentBody?.model).toBe("claude-3")
      expect(sentBody?.max_tokens).toBe(512)
      // reasoning_effort is dropped on this surface — no thinking/output_config synthesized.
      expect(sentBody).not.toHaveProperty("thinking")
      expect(sentBody).not.toHaveProperty("output_config")
      const json = await res.json()
      expect(json).toMatchObject({ choices: [{ message: { content: "hello" } }] })
    })

    it("sends no anthropic-beta header on this surface", async () => {
      let capturedInit: RequestInit | undefined
      globalThis.fetch = (async (_url: string, init?: RequestInit) => {
        capturedInit = init
        return new Response(JSON.stringify({ content: [] }), { status: 200 })
      }) as typeof fetch

      const adapter = createCustomAnthropicAdapter(row)
      await adapter.chatCompletions({} as Env, account, {
        model: "my-claude/claude-3",
        rawModel: "my-claude/claude-3",
        upstreamModel: "claude-3",
        messages: [{ role: "user", content: "hi" }],
        rawBody: {},
      })

      const headers = capturedInit?.headers as Record<string, string>
      expect(headers["anthropic-beta"]).toBeUndefined()
    })
  })

  describe("listModels()", () => {
    it("GETs {base}/v1/models with x-api-key auth", async () => {
      let capturedUrl: string | undefined
      globalThis.fetch = (async (url: string) => {
        capturedUrl = url
        return new Response(
          JSON.stringify({ data: [{ id: "claude-3", display_name: "Claude 3" }] }),
          { status: 200 },
        )
      }) as typeof fetch

      const adapter = createCustomAnthropicAdapter(row)
      const result = await adapter.listModels!({} as Env, account)
      expect(capturedUrl).toBe("https://upstream.example.com/v1/models")
      expect(result.models).toEqual([{ id: "claude-3", display_name: "Claude 3" }])
    })
  })
})
