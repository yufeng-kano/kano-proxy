import type { Env } from "../env"
import { estimateCost, getPriceTable } from "../pricing/litellm"
import { newId, nowIso } from "../utils/id"

export async function logRequest(
  env: Env,
  entry: {
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
  },
): Promise<void> {
  try {
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
        cost, error_code, created_at)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
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
        nowIso(),
      )
      .run()
  } catch {
    // logging must never break proxy
  }
}
