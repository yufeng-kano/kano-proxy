import { describe, expect, it } from "vitest"
import { encryptJson } from "../src/crypto/token_crypto"
import type { AccountRow } from "../src/db/accounts"
import type { Env } from "../src/env"
import { refreshAccountUsageInBackground } from "../src/providers/usage_refresh"
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
