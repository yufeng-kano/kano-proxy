export type ProviderId = "claude-code" | "codex" | "grok"

export type AccountStatus = "active" | "standby" | "benched" | "unusable"

export type User = {
  id: string
  email: string
  name: string | null
  picture_url: string | null
}

export type UsageWindow = {
  label: string
  utilization: number | null
  resets_at: string | null
}

export type ProviderAccount = {
  id: string
  priority: number
  status: AccountStatus
  label: string | null
  account: Record<string, unknown> | null
  usage: { windows: UsageWindow[] } | null
  error: string | null
  stale: boolean
}

export type AccountsResponse = {
  available: boolean
  accounts: ProviderAccount[]
  models: string[]
  error: string | null
}

export type ApiKey = {
  id: string
  name: string
  key_prefix: string
  created_at: string
  last_used_at: string | null
}

export type CreatedKey = ApiKey & {
  key: string
}

export type LoginStart = {
  login_id: string
  authorization_url?: string
  user_code?: string
  verification_uri?: string
  verification_uri_complete?: string
  interval?: number
}

export const PROVIDERS: { id: ProviderId; name: string; blurb: string }[] = [
  { id: "claude-code", name: "Claude Code", blurb: "Anthropic subscription OAuth" },
  { id: "codex", name: "Codex", blurb: "ChatGPT / Codex OAuth" },
  { id: "grok", name: "Grok", blurb: "xAI SuperGrok device code" },
]

export type CatalogModel = {
  id: string
  provider: ProviderId
  upstream: string
  display_name: string
  available: boolean
  owned_by: ProviderId
  object: "model"
}

export type ModelsResponse = {
  object: "list"
  data: CatalogModel[]
  providers?: Array<{
    provider: ProviderId
    count: number
    error: string | null
    cached: boolean
  }>
  openai_base?: string
  anthropic_base?: string
}
