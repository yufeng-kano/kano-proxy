/**
 * CLI providers on the routing surface (docs/cli.md § Failover semantics):
 * the third prefix branch, the DO-stub transport, the tri-state fault guard,
 * and the agent-reported catalog.
 */
import { describe, expect, it } from "vitest"
import { encryptJson } from "../src/crypto/token_crypto"
import { listModelsForUser } from "../src/catalog/models"
import type { Env } from "../src/env"
import { agentFaultVerdict } from "../src/routing/feedback"
import { resolveCandidates } from "../src/routing/candidates"
import { dispatchAnthropicMessages, dispatchChatCompletions } from "../src/proxy/dispatch"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

const TOKEN_KEY = "test-token-encryption-key-not-secret"

function fakeTunnelNamespace(handler: (req: Request) => Response | Promise<Response>): {
  namespace: DurableObjectNamespace
  requests: Request[]
} {
  const requests: Request[] = []
  const namespace = {
    idFromName: (name: string) => ({ name }),
    get: () => ({
      fetch: async (input: RequestInfo | URL, init?: RequestInit) => {
        const req = new Request(input as RequestInfo, init)
        requests.push(req)
        return handler(req)
      },
    }),
  } as unknown as DurableObjectNamespace
  return { namespace, requests }
}

function buildEnv(db: FakeD1, namespace?: DurableObjectNamespace): Env {
  return {
    DB: db as unknown as D1Database,
    BENCH: fakeKV(),
    CACHE: fakeKV(),
    AGENT_TUNNEL: namespace,
    APP_URL: "https://app.example.com",
    TOKEN_ENCRYPTION_KEY: TOKEN_KEY,
  } as unknown as Env
}

async function seedCliProvider(
  db: FakeD1,
  overrides: Partial<{ slug: string; format: string; models_json: string | null; model_filter_json: string | null }> = {},
): Promise<void> {
  db.seed("cli_providers", [
    {
      id: "cliprov_1",
      user_id: "user_1",
      device_id: "clidev_1",
      slug: overrides.slug ?? "my-mac",
      name: "My Mac",
      format: overrides.format ?? "openai",
      models_json: overrides.models_json ?? null,
      models_updated_at: null,
      model_filter_json: overrides.model_filter_json ?? null,
      sort_order: 0,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
  db.seed("upstream_accounts", [
    {
      id: "acc_cli_1",
      user_id: "user_1",
      provider: overrides.slug ?? "my-mac",
      external_account_id: null,
      label: "My Mac",
      custom_label: null,
      priority: 1,
      encrypted_payload: await encryptJson(TOKEN_KEY, { access_token: "" }),
      account_meta_json: null,
      usage_snapshot_json: null,
      usage_fetched_at: null,
      usage_fetching_at: null,
      bench_until: null,
      bench_reason: null,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
}

const noopWaitUntil = () => {}

describe("routing third branch", () => {
  it("resolves a CLI slug after builtin and custom misses", async () => {
    const db = new FakeD1()
    await seedCliProvider(db)
    const env = buildEnv(db)
    const resolution = await resolveCandidates(env, "user_1", "my-mac/llama3.3:70b")
    expect(resolution).not.toBeNull()
    expect(resolution!.primary.provider).toBe("my-mac")
    expect(resolution!.primary.upstreamModel).toBe("llama3.3:70b")
    expect(resolution!.primary.isBuiltin).toBe(false)
    expect(resolution!.candidates).toHaveLength(1)
  })

  it("still returns null for a slug that exists nowhere", async () => {
    const db = new FakeD1()
    const env = buildEnv(db)
    expect(await resolveCandidates(env, "user_1", "ghost/model")).toBeNull()
  })
})

describe("dispatch over the DO stub", () => {
  it("passes a local answer through and sends only the bare suffix + format header", async () => {
    const db = new FakeD1()
    await seedCliProvider(db)
    const { namespace, requests } = fakeTunnelNamespace(() =>
      new Response(JSON.stringify({ id: "cmpl", usage: { prompt_tokens: 3, completion_tokens: 5 } }), {
        status: 200,
        headers: { "x-agent-upstream": "1", "content-type": "application/json" },
      }),
    )
    const env = buildEnv(db, namespace)
    const resolution = (await resolveCandidates(env, "user_1", "my-mac/llama3"))!
    const res = await dispatchChatCompletions(env, {
      userId: "user_1",
      apiKeyId: "key_1",
      provider: resolution.primary.provider,
      adapter: resolution.primary.adapter,
      candidates: resolution.candidates,
      strategy: resolution.strategy,
      isBuiltin: false,
      waitUntil: noopWaitUntil,
      req: {
        model: "my-mac/llama3",
        rawModel: "my-mac/llama3",
        upstreamModel: "llama3",
        messages: [{ role: "user", content: "hi" }],
        rawBody: { model: "my-mac/llama3", messages: [{ role: "user", content: "hi" }] },
      },
    })
    expect(res.status).toBe(200)
    const sent = requests[0]!
    expect(new URL(sent.url).pathname).toBe("/chat/completions")
    expect(sent.headers.get("x-kano-format")).toBe("openai")
    // The placeholder credential's auth header is stripped before the tunnel.
    expect(sent.headers.get("authorization")).toBeNull()
    const body = (await sent.json()) as Record<string, unknown>
    expect(body.model).toBe("llama3")
    const log = db.rows("request_logs")[0]!
    expect(log.provider).toBe("my-mac")
    expect(log.prompt_tokens).toBe(3)
  })

  it("fault offline benches the internal account row 60s and synthesizes 503", async () => {
    const db = new FakeD1()
    await seedCliProvider(db)
    const { namespace } = fakeTunnelNamespace(
      () =>
        new Response(JSON.stringify({ error: { type: "agent_fault", reason: "offline" } }), {
          status: 502,
          headers: { "x-agent-fault": "offline", "content-type": "application/json" },
        }),
    )
    const env = buildEnv(db, namespace)
    const resolution = (await resolveCandidates(env, "user_1", "my-mac/llama3"))!
    const before = Date.now()
    const res = await dispatchChatCompletions(env, {
      userId: "user_1",
      apiKeyId: "key_1",
      provider: resolution.primary.provider,
      adapter: resolution.primary.adapter,
      candidates: resolution.candidates,
      strategy: resolution.strategy,
      isBuiltin: false,
      waitUntil: noopWaitUntil,
      req: {
        model: "my-mac/llama3",
        rawModel: "my-mac/llama3",
        upstreamModel: "llama3",
        messages: [],
        rawBody: { model: "my-mac/llama3", messages: [] },
      },
    })
    expect(res.status).toBe(503)
    const account = db.rows("upstream_accounts")[0]!
    expect(account.bench_reason).toBe("offline")
    const until = Date.parse(account.bench_until as string)
    expect(until - before).toBeGreaterThan(50_000)
    expect(until - before).toBeLessThanOrEqual(61_000)
  })

  it("fault busy fails over without benching", async () => {
    const db = new FakeD1()
    await seedCliProvider(db)
    const { namespace } = fakeTunnelNamespace(
      () =>
        new Response(JSON.stringify({ error: { type: "agent_fault", reason: "busy" } }), {
          status: 502,
          headers: { "x-agent-fault": "busy", "content-type": "application/json" },
        }),
    )
    const env = buildEnv(db, namespace)
    const resolution = (await resolveCandidates(env, "user_1", "my-mac/llama3"))!
    const res = await dispatchChatCompletions(env, {
      userId: "user_1",
      apiKeyId: "key_1",
      provider: resolution.primary.provider,
      adapter: resolution.primary.adapter,
      candidates: resolution.candidates,
      strategy: resolution.strategy,
      isBuiltin: false,
      waitUntil: noopWaitUntil,
      req: {
        model: "my-mac/llama3",
        rawModel: "my-mac/llama3",
        upstreamModel: "llama3",
        messages: [],
        rawBody: { model: "my-mac/llama3", messages: [] },
      },
    })
    expect(res.status).toBe(503)
    expect(db.rows("upstream_accounts")[0]!.bench_until).toBeNull()
  })

  it("anthropic-format CLI provider is a native /v1/messages passthrough", async () => {
    const db = new FakeD1()
    await seedCliProvider(db, { slug: "my-box", format: "anthropic" })
    const { namespace, requests } = fakeTunnelNamespace(() =>
      new Response(JSON.stringify({ id: "msg_1", usage: { input_tokens: 2, output_tokens: 4 } }), {
        status: 200,
        headers: { "x-agent-upstream": "1", "content-type": "application/json" },
      }),
    )
    const env = buildEnv(db, namespace)
    const resolution = (await resolveCandidates(env, "user_1", "my-box/some-model"))!
    const res = await dispatchAnthropicMessages(env, {
      userId: "user_1",
      apiKeyId: "key_1",
      model: "my-box/some-model",
      provider: "my-box",
      adapter: resolution.primary.adapter,
      candidates: resolution.candidates,
      strategy: resolution.strategy,
      isBuiltin: false,
      body: {
        model: "my-box/some-model",
        max_tokens: 16,
        messages: [{ role: "user", content: [{ type: "text", text: "hi", cache_control: { type: "ephemeral" } }] }],
      },
      headers: new Headers({ "anthropic-version": "2023-06-01", "anthropic-beta": "some-beta" }),
      waitUntil: noopWaitUntil,
    })
    expect(res.status).toBe(200)
    const sent = requests[0]!
    expect(new URL(sent.url).pathname).toBe("/v1/messages")
    expect(sent.headers.get("x-kano-format")).toBe("anthropic")
    expect(sent.headers.get("x-api-key")).toBeNull()
    expect(sent.headers.get("anthropic-beta")).toBe("some-beta")
    const body = (await sent.json()) as Record<string, unknown>
    expect(body.model).toBe("some-model")
    // cache_control passes through untouched — native passthrough semantics.
    const content = (body.messages as any[])[0].content
    expect(content[0].cache_control).toEqual({ type: "ephemeral" })
  })

  it("faults offline without a tunnel binding", async () => {
    const db = new FakeD1()
    await seedCliProvider(db)
    const env = buildEnv(db)
    const resolution = (await resolveCandidates(env, "user_1", "my-mac/llama3"))!
    const res = await dispatchChatCompletions(env, {
      userId: "user_1",
      apiKeyId: "key_1",
      provider: resolution.primary.provider,
      adapter: resolution.primary.adapter,
      candidates: resolution.candidates,
      strategy: resolution.strategy,
      isBuiltin: false,
      waitUntil: noopWaitUntil,
      req: {
        model: "my-mac/llama3",
        rawModel: "my-mac/llama3",
        upstreamModel: "llama3",
        messages: [],
        rawBody: { model: "my-mac/llama3", messages: [] },
      },
    })
    expect(res.status).toBe(503)
  })
})

describe("agent-reported catalog", () => {
  it("lists stored models with the expose filter applied at read time", async () => {
    const db = new FakeD1()
    await seedCliProvider(db, {
      models_json: JSON.stringify(["llama3.3:70b", "qwen3:8b", "hidden"]),
      model_filter_json: JSON.stringify(["llama3.3:70b", "qwen3:8b"]),
    })
    const env = buildEnv(db)
    const { models, providers } = await listModelsForUser(env, "user_1")
    const section = providers.find((s) => s.provider === "my-mac")!
    expect(section.models.map((m) => m.id)).toEqual(["my-mac/llama3.3:70b", "my-mac/qwen3:8b"])
    expect(models.some((m) => m.id === "my-mac/hidden")).toBe(false)
  })

  it("a never-connected provider shows an empty section, never a fabricated one", async () => {
    const db = new FakeD1()
    await seedCliProvider(db)
    const env = buildEnv(db)
    const { providers } = await listModelsForUser(env, "user_1")
    const section = providers.find((s) => s.provider === "my-mac")!
    expect(section.models).toEqual([])
    expect(section.error).toBeNull()
  })
})

describe("agentFaultVerdict", () => {
  it("classifies fault vs upstream markers", () => {
    expect(agentFaultVerdict(new Headers({ "x-agent-fault": "offline" }))).toMatchObject({
      failover: true,
      benchMs: 60_000,
    })
    expect(agentFaultVerdict(new Headers({ "x-agent-fault": "busy" }))).toMatchObject({
      failover: true,
      benchMs: null,
    })
    expect(agentFaultVerdict(new Headers({ "x-agent-upstream": "1" }))).toBeNull()
    expect(agentFaultVerdict(new Headers())).toBeNull()
  })
})
