import { computed, reactive, ref } from "vue"
import { getChangelog } from "@/services/api"
import {
  CHANGELOG_CACHE_TTL_MS,
  isChangelogCacheFresh,
  readChangelogCache,
  writeChangelogCache,
} from "@/services/cache"
import type { ChangelogResponse } from "@/types"

type ChangelogState = {
  data: ChangelogResponse | null
  loading: boolean
  refreshing: boolean
  error: string | null
  fromCache: boolean
}

function emptyState(): ChangelogState {
  return {
    data: null,
    loading: false,
    refreshing: false,
    error: null,
    fromCache: false,
  }
}

// Module-level singleton, same pattern as useUsage.ts / useCustomProviders.ts:
// the topbar badge and the Changelog page read the same state, so opening the
// page from the badge repaints what is already loaded.
const state = reactive<ChangelogState>(emptyState())

/**
 * The one request currently on the wire, shared by every caller.
 *
 * Unlike the other composables this one has two mounted consumers at once:
 * the topbar badge lives in App.vue on every signed-in page, and the page
 * loads too. Without this they would fire two identical requests on the same
 * tick — a wasted round trip against a 60/hr upstream budget.
 */
let inFlight: Promise<void> | null = null

async function fetchInto(force: boolean): Promise<void> {
  state.refreshing = true
  state.error = null
  try {
    const data = await getChangelog({ refresh: force })
    state.data = data
    state.fromCache = false
    writeChangelogCache(data)
  } catch (e) {
    state.error = e instanceof Error ? e.message : "Failed to load changelog"
    // keep cache on failure
  } finally {
    state.loading = false
    state.refreshing = false
  }
}

export function useChangelog() {
  const userId = ref<string | null>(null)

  /**
   * Parity with the other composables' call sites, but note the cache key is
   * **not** user-scoped (see services/cache.ts) — the payload is public
   * release notes. The id is only used to tell signed-in from signed-out.
   */
  function setUserId(id: string | null) {
    userId.value = id
  }

  /**
   * Cache-first load.
   * - Always paint sessionStorage cache immediately when present.
   * - Network fetch only if cache missing/stale (>1h) or opts.refresh.
   * - `opts.refresh` also bypasses the server's own 1h freshness window.
   */
  async function load(opts?: { refresh?: boolean }) {
    const force = !!opts?.refresh

    const cached = readChangelogCache()
    if (cached) {
      state.data = cached
      state.fromCache = true
      state.loading = false
    } else if (!state.data) {
      state.loading = true
    }

    // Signed out: /api/changelog 401s like every other /api route, so there is
    // nothing worth asking for. Any cache still paints.
    if (!userId.value) {
      state.loading = false
      return
    }

    // Within TTL: keep cache, skip network unless forced. An hour here, not
    // the 90s the other domains use — release notes change on deploy.
    if (!force && isChangelogCacheFresh(CHANGELOG_CACHE_TTL_MS)) {
      state.loading = false
      state.refreshing = false
      return
    }

    if (inFlight) {
      await inFlight
      // A background load folds into whatever was already running. A user's
      // Refresh must actually reach the server, so it waits and then goes.
      if (!force) return
    }

    const pending = fetchInto(force).finally(() => {
      if (inFlight === pending) inFlight = null
    })
    inFlight = pending
    await pending
  }

  /** Manual Refresh — force the network and bypass the server-side cache. */
  async function refresh() {
    await load({ refresh: true })
  }

  const data = computed(() => state.data)
  const loading = computed(() => state.loading)
  const refreshing = computed(() => state.refreshing)
  const error = computed(() => state.error)
  const fromCache = computed(() => state.fromCache)

  return {
    data,
    loading,
    refreshing,
    error,
    fromCache,
    setUserId,
    load,
    refresh,
    CHANGELOG_CACHE_TTL_MS,
  }
}
