import { afterEach, describe, expect, it } from "vitest"
import {
  ANTIGRAVITY_OAUTH,
  antigravityOAuthClient,
  beginAntigravityAuthorization,
  exchangeAntigravityCode,
  parseAntigravityCallback,
} from "../src/auth/provider_oauth"
import { createSession } from "../src/auth/session"
import { decryptJson } from "../src/crypto/token_crypto"
import type { Env } from "../src/env"
import type { StoredCredential } from "../src/pool/acquire"
import { providerRoutes } from "../src/routes/providers"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

const SESSION_SECRET = "test-session-secret-not-real"
const TOKEN_KEY = "test-token-encryption-key-not-secret"
const NOW = "2026-01-01T00:00:00.000Z"

/** The client pair is never committed, so every test that reaches OAuth supplies one. */
const CLIENT_ID = "test-client.apps.googleusercontent.com"
const CLIENT_SECRET = "test-client-secret"

function buildEnv(db: FakeD1, opts?: { configured?: boolean }): Env {
  return {
    DB: db as unknown as D1Database,
    BENCH: fakeKV(),
    CACHE: fakeKV(),
    APP_URL: "https://app.example.com",
    SESSION_SECRET,
    TOKEN_ENCRYPTION_KEY: TOKEN_KEY,
    ...(opts?.configured === false
      ? {}
      : {
          ANTIGRAVITY_OAUTH_CLIENT_ID: CLIENT_ID,
          ANTIGRAVITY_OAUTH_CLIENT_SECRET: CLIENT_SECRET,
        }),
  } as unknown as Env
}

function seedUser(db: FakeD1, id: string): void {
  db.seed("users", [
    {
      id,
      google_sub: `sub-${id}`,
      email: `${id}@example.com`,
      name: "Test User",
      picture_url: null,
      created_at: NOW,
      updated_at: NOW,
    },
  ])
}

async function cookieFor(env: Env, userId: string): Promise<string> {
  const { cookie } = await createSession(env, userId)
  return cookie.split(";")[0]!
}

function req(method: string, cookie?: string, body?: unknown): RequestInit {
  return {
    method,
    headers: {
      "content-type": "application/json",
      host: "app.example.com",
      ...(cookie ? { cookie } : {}),
    },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  }
}

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

describe("antigravityOAuthClient", () => {
  it("resolves only when both halves are configured — half a pair is none", () => {
    expect(
      antigravityOAuthClient({
        ANTIGRAVITY_OAUTH_CLIENT_ID: "a",
        ANTIGRAVITY_OAUTH_CLIENT_SECRET: "b",
      }),
    ).toEqual({ clientId: "a", clientSecret: "b" })
    expect(antigravityOAuthClient({ ANTIGRAVITY_OAUTH_CLIENT_ID: "a" })).toBeNull()
    expect(antigravityOAuthClient({ ANTIGRAVITY_OAUTH_CLIENT_SECRET: "b" })).toBeNull()
    expect(antigravityOAuthClient({})).toBeNull()
    // Whitespace-only config is not config.
    expect(
      antigravityOAuthClient({
        ANTIGRAVITY_OAUTH_CLIENT_ID: "  ",
        ANTIGRAVITY_OAUTH_CLIENT_SECRET: "b",
      }),
    ).toBeNull()
  })
})

describe("beginAntigravityAuthorization", () => {
  it("builds a consent-forcing offline authorize URL with the registered redirect", () => {
    const { authorizationUrl, pending } = beginAntigravityAuthorization(CLIENT_ID)
    const url = new URL(authorizationUrl)
    expect(url.origin + url.pathname).toBe(ANTIGRAVITY_OAUTH.authorizeUrl)
    expect(url.searchParams.get("response_type")).toBe("code")
    expect(url.searchParams.get("access_type")).toBe("offline")
    expect(url.searchParams.get("prompt")).toBe("consent")
    expect(url.searchParams.get("client_id")).toBe(CLIENT_ID)
    // Google rejects every other redirect for this client (docs/auth.md).
    expect(url.searchParams.get("redirect_uri")).toBe("http://localhost:51121/oauth-callback")
    expect(url.searchParams.get("scope")).toBe(ANTIGRAVITY_OAUTH.scope)
    expect(url.searchParams.get("state")).toBe(pending.oauth_state)
    // Confidential client: no PKCE challenge is sent or stored.
    expect(url.searchParams.get("code_challenge")).toBeNull()
    expect(pending).not.toHaveProperty("code_verifier")
  })

})

describe("parseAntigravityCallback", () => {
  it("takes the code and state out of a pasted callback URL", () => {
    expect(
      parseAntigravityCallback("http://localhost:51121/oauth-callback?code=abc123&state=st-1"),
    ).toEqual({ code: "abc123", state: "st-1" })
  })

  it("accepts a bare code, reporting that it carries no state", () => {
    expect(parseAntigravityCallback("  abc123 ")).toEqual({ code: "abc123", state: null })
  })

  it("surfaces a Google error carried in the callback URL", () => {
    expect(() =>
      parseAntigravityCallback("http://localhost:51121/oauth-callback?error=access_denied"),
    ).toThrow(/access_denied/)
  })

  it("rejects a callback URL with no code", () => {
    expect(() => parseAntigravityCallback("http://localhost:51121/oauth-callback")).toThrow(
      /no code/,
    )
  })

  it("rejects an empty paste", () => {
    expect(() => parseAntigravityCallback("   ")).toThrow()
  })
})

describe("exchangeAntigravityCode", () => {
  const pending = {
    client_id: "client-1",
    oauth_state: "state-1",
    redirect_uri: ANTIGRAVITY_OAUTH.redirectUri,
  }

  it("posts a form-encoded exchange carrying the client secret", async () => {
    let seen: { url: string; body: string } | null = null
    globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      seen = { url: String(input), body: String(init?.body) }
      return new Response(
        JSON.stringify({ access_token: "at", refresh_token: "rt", expires_in: 3600 }),
        { status: 200 },
      )
    }) as typeof fetch

    const tok = await exchangeAntigravityCode({
      code: "code-1",
      returnedState: "state-1",
      pending,
      clientSecret: "secret-1",
    })

    expect(tok).toEqual({ access_token: "at", refresh_token: "rt", expires_in: 3600 })
    expect(seen!.url).toBe(ANTIGRAVITY_OAUTH.tokenUrl)
    const form = new URLSearchParams(seen!.body)
    expect(Object.fromEntries(form)).toEqual({
      grant_type: "authorization_code",
      code: "code-1",
      client_id: "client-1",
      client_secret: "secret-1",
      redirect_uri: ANTIGRAVITY_OAUTH.redirectUri,
    })
  })

  it("refuses a state that does not match the one it issued", async () => {
    globalThis.fetch = (async () => new Response("{}", { status: 200 })) as typeof fetch
    await expect(
      exchangeAntigravityCode({
        code: "c",
        returnedState: "not-state-1",
        pending,
        clientSecret: "s",
      }),
    ).rejects.toThrow(/state mismatch/i)
  })

  it("proceeds when the user pasted a bare code, which carries no state", async () => {
    globalThis.fetch = (async () =>
      new Response(JSON.stringify({ access_token: "at" }), { status: 200 })) as typeof fetch
    await expect(
      exchangeAntigravityCode({ code: "c", returnedState: null, pending, clientSecret: "s" }),
    ).resolves.toMatchObject({ access_token: "at" })
  })
})

describe("POST /antigravity/login", () => {
  it("stores the pending state and returns the authorize URL", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")

    const res = await providerRoutes.request("/antigravity/login", req("POST", cookie), env)

    expect(res.status).toBe(200)
    const json = (await res.json()) as Record<string, string>
    expect(json.login_id).toEqual(expect.any(String))
    const url = new URL(json.authorization_url)
    expect(url.searchParams.get("client_id")).toBe(CLIENT_ID)

    const state = db.rows("oauth_login_states")[0]!
    expect(state.provider).toBe("antigravity")
    const payload = JSON.parse(state.payload_json as string) as Record<string, string>
    expect(payload.client_id).toBe(CLIENT_ID)
    expect(payload.oauth_state).toBe(url.searchParams.get("state"))
  })

  it("refuses plainly when the deploy has no client configured", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    const env = buildEnv(db, { configured: false })
    const cookie = await cookieFor(env, "user_1")

    const res = await providerRoutes.request("/antigravity/login", req("POST", cookie), env)

    expect(res.status).toBe(400)
    expect((await res.json()) as Record<string, string>).toMatchObject({
      error: expect.stringContaining("ANTIGRAVITY_OAUTH_CLIENT_ID"),
    })
    // No half-started login row is left behind.
    expect(db.rows("oauth_login_states")).toHaveLength(0)
  })

  it("requires a session", async () => {
    const db = new FakeD1()
    const res = await providerRoutes.request("/antigravity/login", req("POST"), buildEnv(db))
    expect(res.status).toBe(401)
  })
})

describe("POST /antigravity/login/:id/complete", () => {
  async function startLogin(env: Env, cookie: string): Promise<{ loginId: string; state: string }> {
    const res = await providerRoutes.request("/antigravity/login", req("POST", cookie), env)
    const json = (await res.json()) as Record<string, string>
    return {
      loginId: json.login_id,
      state: new URL(json.authorization_url).searchParams.get("state")!,
    }
  }

  it("exchanges the code, resolves the project, and stores an encrypted credential", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    globalThis.fetch = (async () => new Response("{}", { status: 200 })) as typeof fetch
    const { loginId, state } = await startLogin(env, cookie)

    const urls: string[] = []
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      const url = String(input)
      urls.push(url)
      if (url === ANTIGRAVITY_OAUTH.tokenUrl) {
        return new Response(
          JSON.stringify({ access_token: "at-1", refresh_token: "rt-1", expires_in: 3600 }),
          { status: 200 },
        )
      }
      if (url.includes("userinfo")) {
        return new Response(JSON.stringify({ email: "a@b.com", name: "A B" }), { status: 200 })
      }
      if (url.includes("loadCodeAssist")) {
        return new Response(
          JSON.stringify({
            cloudaicompanionProject: "proj-42",
            allowedTiers: [{ id: "pro-tier", isDefault: true }],
          }),
          { status: 200 },
        )
      }
      throw new Error(`unexpected fetch: ${url}`)
    }) as typeof fetch

    const res = await providerRoutes.request(
      `/antigravity/login/${loginId}/complete`,
      req("POST", cookie, {
        code: `http://localhost:51121/oauth-callback?code=code-1&state=${state}`,
      }),
      env,
    )

    expect(res.status).toBe(200)
    expect(await res.json()).toMatchObject({ ok: true, label: "a@b.com" })

    const account = db.rows("upstream_accounts")[0]!
    expect(account.provider).toBe("antigravity")
    const credential = await decryptJson<StoredCredential>(
      TOKEN_KEY,
      account.encrypted_payload as string,
    )
    expect(credential).toMatchObject({
      access_token: "at-1",
      refresh_token: "rt-1",
      email: "a@b.com",
      token_endpoint: ANTIGRAVITY_OAUTH.tokenUrl,
      // Project id rides inside the encrypted payload — no new column.
      extra: { project_id: "proj-42", tier_id: "pro-tier" },
    })
    // The pending state is consumed, not left behind for replay.
    expect(db.rows("oauth_login_states")).toHaveLength(0)
  })

  it("still binds the account when the project bootstrap fails", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    globalThis.fetch = (async () => new Response("{}", { status: 200 })) as typeof fetch
    const { loginId, state } = await startLogin(env, cookie)

    globalThis.fetch = (async (input: RequestInfo | URL) => {
      const url = String(input)
      if (url === ANTIGRAVITY_OAUTH.tokenUrl) {
        return new Response(JSON.stringify({ access_token: "at-1" }), { status: 200 })
      }
      if (url.includes("userinfo")) {
        return new Response(JSON.stringify({ email: "a@b.com" }), { status: 200 })
      }
      return new Response("nope", { status: 500 })
    }) as typeof fetch

    const res = await providerRoutes.request(
      `/antigravity/login/${loginId}/complete`,
      req("POST", cookie, { code: `x?code=1&state=${state}` }),
      env,
    )

    expect(res.status).toBe(200)
    const credential = await decryptJson<StoredCredential>(
      TOKEN_KEY,
      db.rows("upstream_accounts")[0]!.encrypted_payload as string,
    )
    expect(credential.access_token).toBe("at-1")
    expect(credential.extra).toBeUndefined()
  })

  it("rejects a callback whose state does not match the stored login", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    globalThis.fetch = (async () => new Response("{}", { status: 200 })) as typeof fetch
    const { loginId } = await startLogin(env, cookie)

    let exchanged = false
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      if (String(input) === ANTIGRAVITY_OAUTH.tokenUrl) exchanged = true
      return new Response("{}", { status: 200 })
    }) as typeof fetch

    const res = await providerRoutes.request(
      `/antigravity/login/${loginId}/complete`,
      req("POST", cookie, {
        code: "http://localhost:51121/oauth-callback?code=code-1&state=forged",
      }),
      env,
    )

    expect(res.status).toBe(400)
    // The CSRF check must happen before any token is requested.
    expect(exchanged).toBe(false)
    expect(db.rows("upstream_accounts")).toHaveLength(0)
  })
})
