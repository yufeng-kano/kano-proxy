import { describe, expect, it } from "vitest"
import { encryptJson } from "../src/crypto/token_crypto"
import { acquireUsageLock, type AccountRow } from "../src/db/accounts"
import type { Env } from "../src/env"
import { fetchAndPersistUsage, refreshAccountUsageInBackground } from "../src/providers/usage_refresh"
import type { ProviderAdapter } from "../src/providers/types"
import { FakeD1 } from "./helpers/fake_d1"

const TOKEN_KEY = "usage-refresh-test-key"

function row(encryptedPayload: string): AccountRow {
  return {
    id: "acc_1",
    user_id: "user_1",
    provider: "grok",
    external_account_id: null,
    label: null,
    custom_label: null,
    priority: 1,
    encrypted_payload: encryptedPayload,
    account_meta_json: null,
    usage_snapshot_json: null,
    usage_fetched_at: null,
    usage_fetching_at: null,
    bench_until: null,
    bench_reason: null,
    refreshing_at: null,
    created_at: "2026-01-01T00:00:00.000Z",
    updated_at: "2026-01-01T00:00:00.000Z",
  }
}

describe("background usage refresh", () => {
  it("re-reads the account after its usage lock and uses a post-refresh credential", async () => {
    const db = new FakeD1()
    const stalePayload = await encryptJson(TOKEN_KEY, { access_token: "stale-access" })
    const freshPayload = await encryptJson(TOKEN_KEY, { access_token: "fresh-access" })
    const capturedRow = row(stalePayload)
    db.seed("upstream_accounts", [capturedRow])
    // Simulate dispatch completing OAuth refresh after it built its candidates.
    db.rows("upstream_accounts")[0]!.encrypted_payload = freshPayload
    const env = { DB: db as unknown as D1Database, TOKEN_ENCRYPTION_KEY: TOKEN_KEY } as Env
    let usageCredential: string | undefined
    const adapter: ProviderAdapter = {
      id: "grok",
      chatCompletions: async () => new Response(),
      fetchUsage: async (_env, account) => {
        usageCredential = account.credential.access_token
        return { windows: [], account: {} }
      },
    }

    await refreshAccountUsageInBackground(env, capturedRow, adapter)
    expect(usageCredential).toBe("fresh-access")
  })
})

describe("fetchAndPersistUsage — soft failure vs. thrown (docs/providers.md § Usage cache)", () => {
  const PRIOR_SNAPSHOT = {
    windows: [{ label: "5h", utilization: 10, resets_at: null }],
    account: { email: "a@example.com" },
    error: null,
    stale: false,
    edgeBlocked: false,
  }

  async function setup(priorSnapshot: typeof PRIOR_SNAPSHOT | null) {
    const db = new FakeD1()
    const payload = await encryptJson(TOKEN_KEY, { access_token: "tok" })
    const seeded = row(payload)
    if (priorSnapshot) {
      seeded.usage_snapshot_json = JSON.stringify(priorSnapshot)
      seeded.usage_fetched_at = new Date(Date.now() - 120_000).toISOString()
    }
    db.seed("upstream_accounts", [seeded])
    const env = { DB: db as unknown as D1Database, TOKEN_ENCRYPTION_KEY: TOKEN_KEY } as Env
    const lockToken = await acquireUsageLock(env.DB, "acc_1")
    if (!lockToken) throw new Error("expected to acquire the lock")
    return { db, env, seeded, lockToken }
  }

  it("preserves the prior snapshot on a soft failure (error, no windows), flagged stale", async () => {
    const before = new Date().toISOString()
    const { db, env, seeded, lockToken } = await setup(PRIOR_SNAPSHOT)
    const adapter: ProviderAdapter = {
      id: "grok",
      chatCompletions: async () => new Response(),
      fetchUsage: async () => ({
        windows: [],
        account: {},
        error: "usage 429",
        stale: true,
        edgeBlocked: false,
      }),
    }

    const result = await fetchAndPersistUsage(env, seeded, adapter, lockToken)

    expect(result.ok).toBe(true)
    if (!result.ok) throw new Error("unreachable")
    expect(result.snapshot).toEqual({
      windows: PRIOR_SNAPSHOT.windows,
      account: PRIOR_SNAPSHOT.account,
      error: "usage 429",
      stale: true,
      edgeBlocked: false,
    })
    const stored = db.rows("upstream_accounts")[0]!
    expect(JSON.parse(stored.usage_snapshot_json as string)).toEqual(result.snapshot)
    // The read genuinely happened: the lock is released via a write, not a bare release.
    expect(stored.usage_fetching_at).toBeNull()
    expect((stored.usage_fetched_at as string) >= before).toBe(true)
  })

  it("writes the empty/errored snapshot unchanged when there is no prior snapshot", async () => {
    const before = new Date().toISOString()
    const { db, env, seeded, lockToken } = await setup(null)
    const adapter: ProviderAdapter = {
      id: "grok",
      chatCompletions: async () => new Response(),
      fetchUsage: async () => ({
        windows: [],
        account: {},
        error: "usage 429",
        stale: true,
        edgeBlocked: false,
      }),
    }

    const result = await fetchAndPersistUsage(env, seeded, adapter, lockToken)

    expect(result.ok).toBe(true)
    if (!result.ok) throw new Error("unreachable")
    expect(result.snapshot).toEqual({
      windows: [],
      account: {},
      error: "usage 429",
      stale: true,
      edgeBlocked: false,
    })
    const stored = db.rows("upstream_accounts")[0]!
    expect(JSON.parse(stored.usage_snapshot_json as string)).toEqual(result.snapshot)
    expect(stored.usage_fetching_at).toBeNull()
    expect((stored.usage_fetched_at as string) >= before).toBe(true)
  })

  it("lets windows win even when the incoming snapshot also carries an error", async () => {
    const before = new Date().toISOString()
    const { db, env, seeded, lockToken } = await setup(PRIOR_SNAPSHOT)
    const adapter: ProviderAdapter = {
      id: "grok",
      chatCompletions: async () => new Response(),
      fetchUsage: async () => ({
        windows: [{ label: "5h", utilization: 42, resets_at: null }],
        account: { email: "new@example.com" },
        error: "usage degraded",
        stale: false,
        edgeBlocked: false,
      }),
    }

    const result = await fetchAndPersistUsage(env, seeded, adapter, lockToken)

    expect(result.ok).toBe(true)
    if (!result.ok) throw new Error("unreachable")
    expect(result.snapshot).toEqual({
      windows: [{ label: "5h", utilization: 42, resets_at: null }],
      account: { email: "new@example.com" },
      error: "usage degraded",
      stale: false,
      edgeBlocked: false,
    })
    const stored = db.rows("upstream_accounts")[0]!
    expect(JSON.parse(stored.usage_snapshot_json as string)).toEqual(result.snapshot)
    expect(stored.usage_fetching_at).toBeNull()
    expect((stored.usage_fetched_at as string) >= before).toBe(true)
  })

  it("still releases the lock without writing on a thrown failure, preserving the prior snapshot", async () => {
    const { db, env, seeded, lockToken } = await setup(PRIOR_SNAPSHOT)
    const priorFetchedAt = (db.rows("upstream_accounts")[0]!.usage_fetched_at as string)
    const adapter: ProviderAdapter = {
      id: "grok",
      chatCompletions: async () => new Response(),
      fetchUsage: async () => {
        throw new Error("network error")
      },
    }

    const result = await fetchAndPersistUsage(env, seeded, adapter, lockToken)

    expect(result).toEqual({ ok: false, error: "network error" })
    const stored = db.rows("upstream_accounts")[0]!
    expect(JSON.parse(stored.usage_snapshot_json as string)).toEqual(PRIOR_SNAPSHOT)
    expect(stored.usage_fetched_at).toBe(priorFetchedAt)
    expect(stored.usage_fetching_at).toBeNull()
  })
})
