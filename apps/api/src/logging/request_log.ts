import type { Env } from "../env"
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
    errorCode?: string | null
  },
): Promise<void> {
  try {
    await env.DB.prepare(
      `INSERT INTO request_logs
       (id, user_id, api_key_id, provider, model, account_id, status_code, latency_ms,
        prompt_tokens, completion_tokens, error_code, created_at)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
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
        entry.errorCode ?? null,
        nowIso(),
      )
      .run()
  } catch {
    // logging must never break proxy
  }
}
