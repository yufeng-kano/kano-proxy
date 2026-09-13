import type { Env } from "../env"
import { estimateCost, getPriceTable } from "../pricing/litellm"
import { newId, nowIso } from "../utils/id"

export async function logRequest(
  env: Env,
  entry: {
    /** UTC epoch milliseconds captured before dispatch. */
    startedAt?: number
    userId: string
    apiKeyId?: string | null
    provider: string
    model: string
    accountId?: string | null
    statusCode: number
    latencyMs: number
    promptTokens?: number | null
    completionTokens?: number | null
    cacheReadInputTokens?: number | null
    cacheCreationInputTokens?: number | null
    errorCode?: string | null
    /** Last upstream HTTP response status observed; NULL means no upstream response headers arrived. */
    upstreamStatus?: number | null
    /** The model-group alias this request was addressed to, if any (docs/database.md `request_logs.group_name`). `model`/`provider` above always store the expanded canonical target. */
    groupName?: string | null
  },
): Promise<void> {
  try {
    const startedAt = new Date(entry.startedAt ?? Date.now()).toISOString()
    // Name snapshots (docs/database.md § request_logs): the key's name and the
    // account's display label as they are right now, so a record deleted later
    // still reads by its last name on the Logs page. Two point reads off the
    // request path; a record already gone simply leaves NULL.
    const [key, account] = await Promise.all([
      entry.apiKeyId
        ? env.DB.prepare("SELECT name FROM api_keys WHERE id = ?").bind(entry.apiKeyId).first<{ name: string }>()
        : null,
      entry.accountId
        ? env.DB.prepare("SELECT custom_label, label FROM upstream_accounts WHERE id = ?")
            .bind(entry.accountId)
            .first<{ custom_label: string | null; label: string | null }>()
        : null,
    ])
    const apiKeyName = key?.name ?? null
    const accountLabel = account ? account.custom_label || account.label || null : null

    // Estimated USD at write time (docs/pricing.md). getPriceTable never
    // fetches — memo/KV only — so a missing table degrades to NULL cost
    // without delaying the deferred log write.
    let cost: number | null = null
    try {
      const table = await getPriceTable(env)
      if (table) {
        cost = estimateCost(table, entry.model, {
          promptTokens: entry.promptTokens ?? null,
          completionTokens: entry.completionTokens ?? null,
          cacheReadInputTokens: entry.cacheReadInputTokens ?? null,
          cacheCreationInputTokens: entry.cacheCreationInputTokens ?? null,
        })
      }
    } catch {
      // pricing must never break logging
    }
    await env.DB.prepare(
      `INSERT INTO request_logs
       (id, user_id, api_key_id, provider, model, account_id, status_code, latency_ms,
        prompt_tokens, completion_tokens, cache_read_input_tokens, cache_creation_input_tokens,
        cost, error_code, upstream_status, group_name, api_key_name, account_label, created_at, started_at)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    )
      .bind(
        newId("log"),
        entry.userId,
        entry.apiKeyId ?? null,
        entry.provider,
        entry.model,
        entry.accountId ?? null,
        entry.statusCode,
        entry.latencyMs,
        entry.promptTokens ?? null,
        entry.completionTokens ?? null,
        entry.cacheReadInputTokens ?? null,
        entry.cacheCreationInputTokens ?? null,
        cost,
        entry.errorCode ?? null,
        entry.upstreamStatus ?? null,
        entry.groupName ?? null,
        apiKeyName,
        accountLabel,
        nowIso(),
        startedAt,
      )
      .run()
  } catch {
    // logging must never break proxy
  }
}
