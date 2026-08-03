export type Env = {
  DB: D1Database
  BENCH: KVNamespace
  CACHE: KVNamespace
  APP_URL: string
  GOOGLE_REDIRECT_URI: string
  CODEX_REDIRECT_URI: string
  GOOGLE_CLIENT_ID?: string
  GOOGLE_CLIENT_SECRET?: string
  SESSION_SECRET?: string
  TOKEN_ENCRYPTION_KEY?: string
  CLAUDE_CODE_OAUTH_CLIENT_ID?: string
  CODEX_OAUTH_CLIENT_ID?: string
  GROK_OAUTH_CLIENT_ID?: string
  /** Codex egress relay (docs/codex-relay.md) — both required to enable; either missing means direct to chatgpt.com. */
  CODEX_RELAY_URL?: string
  CODEX_RELAY_SA_KEY?: string
  REQUEST_LOG_RETENTION_DAYS?: string
  GITHUB_REPO?: string
  GITHUB_TOKEN?: string
}

export type ProviderId = "claude-code" | "codex" | "grok"

export const PROVIDERS: ProviderId[] = ["claude-code", "codex", "grok"]

export function isProviderId(v: string): v is ProviderId {
  return (PROVIDERS as string[]).includes(v)
}
