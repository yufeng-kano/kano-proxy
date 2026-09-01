/** /agent/v1 device auth + provider CRUD (docs/cli.md § Server routes). */
import { describe, expect, it } from "vitest"
import { agentRoutes } from "../src/routes/agent"
import { cliRoutes } from "../src/routes/cli"
import { createSession } from "../src/auth/session"
import type { Env } from "../src/env"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

const SESSION_SECRET = "test-session-secret-not-real"
const TOKEN_KEY = "test-token-encryption-key-not-secret"
const CLI_SECRET = "test-cli-token-secret-not-real"
const APP_URL = "https://app.example.com"

function buildEnv(db: FakeD1): Env {
  return {
    DB: db as unknown as D1Database,
    BENCH: fakeKV(),
    CACHE: fakeKV(),
    APP_URL,
    SESSION_SECRET,
    TOKEN_ENCRYPTION_KEY: TOKEN_KEY,
    CLI_TOKEN_SECRET: CLI_SECRET,
  } as unknown as Env
}

function seedUser(db: FakeD1, id = "user_1"): void {
  db.seed("users", [
    {
      id,
      google_sub: `sub-${id}`,
      email: `${id}@example.com`,
      name: "Test User",
      picture_url: null,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
}

async function cookieFor(env: Env, userId: string): Promise<string> {
  const { cookie } = await createSession(env, userId)
  return cookie.split(";")[0]!
}

/** Response bodies here are dynamic test JSON — typed loosely on purpose. */
async function readJson(res: Response): Promise<any> {
  return res.json()
}

function jsonReq(method: string, body?: unknown, headers: Record<string, string> = {}): RequestInit {
  return {
    method,
    headers: { "content-type": "application/json", ...headers },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  }
}

/** Full login dance: start → session approve → complete. Returns the token pair. */
async function signInDevice(env: Env, db: FakeD1, userId = "user_1", deviceName = "test-box") {
  const startRes = await agentRoutes.request("/login/start", jsonReq("POST", { device_name: deviceName }), env)
  expect(startRes.status).toBe(200)
  const start = await readJson(startRes)
  expect(start.verify_url).toContain(`${APP_URL}/cli/authorize?request=`)

  const cookie = await cookieFor(env, userId)
  const approveRes = await cliRoutes.request(
    `/login-requests/${start.request_id}/approve`,
    jsonReq("POST", undefined, { cookie }),
    env,
  )
  expect(approveRes.status).toBe(200)
  const { code } = await readJson(approveRes)
  expect(code).toMatch(/^[A-Z2-9]{4}-[A-Z2-9]{4}$/)

  const completeRes = await agentRoutes.request(
    "/login/complete",
    jsonReq("POST", { request_id: start.request_id, code }),
    env,
  )
  expect(completeRes.status).toBe(200)
  return readJson(completeRes)
}

describe("device login flow", () => {
  it("start → approve → complete mints a device with tokens", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const tokens = await signInDevice(env, db)
    expect(tokens.device_id).toMatch(/^clidev_/)
    expect(tokens.refresh_token).toMatch(/^kpr_/)
    expect(tokens.access_token).toContain(".")
    expect(tokens.expires_in).toBe(3600)
    const device = db.rows("cli_devices")[0]!
    expect(device.user_id).toBe("user_1")
    expect(device.name).toBe("test-box")
    // Only the hash is stored, never the token.
    expect(device.refresh_token_hash).not.toContain("kpr_")
  })

  it("rejects a wrong code and kills the request after 5 attempts", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const start = await readJson(
      await agentRoutes.request("/login/start", jsonReq("POST", { device_name: "box" }), env),
    )
    const cookie = await cookieFor(env, "user_1")
    await cliRoutes.request(`/login-requests/${start.request_id}/approve`, jsonReq("POST", undefined, { cookie }), env)

    for (let i = 0; i < 5; i++) {
      const res = await agentRoutes.request(
        "/login/complete",
        jsonReq("POST", { request_id: start.request_id, code: "AAAA-AAAA" }),
        env,
      )
      expect(res.status).toBe(401)
    }
    const res = await agentRoutes.request(
      "/login/complete",
      jsonReq("POST", { request_id: start.request_id, code: "AAAA-AAAA" }),
      env,
    )
    expect((await readJson(res)).error).toContain("too many wrong codes")
  })

  it("a code redeems exactly once", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const start = await readJson(
      await agentRoutes.request("/login/start", jsonReq("POST", { device_name: "box" }), env),
    )
    const cookie = await cookieFor(env, "user_1")
    const { code } = await readJson(
      await cliRoutes.request(`/login-requests/${start.request_id}/approve`, jsonReq("POST", undefined, { cookie }), env),
    )
    const first = await agentRoutes.request(
      "/login/complete",
      jsonReq("POST", { request_id: start.request_id, code }),
      env,
    )
    expect(first.status).toBe(200)
    const second = await agentRoutes.request(
      "/login/complete",
      jsonReq("POST", { request_id: start.request_id, code }),
      env,
    )
    expect(second.status).toBe(401)
  })

  it("rate-limits login starts per IP and fails closed", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const headers = { "cf-connecting-ip": "203.0.113.9" }
    for (let i = 0; i < 10; i++) {
      const res = await agentRoutes.request("/login/start", jsonReq("POST", { device_name: "box" }, headers), env)
      expect(res.status).toBe(200)
    }
    const res = await agentRoutes.request("/login/start", jsonReq("POST", { device_name: "box" }, headers), env)
    expect(res.status).toBe(429)
  })
})

describe("refresh token rotation", () => {
  it("rotates on every use and keeps one generation of history", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const tokens = await signInDevice(env, db)

    const res = await agentRoutes.request("/token", jsonReq("POST", { refresh_token: tokens.refresh_token }), env)
    expect(res.status).toBe(200)
    const next = await readJson(res)
    expect(next.refresh_token).not.toBe(tokens.refresh_token)
    expect(next.access_token).toBeTruthy()
    const device = db.rows("cli_devices")[0]!
    expect(device.refresh_token_prev_hash).toBeTruthy()
  })

  it("revokes the device when a superseded token is presented", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const tokens = await signInDevice(env, db)
    await agentRoutes.request("/token", jsonReq("POST", { refresh_token: tokens.refresh_token }), env)

    const reuse = await agentRoutes.request("/token", jsonReq("POST", { refresh_token: tokens.refresh_token }), env)
    expect(reuse.status).toBe(401)
    expect((await readJson(reuse)).error).toBe("device_revoked")
    expect(db.rows("cli_devices")[0]!.revoked_at).toBeTruthy()
  })

  it("a token matching nothing is a plain 401 with no revocation", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    await signInDevice(env, db)
    const res = await agentRoutes.request("/token", jsonReq("POST", { refresh_token: "kpr_garbage" }), env)
    expect(res.status).toBe(401)
    expect(db.rows("cli_devices")[0]!.revoked_at).toBeNull()
  })

  it("a revoked device cannot refresh", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const tokens = await signInDevice(env, db)
    db.rows("cli_devices")[0]!.revoked_at = "2026-01-02T00:00:00.000Z"
    const res = await agentRoutes.request("/token", jsonReq("POST", { refresh_token: tokens.refresh_token }), env)
    expect(res.status).toBe(401)
  })
})

describe("provider CRUD over the agent surface", () => {
  async function authed(env: Env, db: FakeD1) {
    const tokens = await signInDevice(env, db)
    return { authorization: `Bearer ${tokens.access_token}` }
  }

  it("creates a provider with its internal account row", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const auth = await authed(env, db)
    const res = await agentRoutes.request(
      "/providers",
      jsonReq("POST", { slug: "my-mac", format: "openai" }, auth),
      env,
    )
    expect(res.status).toBe(201)
    const created = await readJson(res)
    expect(created.slug).toBe("my-mac")
    const provider = db.rows("cli_providers")[0]!
    expect(provider.device_id).toBe(db.rows("cli_devices")[0]!.id)
    const account = db.rows("upstream_accounts")[0]!
    expect(account.provider).toBe("my-mac")
    expect(account.user_id).toBe("user_1")
  })

  it("stores expose whitelist and initial models within report bounds", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const auth = await authed(env, db)
    const res = await agentRoutes.request(
      "/providers",
      jsonReq(
        "POST",
        { slug: "box", format: "anthropic", expose: ["a", "b"], initial_models: ["a", "b", "c"] },
        auth,
      ),
      env,
    )
    expect(res.status).toBe(201)
    const row = db.rows("cli_providers")[0]!
    expect(JSON.parse(row.models_json as string)).toEqual(["a", "b", "c"])
    expect(JSON.parse(row.model_filter_json as string)).toEqual(["a", "b"])

    const bad = await agentRoutes.request(
      "/providers",
      jsonReq("POST", { slug: "box2", format: "openai", expose: ["bad id"] }, auth),
      env,
    )
    expect(bad.status).toBe(400)
  })

  it("shares the slug namespace and the 20 cap with custom providers", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const auth = await authed(env, db)
    db.seed("custom_providers", [
      {
        id: "cprov_1",
        user_id: "user_1",
        slug: "taken",
        name: "Taken",
        format: "openai",
        base_url: "https://u.example.com/v1",
        count_tokens_url: null,
        models_mode: "auto",
        manual_models_json: null,
        sort_order: 0,
        created_at: "2026-01-01T00:00:00.000Z",
        updated_at: "2026-01-01T00:00:00.000Z",
      },
    ])
    const conflict = await agentRoutes.request(
      "/providers",
      jsonReq("POST", { slug: "taken", format: "openai" }, auth),
      env,
    )
    expect(conflict.status).toBe(409)

    const reserved = await agentRoutes.request(
      "/providers",
      jsonReq("POST", { slug: "claude-code", format: "openai" }, auth),
      env,
    )
    expect(reserved.status).toBe(400)

    for (let i = 0; i < 19; i++) {
      const res = await agentRoutes.request(
        "/providers",
        jsonReq("POST", { slug: `p-${i}`, format: "openai" }, auth),
        env,
      )
      expect(res.status).toBe(201)
    }
    const over = await agentRoutes.request(
      "/providers",
      jsonReq("POST", { slug: "one-more", format: "openai" }, auth),
      env,
    )
    expect(over.status).toBe(400)
    expect((await readJson(over)).error).toContain("maximum")
  })

  it("lists providers as disconnected without a tunnel binding", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const auth = await authed(env, db)
    await agentRoutes.request("/providers", jsonReq("POST", { slug: "my-mac", format: "openai" }, auth), env)
    const res = await agentRoutes.request("/providers", { headers: auth }, env)
    const { providers } = await readJson(res)
    expect(providers).toHaveLength(1)
    expect(providers[0]).toMatchObject({ slug: "my-mac", connected: false, device_name: "test-box" })
  })

  it("delete removes the provider and its account rows", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const auth = await authed(env, db)
    const created = await readJson(
      await agentRoutes.request("/providers", jsonReq("POST", { slug: "my-mac", format: "openai" }, auth), env),
    )
    const res = await agentRoutes.request(`/providers/${created.id}`, { method: "DELETE", headers: auth }, env)
    expect(res.status).toBe(200)
    expect(db.rows("cli_providers")).toHaveLength(0)
    expect(db.rows("upstream_accounts")).toHaveLength(0)
  })

  it("rejects a revoked device's still-valid access token", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const auth = await authed(env, db)
    db.rows("cli_devices")[0]!.revoked_at = "2026-01-02T00:00:00.000Z"
    const res = await agentRoutes.request("/providers", { headers: auth }, env)
    expect(res.status).toBe(401)
  })

  it("connect requires a websocket upgrade", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const auth = await authed(env, db)
    const created = await readJson(
      await agentRoutes.request("/providers", jsonReq("POST", { slug: "my-mac", format: "openai" }, auth), env),
    )
    const res = await agentRoutes.request(`/connect/${created.id}`, { headers: auth }, env)
    expect(res.status).toBe(426)
  })
})
