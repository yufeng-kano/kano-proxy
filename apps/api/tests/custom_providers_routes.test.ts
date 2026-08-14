import { afterEach, describe, expect, it } from "vitest"
import { customProviderRoutes } from "../src/routes/custom_providers"
import { createSession } from "../src/auth/session"
import { decryptJson } from "../src/crypto/token_crypto"
import { isBenched, markBenched } from "../src/pool/bench"
import type { StoredCredential } from "../src/pool/acquire"
import type { Env } from "../src/env"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

const SESSION_SECRET = "test-session-secret-not-real"
const TOKEN_KEY = "test-token-encryption-key-not-secret"
const APP_URL = "https://app.example.com"

function buildEnv(db: FakeD1): Env {
  return {
    DB: db as unknown as D1Database,
    BENCH: fakeKV(),
    CACHE: fakeKV(),
    APP_URL,
    SESSION_SECRET,
    TOKEN_ENCRYPTION_KEY: TOKEN_KEY,
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

function req(method: string, cookie: string, body?: unknown): RequestInit {
  return {
    method,
    headers: { "content-type": "application/json", cookie, host: "app.example.com" },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  }
}

const validCreateBody = {
  name: "My Endpoint",
  slug: "my-endpoint",
  format: "openai" as const,
  base_url: "https://upstream.example.com/v1",
  api_key: "sk-upstream-secret-key-value",
}

async function createProvider(
  env: Env,
  cookie: string,
  overrides: Partial<typeof validCreateBody & { models_mode: string; manual_models: string[] }> = {},
) {
  return customProviderRoutes.request("/", req("POST", cookie, { ...validCreateBody, ...overrides }), env)
}

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

describe("POST /api/custom-providers (create)", () => {
  it("requires auth", async () => {
    const db = new FakeD1()
    const env = buildEnv(db)
    const res = await customProviderRoutes.request("/", { method: "POST" }, env)
    expect(res.status).toBe(401)
  })

  it("creates a valid provider and returns 201 with a masked key, never the raw key", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")

    const res = await createProvider(env, cookie)
    expect(res.status).toBe(201)
    const json = await readJson(res)
    expect(json).toMatchObject({
      slug: "my-endpoint",
      name: "My Endpoint",
      format: "openai",
      base_url: "https://upstream.example.com/v1",
      models_mode: "auto",
      manual_models: [],
      status: "active",
    })
    expect(json.key_mask).toBe("sk-ups…alue")
    expect(JSON.stringify(json)).not.toContain(validCreateBody.api_key)
    // docs/auth.md: account_id is the provider's single upstream_accounts row
    // (its stored API key) — the Groups picker pins targets to it.
    expect(json.account_id).toBe(db.rows("upstream_accounts")[0]!.id)
  })

  it("account_id is null when the provider's account row is somehow missing", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const created = await readJson(await createProvider(env, cookie))

    // Simulate the account row going missing without the provider row itself
    // being deleted (should never happen via the normal API, but the field
    // must still degrade to null rather than throw).
    const accounts = db.rows("upstream_accounts")
    accounts.splice(
      accounts.findIndex((r) => r.id === created.account_id),
      1,
    )

    const res = await customProviderRoutes.request("/", req("GET", cookie), env)
    const json = await readJson(res)
    expect(json.providers[0].account_id).toBeNull()
  })

  it("stores the api key encrypted, decryptable back to the original value", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    await createProvider(env, cookie)

    const accountRows = db.rows("upstream_accounts")
    expect(accountRows).toHaveLength(1)
    const cred = await decryptJson<StoredCredential>(TOKEN_KEY, accountRows[0]!.encrypted_payload as string)
    expect(cred.access_token).toBe(validCreateBody.api_key)
  })

  it("rejects an invalid format", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createProvider(env, cookie, { format: "openrouter" as unknown as "openai" })
    expect(res.status).toBe(400)
  })

  it("rejects a reserved slug", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createProvider(env, cookie, { slug: "grok" })
    expect(res.status).toBe(400)
  })

  it("rejects a too-short slug", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createProvider(env, cookie, { slug: "a" })
    expect(res.status).toBe(400)
  })

  it("rejects an empty name", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createProvider(env, cookie, { name: "" })
    expect(res.status).toBe(400)
  })

  it("rejects an empty api_key", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createProvider(env, cookie, { api_key: "" })
    expect(res.status).toBe(400)
  })

  it("rejects an invalid models_mode", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createProvider(env, cookie, { models_mode: "live" })
    expect(res.status).toBe(400)
  })

  it("rejects manual_models with more than 100 entries", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createProvider(env, cookie, {
      manual_models: Array.from({ length: 101 }, (_, i) => `m${i}`),
    })
    expect(res.status).toBe(400)
  })

  it("stores a provided manual_models list", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createProvider(env, cookie, {
      models_mode: "manual",
      manual_models: ["model-a", "model-b"],
    })
    const json = await readJson(res)
    expect(json.manual_models).toEqual(["model-a", "model-b"])
  })

  it("includes sort_order and appends a newly created provider last", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")

    const first = await createProvider(env, cookie, { slug: "first-endpoint", name: "First" })
    const second = await createProvider(env, cookie, { slug: "second-endpoint", name: "Second" })
    expect((await readJson(first)).sort_order).toBe(0)
    expect((await readJson(second)).sort_order).toBe(1)

    const list = await customProviderRoutes.request("/", req("GET", cookie), env)
    const json = await readJson(list)
    expect(json.providers.map((p: any) => p.slug)).toEqual(["first-endpoint", "second-endpoint"])
    expect(json.providers.map((p: any) => p.sort_order)).toEqual([0, 1])
  })

  it("rejects a base_url over 300 characters", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createProvider(env, cookie, {
      base_url: "https://upstream.example.com/" + "a".repeat(290),
    })
    expect(res.status).toBe(400)
  })

  it("rejects a non-https base_url (SSRF/loop guard wired in)", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createProvider(env, cookie, { base_url: "http://upstream.example.com/v1" })
    expect(res.status).toBe(400)
  })

  it("rejects a base_url pointing at this deploy's own APP_URL host", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createProvider(env, cookie, { base_url: `${APP_URL}/v1` })
    expect(res.status).toBe(400)
  })

  it("rejects a duplicate slug for the same user with 409", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    await createProvider(env, cookie)
    const res = await createProvider(env, cookie, { name: "Second" })
    expect(res.status).toBe(409)
  })

  it("allows the same slug for a different user", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    seedUser(db, "user_2")
    const env = buildEnv(db)
    const cookie1 = await cookieFor(env, "user_1")
    const cookie2 = await cookieFor(env, "user_2")
    await createProvider(env, cookie1)
    const res = await createProvider(env, cookie2)
    expect(res.status).toBe(201)
  })

  it("enforces the 20-provider-per-user cap", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const now = "2026-01-01T00:00:00.000Z"
    db.seed(
      "custom_providers",
      Array.from({ length: 20 }, (_, i) => ({
        id: `cprov_${i}`,
        user_id: "user_1",
        slug: `existing-${i}`,
        name: `Existing ${i}`,
        format: "openai",
        base_url: "https://upstream.example.com/v1",
        models_mode: "auto",
        manual_models_json: null,
        created_at: now,
        updated_at: now,
      })),
    )
    const res = await createProvider(env, cookie, { slug: "one-too-many" })
    expect(res.status).toBe(400)
  })
})

describe("GET /api/custom-providers (list)", () => {
  it("requires auth", async () => {
    const db = new FakeD1()
    const res = await customProviderRoutes.request("/", { method: "GET" }, buildEnv(db))
    expect(res.status).toBe(401)
  })

  it("lists a created provider with a masked key and status active", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    await createProvider(env, cookie)

    const res = await customProviderRoutes.request("/", req("GET", cookie), env)
    const json = await readJson(res)
    expect(json.providers).toHaveLength(1)
    expect(json.providers[0]).toMatchObject({ slug: "my-endpoint", status: "active" })
    expect(json.providers[0].key_mask).toBe("sk-ups…alue")
    expect(json.providers[0].account_id).toBe(db.rows("upstream_accounts")[0]!.id)
    expect(JSON.stringify(json)).not.toContain(validCreateBody.api_key)
  })

  it("reports status benched when the provider's only account is benched", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    await createProvider(env, cookie)
    const accountId = db.rows("upstream_accounts")[0]!.id as string
    await markBenched(env, "user_1", "my-endpoint", accountId)

    const res = await customProviderRoutes.request("/", req("GET", cookie), env)
    const json = await readJson(res)
    expect(json.providers[0].status).toBe("benched")
  })

  it("only lists the requesting user's providers", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    seedUser(db, "user_2")
    const env = buildEnv(db)
    const cookie1 = await cookieFor(env, "user_1")
    const cookie2 = await cookieFor(env, "user_2")
    await createProvider(env, cookie1)

    const res = await customProviderRoutes.request("/", req("GET", cookie2), env)
    const json = await readJson(res)
    expect(json.providers).toEqual([])
  })
})

describe("PUT /api/custom-providers/order", () => {
  async function createProviderId(env: Env, cookie: string, slug: string): Promise<string> {
    const res = await createProvider(env, cookie, { slug, name: slug })
    return (await readJson(res)).id as string
  }

  async function listSlugs(env: Env, cookie: string): Promise<string[]> {
    const res = await customProviderRoutes.request("/", req("GET", cookie), env)
    const json = await readJson(res)
    return json.providers.map((provider: { slug: string }) => provider.slug)
  }

  it("requires auth", async () => {
    const db = new FakeD1()
    const res = await customProviderRoutes.request("/order", req("PUT", "", { ids: [] }), buildEnv(db))
    expect(res.status).toBe(401)
  })

  it("persists the requested order and returns the server list shape", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const first = await createProviderId(env, cookie, "first-endpoint")
    const second = await createProviderId(env, cookie, "second-endpoint")

    const res = await customProviderRoutes.request("/order", req("PUT", cookie, { ids: [second, first] }), env)
    expect(res.status).toBe(200)
    const json = await readJson(res)
    expect(json.providers.map((provider: { id: string }) => provider.id)).toEqual([second, first])
    expect(json.providers.map((provider: { sort_order: number }) => provider.sort_order)).toEqual([0, 1])
    expect(await listSlugs(env, cookie)).toEqual(["second-endpoint", "first-endpoint"])
  })

  it.each([
    ["missing id", (ids: string[]) => ids.slice(1)],
    ["extra id", (ids: string[]) => [...ids, "cprov_foreign"]],
    ["duplicate id", (ids: string[]) => [ids[0]!, ids[0]!]],
  ])("rejects %s without changing stored order", async (_label, mutate) => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const first = await createProviderId(env, cookie, "first-endpoint")
    const second = await createProviderId(env, cookie, "second-endpoint")
    const before = await listSlugs(env, cookie)

    const res = await customProviderRoutes.request(
      "/order",
      req("PUT", cookie, { ids: mutate([first, second]) }),
      env,
    )
    expect(res.status).toBe(400)
    expect(await listSlugs(env, cookie)).toEqual(before)
  })

  it("rejects another user's id without changing stored order", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    seedUser(db, "user_2")
    const env = buildEnv(db)
    const cookie1 = await cookieFor(env, "user_1")
    const cookie2 = await cookieFor(env, "user_2")
    const first = await createProviderId(env, cookie1, "first-endpoint")
    const second = await createProviderId(env, cookie2, "second-endpoint")
    const before = await listSlugs(env, cookie1)

    const res = await customProviderRoutes.request(
      "/order",
      req("PUT", cookie1, { ids: [first, second] }),
      env,
    )
    expect(res.status).toBe(400)
    expect(await listSlugs(env, cookie1)).toEqual(before)
  })

  it("rejects a non-array body without changing stored order", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    await createProviderId(env, cookie, "first-endpoint")
    await createProviderId(env, cookie, "second-endpoint")
    const before = await listSlugs(env, cookie)

    const res = await customProviderRoutes.request("/order", req("PUT", cookie, { ids: "not-an-array" }), env)
    expect(res.status).toBe(400)
    expect(await listSlugs(env, cookie)).toEqual(before)
  })
})

describe("custom provider legacy sort order", () => {
  it("falls back to created_at order when every sort_order is zero", async () => {
    const db = new FakeD1()
    seedUser(db)
    db.seed("custom_providers", [
      {
        id: "cprov_later",
        user_id: "user_1",
        slug: "later-endpoint",
        name: "Later",
        format: "openai",
        base_url: "https://later.example.com/v1",
        models_mode: "auto",
        manual_models_json: null,
        sort_order: 0,
        created_at: "2026-01-02T00:00:00.000Z",
        updated_at: "2026-01-02T00:00:00.000Z",
      },
      {
        id: "cprov_earlier",
        user_id: "user_1",
        slug: "earlier-endpoint",
        name: "Earlier",
        format: "openai",
        base_url: "https://earlier.example.com/v1",
        models_mode: "auto",
        manual_models_json: null,
        sort_order: 0,
        created_at: "2026-01-01T00:00:00.000Z",
        updated_at: "2026-01-01T00:00:00.000Z",
      },
    ])
    db.seed("upstream_accounts", [])
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")

    const res = await customProviderRoutes.request("/", req("GET", cookie), env)
    const json = await readJson(res)
    expect(json.providers.map((provider: { slug: string }) => provider.slug)).toEqual([
      "earlier-endpoint",
      "later-endpoint",
    ])
  })
})


describe("PUT /api/custom-providers/:id (update)", () => {
  async function createAndGetId(env: Env, cookie: string): Promise<string> {
    const res = await createProvider(env, cookie)
    const json = await readJson(res)
    return json.id as string
  }

  it("404s for an unknown id", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await customProviderRoutes.request(
      "/nonexistent",
      req("PUT", cookie, { name: "x" }),
      env,
    )
    expect(res.status).toBe(404)
  })

  it("404s when the id belongs to another user", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    seedUser(db, "user_2")
    const env = buildEnv(db)
    const cookie1 = await cookieFor(env, "user_1")
    const cookie2 = await cookieFor(env, "user_2")
    const id = await createAndGetId(env, cookie1)
    const res = await customProviderRoutes.request("/" + id, req("PUT", cookie2, { name: "x" }), env)
    expect(res.status).toBe(404)
  })

  it("updates name and base_url without an api_key, leaving the stored key untouched", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const id = await createAndGetId(env, cookie)

    const res = await customProviderRoutes.request(
      "/" + id,
      req("PUT", cookie, { name: "Renamed", base_url: "https://new-upstream.example.com/v1" }),
      env,
    )
    expect(res.status).toBe(200)
    const json = await readJson(res)
    expect(json.name).toBe("Renamed")
    expect(json.base_url).toBe("https://new-upstream.example.com/v1")
    expect(json.key_mask).toBe("sk-ups…alue")
    // Same account row throughout — updates replace the key in place.
    expect(json.account_id).toBe(db.rows("upstream_accounts")[0]!.id)

    const cred = await decryptJson<StoredCredential>(
      TOKEN_KEY,
      db.rows("upstream_accounts")[0]!.encrypted_payload as string,
    )
    expect(cred.access_token).toBe(validCreateBody.api_key)
  })

  it("a blank api_key keeps the stored key (no-op)", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const id = await createAndGetId(env, cookie)

    const res = await customProviderRoutes.request(
      "/" + id,
      req("PUT", cookie, { api_key: "" }),
      env,
    )
    expect(res.status).toBe(200)
    const cred = await decryptJson<StoredCredential>(
      TOKEN_KEY,
      db.rows("upstream_accounts")[0]!.encrypted_payload as string,
    )
    expect(cred.access_token).toBe(validCreateBody.api_key)
  })

  it("replaces the stored key and mask when api_key is provided", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const id = await createAndGetId(env, cookie)

    const res = await customProviderRoutes.request(
      "/" + id,
      req("PUT", cookie, { api_key: "sk-brand-new-rotated-key-9999" }),
      env,
    )
    const json = await readJson(res)
    expect(json.key_mask).not.toBe("sk-ups…alue")
    const cred = await decryptJson<StoredCredential>(
      TOKEN_KEY,
      db.rows("upstream_accounts")[0]!.encrypted_payload as string,
    )
    expect(cred.access_token).toBe("sk-brand-new-rotated-key-9999")
    // Still exactly one account row — replaced in place, not duplicated.
    expect(db.rows("upstream_accounts")).toHaveLength(1)
  })

  it("rejects changing the slug (immutable)", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const id = await createAndGetId(env, cookie)
    const res = await customProviderRoutes.request(
      "/" + id,
      req("PUT", cookie, { slug: "different-slug" }),
      env,
    )
    expect(res.status).toBe(400)
  })

  it("rejects changing the format (immutable)", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const id = await createAndGetId(env, cookie)
    const res = await customProviderRoutes.request(
      "/" + id,
      req("PUT", cookie, { format: "anthropic" }),
      env,
    )
    expect(res.status).toBe(400)
  })

  it("allows re-sending the same slug/format as a no-op", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const id = await createAndGetId(env, cookie)
    const res = await customProviderRoutes.request(
      "/" + id,
      req("PUT", cookie, { slug: "my-endpoint", format: "openai", name: "Still fine" }),
      env,
    )
    expect(res.status).toBe(200)
  })

  it("rejects an invalid base_url on update", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const id = await createAndGetId(env, cookie)
    const res = await customProviderRoutes.request(
      "/" + id,
      req("PUT", cookie, { base_url: "https://127.0.0.1/v1" }),
      env,
    )
    expect(res.status).toBe(400)
  })
})

describe("DELETE /api/custom-providers/:id", () => {
  it("404s for an unknown id", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await customProviderRoutes.request("/nonexistent", req("DELETE", cookie), env)
    expect(res.status).toBe(404)
  })

  it("deletes the provider row and cascades its upstream_accounts rows", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const createRes = await createProvider(env, cookie)
    const { id } = await readJson(createRes)
    expect(db.rows("custom_providers")).toHaveLength(1)
    expect(db.rows("upstream_accounts")).toHaveLength(1)

    const res = await customProviderRoutes.request("/" + id, req("DELETE", cookie), env)
    expect(res.status).toBe(200)
    expect(db.rows("custom_providers")).toHaveLength(0)
    expect(db.rows("upstream_accounts")).toHaveLength(0)
  })

  it("does not delete another user's provider or accounts", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    seedUser(db, "user_2")
    const env = buildEnv(db)
    const cookie1 = await cookieFor(env, "user_1")
    const cookie2 = await cookieFor(env, "user_2")
    const createRes = await createProvider(env, cookie1)
    const { id } = await readJson(createRes)

    const res = await customProviderRoutes.request("/" + id, req("DELETE", cookie2), env)
    expect(res.status).toBe(404)
    expect(db.rows("custom_providers")).toHaveLength(1)
    expect(db.rows("upstream_accounts")).toHaveLength(1)
  })
})

describe("POST /api/custom-providers/:id/unpause", () => {
  it("requires auth", async () => {
    const db = new FakeD1()
    const res = await customProviderRoutes.request("/cprov_1/unpause", { method: "POST" }, buildEnv(db))
    expect(res.status).toBe(401)
  })

  it("404s for an unknown id", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await customProviderRoutes.request("/nonexistent/unpause", req("POST", cookie), env)
    expect(res.status).toBe(404)
    expect(await readJson(res)).toEqual({ error: "not found" })
  })

  it("404s when the id belongs to another user", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    seedUser(db, "user_2")
    const env = buildEnv(db)
    const cookie1 = await cookieFor(env, "user_1")
    const cookie2 = await cookieFor(env, "user_2")
    const createRes = await createProvider(env, cookie1)
    const { id } = await readJson(createRes)
    const accountId = db.rows("upstream_accounts")[0]!.id as string
    await markBenched(env, "user_1", "my-endpoint", accountId)

    const res = await customProviderRoutes.request("/" + id + "/unpause", req("POST", cookie2), env)
    expect(res.status).toBe(404)
    expect(await readJson(res)).toEqual({ error: "not found" })
    expect(await isBenched(env, "user_1", "my-endpoint", accountId)).toBe(true)
  })

  it("200 clears the bench and GET / reports status active", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const createRes = await createProvider(env, cookie)
    const { id } = await readJson(createRes)
    const accountId = db.rows("upstream_accounts")[0]!.id as string
    const storedKey = db.rows("upstream_accounts")[0]!.encrypted_payload
    await markBenched(env, "user_1", "my-endpoint", accountId)

    const benched = await customProviderRoutes.request("/", req("GET", cookie), env)
    expect((await readJson(benched)).providers[0].status).toBe("benched")

    const res = await customProviderRoutes.request("/" + id + "/unpause", req("POST", cookie), env)
    expect(res.status).toBe(200)
    expect(await readJson(res)).toEqual({ ok: true })
    expect(await isBenched(env, "user_1", "my-endpoint", accountId)).toBe(false)
    expect(db.rows("upstream_accounts")[0]!.encrypted_payload).toBe(storedKey)

    const listed = await customProviderRoutes.request("/", req("GET", cookie), env)
    expect((await readJson(listed)).providers[0].status).toBe("active")
  })

  it("200 when the provider is not currently benched", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const createRes = await createProvider(env, cookie)
    const { id } = await readJson(createRes)

    const res = await customProviderRoutes.request("/" + id + "/unpause", req("POST", cookie), env)
    expect(res.status).toBe(200)
    expect(await readJson(res)).toEqual({ ok: true })

    const listed = await customProviderRoutes.request("/", req("GET", cookie), env)
    expect((await readJson(listed)).providers[0].status).toBe("active")
  })
})

describe("POST /api/custom-providers/test", () => {
  it("maps a 200 upstream response to ok:true with a model sample", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    globalThis.fetch = (async () =>
      new Response(JSON.stringify({ data: [{ id: "m1" }, { id: "m2" }] }), { status: 200 })) as typeof fetch

    const res = await customProviderRoutes.request(
      "/test",
      req("POST", cookie, {
        format: "openai",
        base_url: "https://upstream.example.com/v1",
        api_key: "sk-test",
      }),
      env,
    )
    const json = await readJson(res)
    expect(json).toMatchObject({ ok: true, models_count: 2, sample: ["m1", "m2"] })
  })

  it("maps a 404 upstream response to ok:true with a null count and a note", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    globalThis.fetch = (async () => new Response("not found", { status: 404 })) as typeof fetch

    const res = await customProviderRoutes.request(
      "/test",
      req("POST", cookie, {
        format: "openai",
        base_url: "https://upstream.example.com/v1",
        api_key: "sk-test",
      }),
      env,
    )
    const json = await readJson(res)
    expect(json.ok).toBe(true)
    expect(json.models_count).toBeNull()
    expect(json.note).toContain("no models endpoint")
  })

  it("maps 401/403 to ok:false auth rejected", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    globalThis.fetch = (async () => new Response("nope", { status: 401 })) as typeof fetch

    const res = await customProviderRoutes.request(
      "/test",
      req("POST", cookie, {
        format: "openai",
        base_url: "https://upstream.example.com/v1",
        api_key: "sk-test",
      }),
      env,
    )
    const json = await readJson(res)
    expect(json).toEqual({ ok: false, error: "auth rejected (401)" })
  })

  it("maps a network failure to ok:false unreachable/timeout", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    globalThis.fetch = (async () => {
      throw new Error("simulated network failure")
    }) as typeof fetch

    const res = await customProviderRoutes.request(
      "/test",
      req("POST", cookie, {
        format: "openai",
        base_url: "https://upstream.example.com/v1",
        api_key: "sk-test",
      }),
      env,
    )
    const json = await readJson(res)
    expect(json).toEqual({ ok: false, error: "unreachable/timeout" })
  })

  it("maps other upstream status codes to ok:false HTTP <status>", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    globalThis.fetch = (async () => new Response("boom", { status: 500 })) as typeof fetch

    const res = await customProviderRoutes.request(
      "/test",
      req("POST", cookie, {
        format: "openai",
        base_url: "https://upstream.example.com/v1",
        api_key: "sk-test",
      }),
      env,
    )
    const json = await readJson(res)
    expect(json).toEqual({ ok: false, error: "HTTP 500" })
  })

  it("runs the URL guard before probing, for the pre-save shape", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    globalThis.fetch = (async () => {
      throw new Error("must not be called — URL guard should reject first")
    }) as typeof fetch

    const res = await customProviderRoutes.request(
      "/test",
      req("POST", cookie, { format: "openai", base_url: "http://insecure.example.com", api_key: "sk-test" }),
      env,
    )
    expect(res.status).toBe(400)
  })

  it("uses the stored key for a saved provider referenced by id", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const createRes = await createProvider(env, cookie)
    const { id } = await readJson(createRes)

    let capturedAuth: string | undefined
    globalThis.fetch = (async (_url: string, init?: RequestInit) => {
      capturedAuth = (init?.headers as Record<string, string>).authorization
      return new Response(JSON.stringify({ data: [] }), { status: 200 })
    }) as typeof fetch

    const res = await customProviderRoutes.request("/test", req("POST", cookie, { id }), env)
    expect(res.status).toBe(200)
    expect(capturedAuth).toBe(`Bearer ${validCreateBody.api_key}`)
  })
})
