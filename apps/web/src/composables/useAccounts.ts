import { reactive, ref } from "vue"
import { listAccounts } from "@/services/api"
import {
  CACHE_TTL_MS,
  isAccountsCacheFresh,
  readAccountsCache,
  writeAccountsCache,
} from "@/services/cache"
import { PROVIDER_IDS, type AccountsResponse, type ProviderId, type RequestEdits, type RoutingStrategy } from "@/types"

type ProviderState = {
  data: AccountsResponse | null
  loading: boolean
  error: string | null
}

function emptyState(): ProviderState {
  return {
    data: null,
    loading: false,
    error: null,
  }
}

const byProvider = reactive<Record<ProviderId, ProviderState>>({
  "claude-code": emptyState(),
  codex: emptyState(),
  grok: emptyState(),
  antigravity: emptyState(),
})

export function useAccounts() {
  const userId = ref<string | null>(null)

  function setUserId(id: string | null) {
    userId.value = id
  }

  /**
   * Cache-first load.
   * - Always paint localStorage cache immediately when present.
   * - Network fetch only if cache missing/stale (>2 min) or opts.refresh.
   * - Manual refresh passes refresh=true to the API, which also bypasses the
   *   server's own 2 min usage cache (docs/providers.md § Usage cache).
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
      state.loading = false
    } else if (!state.data) {
      state.loading = true
    }

    // Within TTL: keep cache, skip network unless forced
    if (!forceNetwork && isAccountsCacheFresh(userId.value, provider)) {
      state.loading = false
      return
    }

    state.error = null
    try {
      const data = await listAccounts(provider, { refresh: force })
      state.data = data
      writeAccountsCache(userId.value, provider, data)
    } catch (e) {
      const msg = e instanceof Error ? e.message : "Failed to load accounts"
      state.error = msg
      // keep cache on failure
    } finally {
      state.loading = false
    }
  }

  async function loadAll(opts?: { refresh?: boolean }) {
    await Promise.all(PROVIDER_IDS.map((p) => loadProvider(p, opts)))
  }

  /**
   * Records the strategy a write just saved. The whole payload is re-cached
   * with it, so the next cache-first paint shows the new value instead of the
   * one the last read happened to carry — a re-fetch would cost an upstream
   * usage round trip to learn one field the response already told us.
   */
  function setStrategy(provider: ProviderId, strategy: RoutingStrategy) {
    const data = byProvider[provider].data
    if (!data) return
    data.strategy = strategy
    writeAccountsCache(userId.value, provider, data)
  }

  /**
   * Records the switches a write just saved on that one row, the same way:
   * the write's answer is the stored pair, and nothing else on the row moved.
   */
  function setAccountEdits(provider: ProviderId, id: string, edits: RequestEdits) {
    const data = byProvider[provider].data
    const row = data?.accounts.find((a) => a.id === id)
    if (!data || !row) return
    row.strip_fast_mode = edits.strip_fast_mode
    row.strip_claude_code_line = edits.strip_claude_code_line
    row.force_cache_ttl_1h = edits.force_cache_ttl_1h
    writeAccountsCache(userId.value, provider, data)
  }

  return {
    byProvider,
    setUserId,
    loadProvider,
    loadAll,
    setStrategy,
    setAccountEdits,
    CACHE_TTL_MS,
  }
}
