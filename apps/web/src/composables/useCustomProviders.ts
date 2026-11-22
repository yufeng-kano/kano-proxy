import { reactive, ref } from "vue"
import { listCustomProviders, reorderCustomProviders } from "@/services/api"
import {
  isCustomProvidersCacheFresh,
  readCustomProvidersCache,
  writeCustomProvidersCache,
} from "@/services/cache"
import type { CustomProvider, RequestEdits, SharedCustomProvider } from "@/types"

type CustomProvidersState = {
  data: CustomProvider[] | null
  /**
   * Endpoints other users share with the viewer (cloud editions only). Loaded,
   * cached and cleared together with `data`; empty on a standalone install.
   */
  shared: SharedCustomProvider[] | null
  loading: boolean
  error: string | null
}

function emptyState(): CustomProvidersState {
  return {
    data: null,
    shared: null,
    loading: false,
    error: null,
  }
}

// Module-level singleton, same pattern as useAccounts.ts's `byProvider`: state
// is shared across every component that calls this composable, so navigating
// Providers -> Models reuses what's already loaded instead of re-fetching.
const state = reactive<CustomProvidersState>(emptyState())

/**
 * The shared endpoints a request can actually reach. A shadowed one shares its
 * slug with one of the viewer's own providers, which wins every request, so
 * only the Providers page lists it (as Unavailable); every other page treats
 * the slug as the viewer's own.
 */
export function reachableShared(shared: SharedCustomProvider[] | null): SharedCustomProvider[] {
  return (shared ?? []).filter((s) => !s.shadowed)
}

/**
 * The Custom tab's badge: the viewer's own endpoints plus the shared ones they
 * can reach — a shadowed row only reads Unavailable. `null` until the list has
 * loaded, so the chip never states a count the page is not sure of.
 */
export function customEndpointCount(
  own: CustomProvider[] | null,
  shared: SharedCustomProvider[] | null,
): number | null {
  if (own == null) return null
  return own.length + reachableShared(shared).length
}

export function useCustomProviders() {
  const userId = ref<string | null>(null)

  function setUserId(id: string | null) {
    userId.value = id
  }

  /**
   * Cache-first load.
   * - Always paint localStorage cache immediately when present.
   * - Network fetch only if cache missing/stale (>2 min) or opts.refresh.
   */
  async function load(opts?: { refresh?: boolean }) {
    const force = !!opts?.refresh

    const cached = readCustomProvidersCache(userId.value)
    if (cached) {
      state.data = cached.providers
      state.shared = cached.shared
      state.loading = false
    } else if (!state.data) {
      state.loading = true
    }

    // Within TTL: keep cache, skip network unless forced
    if (!force && isCustomProvidersCacheFresh(userId.value)) {
      state.loading = false
      return
    }

    state.error = null
    try {
      const data = await listCustomProviders()
      state.data = data.providers
      state.shared = data.shared
      writeCustomProvidersCache(userId.value, data)
    } catch (e) {
      state.error = e instanceof Error ? e.message : "Failed to load custom providers"
      // keep cache on failure
    } finally {
      state.loading = false
    }
  }

  /**
   * Writes a new display order. Optimistic: the list is repainted in `ids`
   * order first so a keypress or a drop lands immediately, then the server's
   * own returned array is adopted as truth. A failure restores the array the
   * user was looking at and surfaces the error — no half-applied order.
   */
  async function reorder(ids: string[]): Promise<boolean> {
    const previous = state.data
    if (!previous) return false

    const byId = new Map(previous.map((p) => [p.id, p]))
    const optimistic = ids.map((id) => byId.get(id)).filter((p): p is CustomProvider => !!p)
    if (optimistic.length !== previous.length) return false

    state.data = optimistic
    state.error = null
    try {
      const data = await reorderCustomProviders(ids)
      state.data = data
      // The order write answers with the own list only; the shared rows are
      // display-ordered by the edition and unchanged by it.
      writeCustomProvidersCache(userId.value, { providers: data, shared: state.shared ?? [] })
      return true
    } catch (e) {
      state.data = previous
      state.error = e instanceof Error ? e.message : "Failed to reorder custom providers"
      return false
    }
  }

  /**
   * Records the switches a write just saved on that endpoint's row, and
   * re-caches the list: the write's response is the stored truth, and a
   * re-fetch whose failure kept the old cache would paint the switch back.
   */
  function setEdits(id: string, edits: RequestEdits) {
    const row = state.data?.find((p) => p.id === id)
    if (!row || !state.data) return
    row.strip_fast_mode = edits.strip_fast_mode
    row.strip_claude_code_line = edits.strip_claude_code_line
    row.force_cache_ttl_1h = edits.force_cache_ttl_1h
    writeCustomProvidersCache(userId.value, { providers: state.data, shared: state.shared ?? [] })
  }

  return {
    state,
    setUserId,
    load,
    reorder,
    setEdits,
  }
}
