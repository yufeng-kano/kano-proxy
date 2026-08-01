import type { ProviderId } from "../env"
import { claudeCodeAdapter } from "./claude-code"
import { codexAdapter } from "./codex"
import { grokAdapter } from "./grok"
import type { ProviderAdapter } from "./types"

const adapters: Record<ProviderId, ProviderAdapter> = {
  "claude-code": claudeCodeAdapter,
  codex: codexAdapter,
  grok: grokAdapter,
}

export function getAdapter(provider: ProviderId): ProviderAdapter {
  return adapters[provider]
}

export type { ProviderAdapter, ChatCompletionRequest, AccountUsageView } from "./types"
