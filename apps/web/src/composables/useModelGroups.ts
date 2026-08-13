import { reactive, ref } from "vue"
import { listModelGroups } from "@/services/api"
import {
  CACHE_TTL_MS,
  isModelGroupsCacheFresh,
  readModelGroupsCache,
  writeModelGroupsCache,
} from "@/services/cache"
import type { ModelGroup } from "@/types"

type ModelGroupsState = {
  data: ModelGroup[] | null
  loading: boolean
  refreshing: boolean
  error: string | null
  fromCache: boolean
}

function emptyState(): ModelGroupsState {
  return {
    data: null,
    loading: false,
    refreshing: false,
    error: null,
    fromCache: false,
  }
}

// Module-level singleton, same pattern as useCustomProviders.ts: the state is
// shared across every component that calls this composable, so a page that has
// already loaded the list does not re-fetch it.
const state = reactive<ModelGroupsState>(emptyState())

export function useModelGroups() {
  const userId = ref<string | null>(null)

  function setUserId(id: string | null) {
    userId.value = id
  }

  /**
   * Cache-first load.
   * - Always paint the localStorage cache immediately when present.
   * - Network fetch only if the cache is missing/stale (>90s) or opts.refresh.
   *
   * Mutations pass `refresh: true` rather than patching the list locally: the
   * server owns `updated_at` and the name uniqueness check, so re-reading is
   * what keeps the table honest (same call as the custom endpoints page).
   */
  async function load(opts?: { refresh?: boolean }) {
    const force = !!opts?.refresh

    const cached = readModelGroupsCache(userId.value)
    if (cached) {
      state.data = cached
      state.fromCache = true
      state.loading = false
    } else if (!state.data) {
      state.loading = true
    }

    // Within TTL: keep the cache, skip the network unless forced.
    if (!force && isModelGroupsCacheFresh(userId.value, CACHE_TTL_MS)) {
      state.loading = false
      state.refreshing = false
      return
    }

    state.refreshing = true
    state.error = null
    try {
      const data = await listModelGroups()
      state.data = data
      state.fromCache = false
      writeModelGroupsCache(userId.value, data)
    } catch (e) {
      state.error = e instanceof Error ? e.message : "Failed to load model groups"
      // keep cache on failure
    } finally {
      state.loading = false
      state.refreshing = false
    }
  }

  return {
    state,
    setUserId,
    load,
    CACHE_TTL_MS,
  }
}
