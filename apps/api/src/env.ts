export type Env = {
  DB: D1Database
  BENCH: KVNamespace
  CACHE: KVNamespace
  APP_URL: string
  GOOGLE_REDIRECT_URI: string
  GOOGLE_CLIENT_ID?: string
  GOOGLE_CLIENT_SECRET?: string
  SESSION_SECRET?: string
  TOKEN_ENCRYPTION_KEY?: string
  CLAUDE_CODE_OAUTH_CLIENT_ID?: string
  CODEX_OAUTH_CLIENT_ID?: string
  GROK_OAUTH_CLIENT_ID?: string
  /** Antigravity is a confidential Google OAuth client: both id and secret are needed (docs/auth.md § Antigravity). */
  ANTIGRAVITY_OAUTH_CLIENT_ID?: string
  ANTIGRAVITY_OAUTH_CLIENT_SECRET?: string
  /** Antigravity Hub version inside the upstream `User-Agent`; unset uses the pinned fallback. */
  ANTIGRAVITY_CLIENT_VERSION?: string
  /** Codex egress relay (docs/codex-relay.md) — both required to enable; either missing means direct to chatgpt.com. */
  CODEX_RELAY_URL?: string
  CODEX_RELAY_SA_KEY?: string
  REQUEST_LOG_RETENTION_DAYS?: string
  /** Per-attempt wait for upstream response headers; invalid or absent uses 180 seconds. */
  UPSTREAM_FIRST_BYTE_TIMEOUT_MS?: string
  GITHUB_REPO?: string
  GITHUB_TOKEN?: string
}

export type ProviderId = "claude-code" | "codex" | "grok" | "antigravity"

export const PROVIDERS: ProviderId[] = ["claude-code", "codex", "grok", "antigravity"]

export function isProviderId(v: string): v is ProviderId {
  return (PROVIDERS as string[]).includes(v)
}
