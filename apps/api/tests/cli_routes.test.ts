/** /api/cli session-side management (docs/auth.md § CLI devices and providers). */
import { describe, expect, it } from "vitest"
import { cliRoutes } from "../src/routes/cli"
import { createSession } from "../src/auth/session"
import type { Env } from "../src/env"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

const SESSION_SECRET = "test-session-secret-not-real"

function buildEnv(db: FakeD1): Env {
  return {
    DB: db as unknown as D1Database,
    BENCH: fakeKV(),
    CACHE: fakeKV(),
    APP_URL: "https://app.example.com",
    SESSION_SECRET,
    TOKEN_ENCRYPTION_KEY: "test-token-encryption-key-not-secret",
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

function seedDevice(db: FakeD1, id = "clidev_1", userId = "user_1"): void {
  db.seed("cli_devices", [
    {
      id,
      user_id: userId,
      name: "my-mac",
      refresh_token_hash: "hash",
      refresh_token_prev_hash: null,
      last_seen_at: null,
      created_at: "2026-01-01T00:00:00.000Z",
      revoked_at: null,
    },
  ])
}

function seedProvider(db: FakeD1, id = "cliprov_1", userId = "user_1"): void {
  db.seed("cli_providers", [
    {
      id,
      user_id: userId,
      device_id: "clidev_1",
      slug: "my-mac",
      name: "My Mac",
      format: "openai",
      models_json: JSON.stringify(["llama3"]),
      models_updated_at: "2026-01-02T00:00:00.000Z",
      model_filter_json: null,
      sort_order: 0,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
  db.seed("upstream_accounts", [
    {
      id: "acc_1",
      user_id: userId,
      provider: "my-mac",
      external_account_id: null,
      label: "My Mac",
      custom_label: null,
      priority: 1,
      encrypted_payload: "irrelevant",
      account_meta_json: null,
      usage_snapshot_json: null,
      usage_fetched_at: null,
      usage_fetching_at: null,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
}

async function cookieFor(env: Env, userId = "user_1"): Promise<string> {
  const { cookie } = await createSession(env, userId)
  return cookie.split(";")[0]!
}

/** Response bodies here are dynamic test JSON — typed loosely on purpose. */
async function readJson(res: Response): Promise<any> {
  return res.json()
}

function req(method: string, cookie: string, body?: unknown): RequestInit {
  return {
    method,
    headers: { "content-type": "application/json", cookie },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  }
}

describe("devices", () => {
  it("lists the caller's devices only", async () => {
    const db = new FakeD1()
    seedUser(db)
    seedUser(db, "user_2")
    seedDevice(db)
    seedDevice(db, "clidev_other", "user_2")
    const env = buildEnv(db)
    const cookie = await cookieFor(env)
    const res = await cliRoutes.request("/devices", { headers: { cookie } }, env)
    const { devices } = await readJson(res)
    expect(devices).toHaveLength(1)
    expect(devices[0]).toMatchObject({ id: "clidev_1", name: "my-mac", revoked_at: null })
  })

  it("revokes idempotently and never a foreign device", async () => {
    const db = new FakeD1()
    seedUser(db)
    seedUser(db, "user_2")
    seedDevice(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env)
    const res = await cliRoutes.request("/devices/clidev_1/revoke", req("POST", cookie), env)
    expect(res.status).toBe(200)
    expect(db.rows("cli_devices")[0]!.revoked_at).toBeTruthy()
    // Second revoke stays ok.
    expect((await cliRoutes.request("/devices/clidev_1/revoke", req("POST", cookie), env)).status).toBe(200)

    const other = await cookieFor(env, "user_2")
    const foreign = await cliRoutes.request("/devices/clidev_1/revoke", req("POST", other), env)
    expect(foreign.status).toBe(404)
  })
})

describe("providers", () => {
  it("lists with registered-from device name and stored models", async () => {
    const db = new FakeD1()
    seedUser(db)
    seedDevice(db)
    seedProvider(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env)
    const res = await cliRoutes.request("/providers", { headers: { cookie } }, env)
    const { providers } = await readJson(res)
    expect(providers[0]).toMatchObject({
      slug: "my-mac",
      device_name: "my-mac",
      connected: false,
      models: ["llama3"],
      models_reported: 1,
    })
  })

  it("renames the display name only", async () => {
    const db = new FakeD1()
    seedUser(db)
    seedProvider(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env)
    const res = await cliRoutes.request("/providers/cliprov_1", req("PATCH", cookie, { name: "Studio box" }), env)
    expect(res.status).toBe(200)
    const row = db.rows("cli_providers")[0]!
    expect(row.name).toBe("Studio box")
    expect(row.slug).toBe("my-mac")

    const bad = await cliRoutes.request("/providers/cliprov_1", req("PATCH", cookie, { name: "" }), env)
    expect(bad.status).toBe(400)
  })

  it("delete removes the provider and its internal account rows", async () => {
    const db = new FakeD1()
    seedUser(db)
    seedProvider(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env)
    const res = await cliRoutes.request("/providers/cliprov_1", req("DELETE", cookie), env)
    expect(res.status).toBe(200)
    expect(db.rows("cli_providers")).toHaveLength(0)
    expect(db.rows("upstream_accounts")).toHaveLength(0)
  })
})

describe("login requests (authorize view)", () => {
  function seedRequest(db: FakeD1, overrides: Record<string, unknown> = {}): void {
    db.seed("cli_login_requests", [
      {
        id: "clireq_1",
        device_name: "new-box",
        code_hash: null,
        user_id: null,
        expires_at: new Date(Date.now() + 60_000).toISOString(),
        approved_at: null,
        used_at: null,
        attempts: 0,
        created_at: "2026-01-01T00:00:00.000Z",
        ...overrides,
      },
    ])
  }

  it("reads a pending request and 404s expired ones", async () => {
    const db = new FakeD1()
    seedUser(db)
    seedRequest(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env)
    const res = await cliRoutes.request("/login-requests/clireq_1", { headers: { cookie } }, env)
    expect(await readJson(res)).toMatchObject({ device_name: "new-box", approved: false })

    db.rows("cli_login_requests")[0]!.expires_at = new Date(Date.now() - 1000).toISOString()
    const expired = await cliRoutes.request("/login-requests/clireq_1", { headers: { cookie } }, env)
    expect(expired.status).toBe(404)
  })

  it("approve returns the code exactly once and stores only its hash", async () => {
    const db = new FakeD1()
    seedUser(db)
    seedRequest(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env)
    const res = await cliRoutes.request("/login-requests/clireq_1/approve", req("POST", cookie), env)
    const { code } = await readJson(res)
    expect(code).toMatch(/^[A-Z2-9]{4}-[A-Z2-9]{4}$/)
    const row = db.rows("cli_login_requests")[0]!
    expect(row.user_id).toBe("user_1")
    expect(row.code_hash).not.toContain(code.slice(0, 4))

    const again = await cliRoutes.request("/login-requests/clireq_1/approve", req("POST", cookie), env)
    expect(again.status).toBe(400)
  })

  it("deny deletes the request", async () => {
    const db = new FakeD1()
    seedUser(db)
    seedRequest(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env)
    const res = await cliRoutes.request("/login-requests/clireq_1/deny", req("POST", cookie), env)
    expect(res.status).toBe(200)
    expect(db.rows("cli_login_requests")).toHaveLength(0)
  })
})
