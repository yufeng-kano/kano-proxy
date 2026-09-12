/**
 * Pool extension (docs/cloud-edition.md § "Pool extension") — the optional,
 * composition-time cross-user path. Standalone behavior with no extension is
 * covered by every other suite in this directory; what is asserted here is
 * exactly what an extension adds: shared candidates and their ordering, the
 * per-attempt lease and how it settles, the borrower's route surface, the
 * catalog's bound-provider set, and that two applications never see each
 * other's extension.
 */
import { afterEach, describe, expect, it, vi } from "vitest"
import { createApplication } from "../src/core"
import { createSession } from "../src/auth/session"
import { listModelsForUser } from "../src/catalog/models"
import { encryptJson } from "../src/crypto/token_crypto"
import type { AccountRow } from "../src/db/accounts"
import type { Env } from "../src/env"
import type { AttemptLease, PoolExtension, SharedAccount } from "../src/pool/extension"
import { dispatchChatCompletions } from "../src/proxy/dispatch"
import type { ProviderAdapter } from "../src/providers/types"
import { poolCandidates } from "../src/routing/candidates"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

const TOKEN_KEY = "test-token-encryption-key-not-secret"
const SESSION_SECRET = "test-session-secret-not-real"
const APP_URL = "https://app.example.com"
const NOW = "2026-01-01T00:00:00.000Z"

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

async function accountRow(opts: {
  id: string
  userId: string
  provider?: string
  priority?: number
  createdAt?: string
  label?: string | null
  benchUntil?: string | null
}): Promise<AccountRow> {
  return {
    id: opts.id,
    user_id: opts.userId,
    provider: opts.provider ?? "grok",
    external_account_id: null,
    label: opts.label ?? opts.id,
    custom_label: null,
    priority: opts.priority ?? 1,
    encrypted_payload: await encryptJson(TOKEN_KEY, {
      access_token: `token-${opts.id}`,
      expires_at: "2099-01-01T00:00:00.000Z",
    }),
    account_meta_json: null,
    usage_snapshot_json: null,
    usage_fetched_at: null,
    usage_fetching_at: null,
    bench_until: opts.benchUntil ?? null,
    bench_reason: null,
    refreshing_at: null,
    edge_strikes: 0,
    edge_strike_at: null,
    created_at: opts.createdAt ?? NOW,
    updated_at: NOW,
  }
}

async function seedAccount(db: FakeD1, opts: Parameters<typeof accountRow>[0]): Promise<AccountRow> {
  const row = await accountRow(opts)
  db.seed("upstream_accounts", [row as unknown as Record<string, unknown>])
  return row
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

function share(overrides: Partial<SharedAccount["share"]> = {}): SharedAccount["share"] {
  return { teamId: "team_1", teamName: "Acme", ownerLabel: "owner@example.com", ...overrides }
}

/** A `PoolExtension` whose three methods are spies; every one defaults to "nothing shared, nothing governed". */
function fakeExtension(overrides: Partial<PoolExtension> = {}) {
  const ext: PoolExtension = {
    listShared: overrides.listShared ?? vi.fn(async () => []),
    reserveAttempt: overrides.reserveAttempt ?? vi.fn(async () => null),
    setSharedPriority: overrides.setSharedPriority ?? vi.fn(async () => true),
  }
  return ext
}

/** Records every `settle` call so "exactly once, with this outcome" is assertable. */
function recordingLease(): { lease: AttemptLease; settled: string[] } {
  const settled: string[] = []
  return {
    settled,
    lease: {
      settle: async (outcome) => {
        settled.push(outcome)
      },
    },
  }
}

function collectWaitUntil(): { waitUntil: (p: Promise<unknown>) => void; drain: () => Promise<void> } {
  const pending: Promise<unknown>[] = []
  return {
    waitUntil: (p) => pending.push(p),
    drain: async () => {
      await Promise.all(pending)
    },
  }
}

async function drainBody(body: ReadableStream<Uint8Array> | null): Promise<string> {
  if (!body) return ""
  const reader = body.getReader()
  let out = ""
  for (;;) {
    const { done, value } = await reader.read()
    if (done) break
    out += new TextDecoder().decode(value)
  }
  return out
}

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

describe("candidates — shared rows join the viewer's unpinned builtin pool", () => {
  const target = {
    provider: "grok",
    upstreamModel: "grok-4.5",
    isBuiltin: true,
    adapter: { id: "grok" } as ProviderAdapter,
  }

  it("merges shared rows by the viewer's priority, not the owner's, and marks them with `share`", async () => {
    const db = new FakeD1()
    await seedAccount(db, { id: "own_high", userId: "user_1", priority: 10 })
    await seedAccount(db, { id: "own_low", userId: "user_1", priority: 1 })
    const borrowed = await accountRow({ id: "shared_mid", userId: "user_2", priority: 99 })
    const ext = fakeExtension({
      listShared: vi.fn(async () => [{ account: borrowed, priority: 5, share: share() }]),
    })

    const candidates = await poolCandidates(buildEnv(db), "user_1", target, ext)

    expect(candidates.map((c) => c.account.id)).toEqual(["own_high", "shared_mid", "own_low"])
    expect(candidates[1]!.share).toEqual(share())
    expect(candidates[0]!.share).toBeUndefined()
    expect(ext.listShared).toHaveBeenCalledWith(expect.anything(), "user_1", "grok")
  })

  it("breaks a priority tie by created_at, newest first, across own and shared rows", async () => {
    const db = new FakeD1()
    await seedAccount(db, { id: "own_old", userId: "user_1", priority: 5, createdAt: "2026-01-01T00:00:00.000Z" })
    const borrowed = await accountRow({
      id: "shared_new",
      userId: "user_2",
      createdAt: "2026-06-01T00:00:00.000Z",
    })
    const ext = fakeExtension({
      listShared: vi.fn(async () => [{ account: borrowed, priority: 5, share: share() }]),
    })

    const candidates = await poolCandidates(buildEnv(db), "user_1", target, ext)
    expect(candidates.map((c) => c.account.id)).toEqual(["shared_new", "own_old"])
  })

  it("never consults the extension for a pinned target", async () => {
    const db = new FakeD1()
    await seedAccount(db, { id: "own_pinned", userId: "user_1" })
    const ext = fakeExtension({
      listShared: vi.fn(async () => [
        { account: await accountRow({ id: "shared", userId: "user_2" }), priority: 99, share: share() },
      ]),
    })

    const candidates = await poolCandidates(buildEnv(db), "user_1", { ...target, accountId: "own_pinned" }, ext)
    expect(candidates.map((c) => c.account.id)).toEqual(["own_pinned"])
    expect(ext.listShared).not.toHaveBeenCalled()
  })

  it("never consults the extension for a custom (non-builtin) provider", async () => {
    const db = new FakeD1()
    await seedAccount(db, { id: "acc_custom", userId: "user_1", provider: "my-endpoint" })
    const ext = fakeExtension()

    const candidates = await poolCandidates(
      buildEnv(db),
      "user_1",
      { ...target, provider: "my-endpoint", isBuiltin: false },
      ext,
    )
    expect(candidates.map((c) => c.account.id)).toEqual(["acc_custom"])
    expect(ext.listShared).not.toHaveBeenCalled()
  })
})

/** One adapter whose chatCompletions answers from a scripted per-account table. */
function scriptedAdapter(reply: (accountId: string, call: number) => Response): {
  adapter: ProviderAdapter
  calls: () => string[]
} {
  const calls: string[] = []
  return {
    calls: () => calls,
    adapter: {
      id: "grok",
      async chatCompletions(_env, acquired) {
        calls.push(acquired.row.id)
        return reply(acquired.row.id, calls.length)
      },
    },
  }
}

function chatOpts(extra: Record<string, unknown>) {
  return {
    userId: "user_1",
    apiKeyId: "key_1",
    provider: "grok",
    req: {
      model: "grok/grok-4.5",
      rawModel: "grok/grok-4.5",
      upstreamModel: "grok-4.5",
      messages: [{ role: "user", content: "hi" }],
      stream: false,
      rawBody: {},
    },
    ...extra,
  } as Parameters<typeof dispatchChatCompletions>[1]
}

describe("dispatch — reserveAttempt gates every attempt", () => {
  it("a `skip` moves to the next candidate and never spends one of the 8 attempts", async () => {
    const db = new FakeD1()
    for (let i = 0; i < 10; i++) {
      await seedAccount(db, { id: `acc_${i}`, userId: "user_1", priority: 100 - i })
    }
    const { adapter, calls } = scriptedAdapter(() => new Response("{}", { status: 429 }))
    const { waitUntil, drain } = collectWaitUntil()
    const reserveAttempt = vi.fn(async (_env: Env, _ctx: unknown, candidate: { account: { id: string } }) =>
      ["acc_0", "acc_1", "acc_2"].includes(candidate.account.id) ? ({ skip: true } as const) : null,
    )

    const res = await dispatchChatCompletions(
      buildEnv(db),
      chatOpts({ adapter, waitUntil, poolExtension: fakeExtension({ reserveAttempt: reserveAttempt as never }) }),
    )
    await drain()

    expect(res.status).toBe(503)
    // Three skips, then the seven remaining rows are all really attempted:
    // had a skip counted toward MAX_ATTEMPTS (8), only five would have run.
    expect(calls()).toEqual(["acc_3", "acc_4", "acc_5", "acc_6", "acc_7", "acc_8", "acc_9"])
    expect(reserveAttempt).toHaveBeenCalledTimes(10)
  })

  it("`null` (not governed) leaves dispatch exactly as it is without an extension", async () => {
    const db = new FakeD1()
    await seedAccount(db, { id: "acc_1", userId: "user_1" })
    const { adapter, calls } = scriptedAdapter(() => Response.json({ ok: true }))
    const { waitUntil, drain } = collectWaitUntil()

    const res = await dispatchChatCompletions(
      buildEnv(db),
      chatOpts({ adapter, waitUntil, poolExtension: fakeExtension() }),
    )
    await drain()
    expect(res.status).toBe(200)
    expect(calls()).toEqual(["acc_1"])
  })
})

describe("dispatch — the attempt lease settles exactly once, with the log row", () => {
  it("consumed on a non-stream success", async () => {
    const db = new FakeD1()
    await seedAccount(db, { id: "acc_1", userId: "user_1" })
    const { lease, settled } = recordingLease()
    const { adapter } = scriptedAdapter(() => Response.json({ ok: true }))
    const { waitUntil, drain } = collectWaitUntil()

    const res = await dispatchChatCompletions(
      buildEnv(db),
      chatOpts({
        adapter,
        waitUntil,
        poolExtension: fakeExtension({ reserveAttempt: vi.fn(async () => lease) }),
      }),
    )
    await drain()

    expect(res.status).toBe(200)
    expect(settled).toEqual(["consumed"])
    expect(db.rows("request_logs")).toHaveLength(1)
  })

  it("released on a non-stream upstream error passed through", async () => {
    const db = new FakeD1()
    await seedAccount(db, { id: "acc_1", userId: "user_1" })
    const { lease, settled } = recordingLease()
    const { adapter } = scriptedAdapter(() => Response.json({ error: "bad request" }, { status: 400 }))
    const { waitUntil, drain } = collectWaitUntil()

    const res = await dispatchChatCompletions(
      buildEnv(db),
      chatOpts({
        adapter,
        waitUntil,
        poolExtension: fakeExtension({ reserveAttempt: vi.fn(async () => lease) }),
      }),
    )
    await drain()

    expect(res.status).toBe(400)
    expect(settled).toEqual(["released"])
    expect(db.rows("request_logs")).toHaveLength(1)
  })

  it("consumed on a streamed success, at the stream-close log write", async () => {
    const db = new FakeD1()
    await seedAccount(db, { id: "acc_1", userId: "user_1" })
    const { lease, settled } = recordingLease()
    const { adapter } = scriptedAdapter(
      () =>
        new Response('data: {"choices":[{"delta":{"content":"hi"}}]}\n\ndata: [DONE]\n\n', {
          status: 200,
          headers: { "content-type": "text/event-stream" },
        }),
    )
    const { waitUntil, drain } = collectWaitUntil()

    const res = await dispatchChatCompletions(
      buildEnv(db),
      chatOpts({
        adapter,
        waitUntil,
        poolExtension: fakeExtension({ reserveAttempt: vi.fn(async () => lease) }),
        req: {
          model: "grok/grok-4.5",
          rawModel: "grok/grok-4.5",
          upstreamModel: "grok-4.5",
          messages: [{ role: "user", content: "hi" }],
          stream: true,
          rawBody: {},
        },
      }),
    )
    // The lease outlives the returned Response: nothing is settled until the
    // stream closes and its one log row is written.
    expect(settled).toEqual([])
    await drainBody(res.body)
    await drain()

    expect(settled).toEqual(["consumed"])
    expect(db.rows("request_logs")[0]).toMatchObject({ error_code: null })
  })

  it("released on a bench-type failure, once per attempt", async () => {
    const db = new FakeD1()
    await seedAccount(db, { id: "acc_1", userId: "user_1", priority: 2 })
    await seedAccount(db, { id: "acc_2", userId: "user_1", priority: 1 })
    const first = recordingLease()
    const second = recordingLease()
    const { adapter } = scriptedAdapter(() => new Response("{}", { status: 401 }))
    const { waitUntil, drain } = collectWaitUntil()

    const res = await dispatchChatCompletions(
      buildEnv(db),
      chatOpts({
        adapter,
        waitUntil,
        poolExtension: fakeExtension({
          reserveAttempt: vi.fn(async (_env: Env, _ctx: unknown, candidate: { account: { id: string } }) =>
            candidate.account.id === "acc_1" ? first.lease : second.lease,
          ) as never,
        }),
      }),
    )
    await drain()

    expect(res.status).toBe(503)
    expect(first.settled).toEqual(["released"])
    expect(second.settled).toEqual(["released"])
  })

  it("released when a committed stream fails before any output", async () => {
    const db = new FakeD1()
    await seedAccount(db, { id: "acc_1", userId: "user_1" })
    const { lease, settled } = recordingLease()
    // 500 is not a bench-type status, so the walk hands it back and the eager
    // transport turns it into a terminal in-stream error frame.
    const { adapter } = scriptedAdapter(() => Response.json({ error: "boom" }, { status: 500 }))
    const { waitUntil, drain } = collectWaitUntil()

    const res = await dispatchChatCompletions(
      buildEnv(db),
      chatOpts({
        adapter,
        waitUntil,
        poolExtension: fakeExtension({ reserveAttempt: vi.fn(async () => lease) }),
        req: {
          model: "grok/grok-4.5",
          rawModel: "grok/grok-4.5",
          upstreamModel: "grok-4.5",
          messages: [{ role: "user", content: "hi" }],
          stream: true,
          rawBody: {},
        },
      }),
    )
    await drainBody(res.body)
    await drain()

    expect(settled).toEqual(["released"])
    expect(db.rows("request_logs")[0]).toMatchObject({ error_code: "upstream_error" })
  })

  it("a settle that rejects never breaks the response", async () => {
    const db = new FakeD1()
    await seedAccount(db, { id: "acc_1", userId: "user_1" })
    const { adapter } = scriptedAdapter(() => Response.json({ ok: true }))
    const { waitUntil, drain } = collectWaitUntil()
    const lease: AttemptLease = {
      settle: async () => {
        throw new Error("ledger unreachable")
      },
    }

    const res = await dispatchChatCompletions(
      buildEnv(db),
      chatOpts({
        adapter,
        waitUntil,
        poolExtension: fakeExtension({ reserveAttempt: vi.fn(async () => lease) }),
      }),
    )
    await drain()
    expect(res.status).toBe(200)
    expect(db.rows("request_logs")).toHaveLength(1)
  })

  it("shared rows log the shared account id against the calling user", async () => {
    const db = new FakeD1()
    const borrowed = await accountRow({ id: "shared_1", userId: "user_2" })
    const { adapter } = scriptedAdapter(() => Response.json({ ok: true }))
    const { waitUntil, drain } = collectWaitUntil()

    const res = await dispatchChatCompletions(
      buildEnv(db),
      chatOpts({
        adapter,
        waitUntil,
        poolExtension: fakeExtension({
          listShared: vi.fn(async () => [{ account: borrowed, priority: 1, share: share() }]),
        }),
      }),
    )
    await drain()

    expect(res.status).toBe(200)
    expect(db.rows("request_logs")[0]).toMatchObject({
      user_id: "user_1",
      api_key_id: "key_1",
      account_id: "shared_1",
      provider: "grok",
    })
  })
})

describe("catalog — a shared account makes a builtin provider bound", () => {
  it("fetches the live list with the shared credential and never returns it", async () => {
    const db = new FakeD1()
    const borrowed = await accountRow({ id: "shared_1", userId: "user_2" })
    let authorization: string | null = null
    globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      if (!String(input).includes("/models")) throw new Error(`unexpected fetch: ${String(input)}`)
      authorization = new Headers(init?.headers).get("authorization")
      return Response.json({ data: [{ id: "grok-4.5", name: "Grok 4.5" }] })
    }) as typeof fetch

    const { models, providers } = await listModelsForUser(buildEnv(db), "user_1", {
      poolExtension: fakeExtension({
        listShared: vi.fn(async (_env, _viewer, provider) =>
          provider === "grok" ? [{ account: borrowed, priority: 1, share: share() }] : [],
        ),
      }),
    })

    expect(models.map((m) => m.id)).toContain("grok/grok-4.5")
    expect(providers.find((p) => p.provider === "grok")!.models).toHaveLength(1)
    expect(authorization).toContain("token-shared_1")
    expect(JSON.stringify(models)).not.toContain("token-shared_1")
  })

  it("stays empty for that provider without an extension", async () => {
    const db = new FakeD1()
    globalThis.fetch = (async () => {
      throw new Error("no upstream call expected")
    }) as typeof fetch
    const { providers } = await listModelsForUser(buildEnv(db), "user_1")
    expect(providers.find((p) => p.provider === "grok")!.models).toEqual([])
  })
})

function sessionRequest(method: string, cookie: string, body?: unknown): RequestInit {
  return {
    method,
    headers: { "content-type": "application/json", host: "app.example.com", cookie },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  }
}

const executionCtx = {
  waitUntil: (p: Promise<unknown>) => {
    void p.catch(() => {})
  },
  passThroughOnException() {},
} as ExecutionContext

describe("GET /api/providers/:provider/accounts — the borrower's view", () => {
  it("returns the shared row with its share descriptor and no usage surface", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    const env = buildEnv(db)
    const cookie = (await createSession(env, "user_1")).cookie.split(";")[0]!
    const borrowed = await accountRow({ id: "shared_1", userId: "user_2", label: "owner-seat" })
    globalThis.fetch = (async () => {
      throw new Error("a borrower must never trigger the owner's usage probe")
    }) as typeof fetch

    const app = createApplication({
      poolExtension: fakeExtension({
        listShared: vi.fn(async (_env, _viewer, provider) =>
          provider === "grok" ? [{ account: borrowed, priority: 3, share: share() }] : [],
        ),
      }),
    })
    const res = await app.request("/api/providers/grok/accounts", sessionRequest("GET", cookie), env, executionCtx)
    const json = (await res.json()) as { accounts: Record<string, unknown>[] }

    expect(res.status).toBe(200)
    expect(json.accounts).toHaveLength(1)
    expect(json.accounts[0]).toMatchObject({
      id: "shared_1",
      priority: 3,
      label: "owner-seat",
      share: share(),
      usage: null,
      error: null,
      account: null,
    })
    expect(JSON.stringify(json)).not.toContain("token-shared_1")
  })

  it("lists rows in the router's merged order, a promoted shared row first and Active", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    const env = buildEnv(db)
    const cookie = (await createSession(env, "user_1")).cookie.split(";")[0]!
    await seedAccount(db, { id: "own_1", userId: "user_1", priority: 1 })
    const borrowed = await accountRow({ id: "shared_1", userId: "user_2" })
    // The usage probe for the viewer's own row is allowed to fail; it must not
    // decide the dot, and the shared row must not be probed at all.
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      if (String(input).includes("shared_1")) throw new Error("shared rows are never probed")
      return new Response("upstream down", { status: 500 })
    }) as typeof fetch

    const app = createApplication({
      poolExtension: fakeExtension({
        listShared: vi.fn(async (_env, _viewer, provider) =>
          provider === "grok" ? [{ account: borrowed, priority: 9, share: share() }] : [],
        ),
      }),
    })
    const res = await app.request("/api/providers/grok/accounts", sessionRequest("GET", cookie), env, executionCtx)
    const json = (await res.json()) as { accounts: { id: string; status: string }[] }

    expect(json.accounts.map((a) => a.id)).toEqual(["shared_1", "own_1"])
    expect(json.accounts.map((a) => a.status)).toEqual(["active", "standby"])
  })
})

describe("mutations on a borrowed row", () => {
  async function borrowedApp(db: FakeD1, setSharedPriority = vi.fn(async () => true)) {
    seedUser(db, "user_1")
    const env = buildEnv(db)
    const cookie = (await createSession(env, "user_1")).cookie.split(";")[0]!
    const borrowed = await accountRow({ id: "shared_1", userId: "user_2" })
    const ext = fakeExtension({
      listShared: vi.fn(async (_env, _viewer, provider) =>
        provider === "grok" ? [{ account: borrowed, priority: 3, share: share() }] : [],
      ),
      setSharedPriority,
    })
    return { env, cookie, ext, app: createApplication({ poolExtension: ext }) }
  }

  it.each([
    ["PATCH", "/api/providers/grok/accounts/shared_1", { custom_label: "mine" }],
    ["POST", "/api/providers/grok/accounts/shared_1/unpause", undefined],
    ["DELETE", "/api/providers/grok/accounts/shared_1", undefined],
  ])("%s %s is 403 forbidden, not 404", async (method, path, body) => {
    const db = new FakeD1()
    const { app, env, cookie } = await borrowedApp(db)
    const res = await app.request(path, sessionRequest(method, cookie, body), env, executionCtx)
    expect(res.status).toBe(403)
    expect(await res.json()).toEqual({ error: "forbidden" })
  })

  it("an id that is neither owned nor shared is still 404", async () => {
    const db = new FakeD1()
    const { app, env, cookie } = await borrowedApp(db)
    const res = await app.request(
      "/api/providers/grok/accounts/ghost",
      sessionRequest("DELETE", cookie),
      env,
      executionCtx,
    )
    expect(res.status).toBe(404)
  })

  it("promote delegates to setSharedPriority with one above the merged pool's maximum", async () => {
    const db = new FakeD1()
    const setSharedPriority = vi.fn(async () => true)
    const { app, env, cookie } = await borrowedApp(db, setSharedPriority)
    await seedAccount(db, { id: "own_1", userId: "user_1", priority: 9 })

    const res = await app.request(
      "/api/providers/grok/accounts/shared_1/promote",
      sessionRequest("POST", cookie),
      env,
      executionCtx,
    )
    expect(res.status).toBe(200)
    expect(await res.json()).toEqual({ ok: true })
    expect(setSharedPriority).toHaveBeenCalledWith(expect.anything(), "user_1", "shared_1", 10)
    // The owner's row is never rewritten by a borrower's promote.
    expect(db.rows("upstream_accounts").map((r) => r.id)).toEqual(["own_1"])
  })

  it("promote is 404 when the extension refuses", async () => {
    const db = new FakeD1()
    const { app, env, cookie } = await borrowedApp(db, vi.fn(async () => false))
    const res = await app.request(
      "/api/providers/grok/accounts/shared_1/promote",
      sessionRequest("POST", cookie),
      env,
      executionCtx,
    )
    expect(res.status).toBe(404)
  })
})

describe("edition composition — extensions never leak between applications", () => {
  it("one app's shared pool is invisible to another app built from the same source", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    const env = buildEnv(db)
    const cookie = (await createSession(env, "user_1")).cookie.split(";")[0]!
    const borrowed = await accountRow({ id: "shared_1", userId: "user_2" })
    const hostedListShared = vi.fn(async () => [{ account: borrowed, priority: 1, share: share() }])

    const hosted = createApplication({ poolExtension: fakeExtension({ listShared: hostedListShared }) })
    const standalone = createApplication()

    const hostedRes = await hosted.request(
      "/api/providers/grok/accounts",
      sessionRequest("GET", cookie),
      env,
      executionCtx,
    )
    const standaloneRes = await standalone.request(
      "/api/providers/grok/accounts",
      sessionRequest("GET", cookie),
      env,
      executionCtx,
    )

    expect(((await hostedRes.json()) as { accounts: unknown[] }).accounts).toHaveLength(1)
    expect(((await standaloneRes.json()) as { accounts: unknown[] }).accounts).toHaveLength(0)
    expect(hostedListShared).toHaveBeenCalledTimes(1)
  })
})
