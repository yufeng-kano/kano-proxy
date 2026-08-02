import { reactive, ref } from "vue"
import { listCustomProviders } from "@/services/api"
import {
  CACHE_TTL_MS,
  isCustomProvidersCacheFresh,
  readCustomProvidersCache,
  writeCustomProvidersCache,
} from "@/services/cache"
import type { CustomProvider } from "@/types"

type CustomProvidersState = {
  data: CustomProvider[] | null
  loading: boolean
  refreshing: boolean
  error: string | null
  fromCache: boolean
}

function emptyState(): CustomProvidersState {
  return {
    data: null,
    loading: false,
    refreshing: false,
    error: null,
    fromCache: false,
  }
}

// Module-level singleton, same pattern as useAccounts.ts's `byProvider`: state
// is shared across every component that calls this composable, so navigating
// Accounts -> Models reuses what's already loaded instead of re-fetching.
const state = reactive<CustomProvidersState>(emptyState())

export function useCustomProviders() {
  const userId = ref<string | null>(null)

  function setUserId(id: string | null) {
    userId.value = id
  }

  /**
   * Cache-first load.
   * - Always paint sessionStorage cache immediately when present.
   * - Network fetch only if cache missing/stale (>90s) or opts.refresh.
   * - Manual refresh (opts.refresh) also bypasses server-side caching upstream.
   */
  async function load(opts?: { refresh?: boolean; /** force network even if cache fresh */ forceNetwork?: boolean }) {
    const force = !!opts?.refresh
    const forceNetwork = !!opts?.forceNetwork || force

    const cached = readCustomProvidersCache(userId.value)
    if (cached) {
      state.data = cached
      state.fromCache = true
      state.loading = false
    } else if (!state.data) {
      state.loading = true
    }

    // Within TTL: keep cache, skip network unless forced
    if (!forceNetwork && isCustomProvidersCacheFresh(userId.value, CACHE_TTL_MS)) {
      state.loading = false
      state.refreshing = false
      return
    }

    state.refreshing = true
    state.error = null
    try {
      const data = await listCustomProviders()
      state.data = data
      state.fromCache = false
      writeCustomProvidersCache(userId.value, data)
    } catch (e) {
      state.error = e instanceof Error ? e.message : "Failed to load custom providers"
      // keep cache on failure
    } finally {
      state.loading = false
      state.refreshing = false
    }
  }

  function invalidateLocal() {
    state.data = null
    state.fromCache = false
  }

  return {
    state,
    setUserId,
    load,
    invalidateLocal,
    CACHE_TTL_MS,
  }
}
