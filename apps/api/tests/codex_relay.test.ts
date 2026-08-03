import { afterEach, describe, expect, it } from "vitest"
import type { Env } from "../src/env"
import { codexAdapter } from "../src/providers/codex"
import {
  codexUpstream,
  mintIdToken,
  relayFetch,
  resetCodexRelayCacheForTests,
} from "../src/providers/codex_relay"
import type { AcquiredAccount } from "../src/pool/acquire"

const TOKEN_EXCHANGE_URL = "https://oauth2.googleapis.com/token"
const RELAY_URL = "https://kano-codex-relay-abc123-uc.a.run.app"

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
  resetCodexRelayCacheForTests()
})

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  })
}

function base64Url(bytes: Uint8Array): string {
  let s = ""
  for (const b of bytes) s += String.fromCharCode(b)
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "")
}

function base64UrlJson(value: unknown): string {
  return base64Url(new TextEncoder().encode(JSON.stringify(value)))
}

function decodeBase64UrlJson<T>(segment: string): T {
  return JSON.parse(atob(segment.replace(/-/g, "+").replace(/_/g, "/"))) as T
}

/** A structurally valid but unsigned/garbage-signature JWT — mintIdToken only reads the payload's `exp`. */
function buildFakeIdToken(payload: Record<string, unknown>): string {
  return `${base64UrlJson({ alg: "RS256", typ: "JWT" })}.${base64UrlJson(payload)}.fake-signature`
}

function freshExp(): number {
  return Math.floor(Date.now() / 1000) + 3600
}

/** Real RSA keypair, exported to the PKCS8 PEM shape a GCP SA JSON key ships. */
async function fakeServiceAccountKey(
  clientEmail: string,
): Promise<{ client_email: string; private_key: string }> {
  // workers-types collapses generateKey/exportKey to their loosest overload
  // (CryptoKey | CryptoKeyPair, ArrayBuffer | JsonWebKey) regardless of the
  // algorithm/format passed in, so the RSA + "pkcs8" shapes need an explicit
  // assertion here — this is a test-only concern, not a runtime one.
  const keyPair = (await crypto.subtle.generateKey(
    {
      name: "RSASSA-PKCS1-v1_5",
      modulusLength: 2048,
      publicExponent: new Uint8Array([1, 0, 1]),
      hash: "SHA-256",
    },
    true,
    ["sign", "verify"],
  )) as CryptoKeyPair
  const pkcs8 = new Uint8Array(
    (await crypto.subtle.exportKey("pkcs8", keyPair.privateKey)) as ArrayBuffer,
  )
  let bin = ""
  for (const b of pkcs8) bin += String.fromCharCode(b)
  const base64 = btoa(bin)
  const lines = base64.match(/.{1,64}/g) ?? [base64]
  const pem = `-----BEGIN PRIVATE KEY-----\n${lines.join("\n")}\n-----END PRIVATE KEY-----\n`
  return { client_email: clientEmail, private_key: pem }
}

function relayEnv(sa: { client_email: string; private_key: string }): Env {
  return {
    CODEX_RELAY_URL: RELAY_URL,
    CODEX_RELAY_SA_KEY: JSON.stringify(sa),
  } as Env
}

describe("mintIdToken", () => {
  it("signs a jwt-bearer assertion, exchanges it, and relayFetch attaches the id token as x-serverless-authorization while keeping the upstream authorization header", async () => {
    const sa = await fakeServiceAccountKey("kano-relay-invoker@test-project.iam.gserviceaccount.com")
    const env = relayEnv(sa)

    let tokenExchangeCalls = 0
    let capturedGrantType = ""
    let capturedAssertion = ""
    const fakeIdToken = buildFakeIdToken({ exp: freshExp() })
    let capturedRelayRequest: { url: string; headers: Headers } | undefined

    globalThis.fetch = (async (url: string, init?: RequestInit) => {
      if (url === TOKEN_EXCHANGE_URL) {
        tokenExchangeCalls++
        const params = new URLSearchParams(String(init?.body))
        capturedGrantType = params.get("grant_type") ?? ""
        capturedAssertion = params.get("assertion") ?? ""
        return jsonResponse({ id_token: fakeIdToken })
      }
      capturedRelayRequest = { url, headers: new Headers(init?.headers) }
      return new Response("", { status: 200, headers: { "x-relay-upstream": "1" } })
    }) as typeof fetch

    const upstream = codexUpstream(env)
    expect(upstream.relay).toBe(true)
    expect(upstream.base).toBe(`${RELAY_URL}/backend-api`)

    await relayFetch(upstream, `${upstream.base}/codex/responses`, {
      method: "POST",
      headers: { authorization: "Bearer upstream-oauth-token", "content-type": "application/json" },
      body: "{}",
    })

    expect(tokenExchangeCalls).toBe(1)
    expect(capturedGrantType).toBe("urn:ietf:params:oauth:grant-type:jwt-bearer")

    const [headerSeg, claimsSeg] = capturedAssertion.split(".")
    const header = decodeBase64UrlJson<{ alg: string; typ: string }>(headerSeg!)
    const claims = decodeBase64UrlJson<{
      iss: string
      sub: string
      aud: string
      target_audience: string
      iat: number
      exp: number
    }>(claimsSeg!)
    expect(header).toEqual({ alg: "RS256", typ: "JWT" })
    expect(claims.iss).toBe(sa.client_email)
    expect(claims.sub).toBe(sa.client_email)
    expect(claims.aud).toBe(TOKEN_EXCHANGE_URL)
    expect(claims.target_audience).toBe(RELAY_URL)
    expect(claims.exp - claims.iat).toBe(3600)

    expect(capturedRelayRequest?.headers.get("x-serverless-authorization")).toBe(`Bearer ${fakeIdToken}`)
    expect(capturedRelayRequest?.headers.get("authorization")).toBe("Bearer upstream-oauth-token")
  })

  it("throws when the relay is not configured", async () => {
    await expect(mintIdToken({} as Env)).rejects.toThrow()
  })
})

describe("id token cache", () => {
  it("reuses a cached id token across relayFetch calls without re-hitting the token endpoint", async () => {
    const sa = await fakeServiceAccountKey("relay-invoker@test-project.iam.gserviceaccount.com")
    const env = relayEnv(sa)
    let tokenExchangeCalls = 0
    globalThis.fetch = (async (url: string) => {
      if (url === TOKEN_EXCHANGE_URL) {
        tokenExchangeCalls++
        return jsonResponse({ id_token: buildFakeIdToken({ exp: freshExp() }) })
      }
      return new Response("", { status: 200, headers: { "x-relay-upstream": "1" } })
    }) as typeof fetch

    const upstream = codexUpstream(env)
    await relayFetch(upstream, `${upstream.base}/codex/responses`, { method: "POST", body: "{}" })
    await relayFetch(upstream, `${upstream.base}/codex/models`, { method: "GET" })

    expect(tokenExchangeCalls).toBe(1)
  })
})

describe("relayFetch tri-state guard", () => {
  async function relayEnvWithFreshKey(): Promise<Env> {
    return relayEnv(await fakeServiceAccountKey("relay-invoker@test-project.iam.gserviceaccount.com"))
  }

  function stubMintThen(relayHandler: (url: string) => Response): void {
    globalThis.fetch = (async (url: string) => {
      if (url === TOKEN_EXCHANGE_URL) {
        return jsonResponse({ id_token: buildFakeIdToken({ exp: freshExp() }) })
      }
      return relayHandler(url)
    }) as typeof fetch
  }

  it("an upstream-marked response passes through untouched, including a 401 — bench semantics stay intact", async () => {
    const env = await relayEnvWithFreshKey()
    stubMintThen(() => new Response("nope", { status: 401, headers: { "x-relay-upstream": "1" } }))

    const upstream = codexUpstream(env)
    const res = await relayFetch(upstream, `${upstream.base}/codex/responses`, {
      method: "POST",
      body: "{}",
    })
    expect(res.status).toBe(401)
    expect(res.headers.get("x-relay-upstream")).toBe("1")
  })

  it("a fault-marked response converts to 502 relay_fault, never propagating the relay's own status", async () => {
    const env = await relayEnvWithFreshKey()
    stubMintThen(() => new Response("bad path", { status: 502, headers: { "x-relay-fault": "path" } }))

    const upstream = codexUpstream(env)
    const res = await relayFetch(upstream, `${upstream.base}/codex/responses`, {
      method: "POST",
      body: "{}",
    })
    expect(res.status).toBe(502)
    const json = await res.json()
    expect(json).toEqual({ error: { type: "relay_fault", reason: "path" } })
  })

  it("a marker-less 401 re-mints and retries exactly once; a still marker-less retry ends in 502 relay_unavailable", async () => {
    const env = await relayEnvWithFreshKey()
    let tokenExchangeCalls = 0
    let relayCalls = 0
    globalThis.fetch = (async (url: string) => {
      if (url === TOKEN_EXCHANGE_URL) {
        tokenExchangeCalls++
        return jsonResponse({ id_token: buildFakeIdToken({ exp: freshExp() }) })
      }
      relayCalls++
      // Neither marker on either attempt — simulates a Cloud Run IAM rejection
      // (the request never reached the relay app at all).
      return new Response("unauthorized", { status: 401 })
    }) as typeof fetch

    const upstream = codexUpstream(env)
    const res = await relayFetch(upstream, `${upstream.base}/codex/responses`, {
      method: "POST",
      body: "{}",
    })

    expect(tokenExchangeCalls).toBe(2) // initial mint + one re-mint after invalidation
    expect(relayCalls).toBe(2) // initial attempt + exactly one retry
    expect(res.status).toBe(502)
    const json = (await res.json()) as { error: { type: string; status: number } }
    expect(json.error.type).toBe("relay_unavailable")
    expect(json.error.status).toBe(401)
  })

  it("a marker-less 429 goes straight to 502 relay_unavailable without retry-minting", async () => {
    const env = await relayEnvWithFreshKey()
    let tokenExchangeCalls = 0
    let relayCalls = 0
    globalThis.fetch = (async (url: string) => {
      if (url === TOKEN_EXCHANGE_URL) {
        tokenExchangeCalls++
        return jsonResponse({ id_token: buildFakeIdToken({ exp: freshExp() }) })
      }
      relayCalls++
      return new Response("rate limited", { status: 429 })
    }) as typeof fetch

    const upstream = codexUpstream(env)
    const res = await relayFetch(upstream, `${upstream.base}/codex/responses`, {
      method: "POST",
      body: "{}",
    })

    expect(tokenExchangeCalls).toBe(1) // no retry means no second mint
    expect(relayCalls).toBe(1)
    expect(res.status).toBe(502)
    const json = await res.json()
    expect(json).toEqual({ error: { type: "relay_unavailable", status: 429 } })
  })
})

describe("codexAdapter wiring", () => {
  const codexAccount: AcquiredAccount = {
    row: {
      id: "acc_1",
      user_id: "user_1",
      provider: "codex",
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
  const baseReq = {
    model: "codex/gpt-5.2",
    rawModel: "codex/gpt-5.2",
    upstreamModel: "gpt-5.2",
    messages: [{ role: "user", content: "hi" }],
    rawBody: {},
  }

  it("routes chatCompletions through the relay base with x-serverless-authorization when both env vars are set", async () => {
    const sa = await fakeServiceAccountKey("relay-invoker@test-project.iam.gserviceaccount.com")
    const env = relayEnv(sa)
    let captured: { url: string; headers: Headers } | undefined
    globalThis.fetch = (async (url: string, init?: RequestInit) => {
      if (url === TOKEN_EXCHANGE_URL) {
        return jsonResponse({ id_token: buildFakeIdToken({ exp: freshExp() }) })
      }
      captured = { url, headers: new Headers(init?.headers) }
      return new Response("", { status: 200, headers: { "x-relay-upstream": "1" } })
    }) as typeof fetch

    await codexAdapter.chatCompletions(env, codexAccount, baseReq)

    expect(captured?.url).toBe(`${RELAY_URL}/backend-api/codex/responses`)
    expect(captured?.headers.get("x-serverless-authorization")).toMatch(/^Bearer .+/)
    expect(captured?.headers.get("authorization")).toBe("Bearer tok_test")
  })

  it("goes direct to chatgpt.com with no serverless header when the relay is unconfigured", async () => {
    let captured: { url: string; headers: Headers } | undefined
    globalThis.fetch = (async (url: string, init?: RequestInit) => {
      captured = { url, headers: new Headers(init?.headers) }
      return new Response("", { status: 200 })
    }) as typeof fetch

    await codexAdapter.chatCompletions({} as Env, codexAccount, baseReq)

    expect(captured?.url).toBe("https://chatgpt.com/backend-api/codex/responses")
    expect(captured?.headers.has("x-serverless-authorization")).toBe(false)
  })
})
