import type { Context } from "hono"
import type { HonoEnv } from "../src/core"
import { describe, expect, it, vi } from "vitest"
import { createApplication } from "../src/core"
import { hashApiKey } from "../src/crypto/keys"
import type { Env } from "../src/env"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

async function setup() {
  const db = new FakeD1()
  db.seed("api_keys", [{ id: "key", user_id: "owner", key_hash: await hashApiKey("test-key") }])
  const env = { DB: db, CACHE: fakeKV(), BENCH: fakeKV(), APP_URL: "https://app.example.com" } as unknown as Env
  const ctx = { waitUntil: (p: Promise<unknown>) => { void p.catch(() => {}) }, passThroughOnException() {} } as ExecutionContext
  return { env, ctx }
}

describe("edition composition", () => {
  it("keeps extension routes and policy private to an application instance", async () => {
    const { env, ctx } = await setup()
    const policy = vi.fn(async (c: Context<HonoEnv>) => c.json({ owner: c.get("apiKeyUserId") }, 402))
    const hosted = createApplication({ requestPolicy: policy, registerRoutes: app => { app.get("/api/edition", c => c.json({ hosted: true })) } })
    const standalone = createApplication()
    expect((await hosted.request("/api/edition", {}, env, ctx)).status).toBe(200)
    expect((await standalone.request("/api/edition", {}, env, ctx)).status).toBe(404)
    const init = { method: "POST", headers: { authorization: "Bearer test-key", "content-type": "application/json" }, body: "{}" }
    expect((await hosted.request("/openai/v1/responses", init, env, ctx)).status).toBe(402)
    expect((await standalone.request("/openai/v1/responses", init, env, ctx)).status).toBe(400)
    expect(policy).toHaveBeenCalledTimes(1)
  })

  it.each([
    "/openai/v1/chat/completions", "/openai/v1/responses", "/openai/v1/audio/transcriptions",
    "/anthropic/v1/messages", "/anthropic/v1/messages/count_tokens",
    "/g/test/openai/v1/chat/completions", "/g/test/openai/v1/responses", "/g/test/openai/v1/audio/transcriptions",
    "/g/test/anthropic/v1/messages", "/g/test/anthropic/v1/messages/count_tokens",
  ])("authenticates before invoking the policy on %s", async path => {
    const { env, ctx } = await setup()
    const policy = vi.fn(async (c: Context<HonoEnv>) => c.json({ owner: c.get("apiKeyUserId"), key: c.get("apiKeyId") }, 402))
    const app = createApplication({ requestPolicy: policy })
    expect((await app.request(path, { method: "POST" }, env, ctx)).status).toBe(401)
    expect(policy).not.toHaveBeenCalled()
    const res = await app.request(path, { method: "POST", headers: { authorization: "Bearer test-key" } }, env, ctx)
    expect(await res.json()).toEqual({ owner: "owner", key: "key" })
    expect(res.status).toBe(402)
    expect(policy).toHaveBeenCalledTimes(1)
  })
})
