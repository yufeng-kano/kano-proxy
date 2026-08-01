import { reactive, ref } from "vue"
import { listAccounts } from "@/services/api"
import {
  CACHE_TTL_MS,
  isAccountsCacheFresh,
  readAccountsCache,
  writeAccountsCache,
} from "@/services/cache"
import type { AccountsResponse, ProviderId } from "@/types"

type ProviderState = {
  data: AccountsResponse | null
  loading: boolean
  refreshing: boolean
  error: string | null
  fromCache: boolean
}

function emptyState(): ProviderState {
  return {
    data: null,
    loading: false,
    refreshing: false,
    error: null,
    fromCache: false,
  }
}

const byProvider = reactive<Record<ProviderId, ProviderState>>({
  "claude-code": emptyState(),
  codex: emptyState(),
  grok: emptyState(),
})

const PROVIDER_IDS: ProviderId[] = ["claude-code", "codex", "grok"]

export function useAccounts() {
  const userId = ref<string | null>(null)

  function setUserId(id: string | null) {
    userId.value = id
  }

  /**
   * Cache-first load.
   * - Always paint sessionStorage cache immediately when present.
   * - Network fetch only if cache missing/stale (>90s) or opts.refresh.
   * - Manual refresh passes refresh=true to API (bypass backend KV too).
   */
  async function loadProvider(
    provider: ProviderId,
    opts?: { refresh?: boolean; /** force network even if cache fresh */ forceNetwork?: boolean },
  ) {
    const state = byProvider[provider]
    const force = !!opts?.refresh
    const forceNetwork = !!opts?.forceNetwork || force

    const cached = readAccountsCache(userId.value, provider)
    if (cached) {
      state.data = cached
      state.fromCache = true
      state.loading = false
    } else if (!state.data) {
      state.loading = true
    }

    // Within TTL: keep cache, skip network unless forced
    if (!forceNetwork && isAccountsCacheFresh(userId.value, provider, CACHE_TTL_MS)) {
      state.loading = false
      state.refreshing = false
      return
    }

    state.refreshing = true
    state.error = null
    try {
      // Only ask backend to skip KV when user explicitly refreshes
      const data = await listAccounts(provider, { refresh: force })
      state.data = data
      state.fromCache = false
      writeAccountsCache(userId.value, provider, data)
    } catch (e) {
      const msg = e instanceof Error ? e.message : "Failed to load accounts"
      state.error = msg
      // keep cache on failure
    } finally {
      state.loading = false
      state.refreshing = false
    }
  }

  async function loadAll(opts?: { refresh?: boolean; forceNetwork?: boolean }) {
    await Promise.all(PROVIDER_IDS.map((p) => loadProvider(p, opts)))
  }

  function invalidateLocal(provider: ProviderId) {
    byProvider[provider].data = null
    byProvider[provider].fromCache = false
  }

  return {
    byProvider,
    setUserId,
    loadProvider,
    loadAll,
    invalidateLocal,
    CACHE_TTL_MS,
  }
}
