import { computed, reactive, ref } from "vue"
import { getUsageSummary } from "@/services/api"
import {
  CACHE_TTL_MS,
  isUsageSummaryCacheFresh,
  readUsageSummaryCache,
  writeUsageSummaryCache,
} from "@/services/cache"
import { getOverviewPrefs, setOverviewPrefs } from "@/services/prefs"
import type { UsageDays, UsageSummary } from "@/types"

type UsageState = {
  data: UsageSummary | null
  loading: boolean
  refreshing: boolean
  error: string | null
  fromCache: boolean
}

function emptyState(): UsageState {
  return {
    data: null,
    loading: false,
    refreshing: false,
    error: null,
    fromCache: false,
  }
}

// Module-level singleton, same pattern as useAccounts.ts's `byProvider`: one
// entry per range so switching 24h/7d/30d repaints that range's own cache
// immediately instead of flashing back to a shared loading state.
const byDays = reactive<Record<UsageDays, UsageState>>({
  1: emptyState(),
  7: emptyState(),
  30: emptyState(),
})

export function useUsage() {
  const userId = ref<string | null>(null)
  // Seeded from the persisted view preference so a reopened tab reloads the
  // range the user last picked, not the 7d default.
  const days = ref<UsageDays>(getOverviewPrefs().days)

  function setUserId(id: string | null) {
    userId.value = id
  }

  /**
   * Cache-first load for one `days` range.
   * - Always paint localStorage cache immediately when present.
   * - Network fetch only if cache missing/stale (>2 min) or opts.refresh.
   * - No backend KV to bypass here (D1 read, no cache layer — see
   *   docs/admin-ui.md Dashboard page) so `refresh` only skips the
   *   frontend cache, unlike accounts/models/custom-providers.
   */
  async function loadDays(target: UsageDays, opts?: { refresh?: boolean }) {
    const state = byDays[target]
    const force = !!opts?.refresh

    const cached = readUsageSummaryCache(userId.value, target)
    if (cached) {
      state.data = cached
      state.fromCache = true
      state.loading = false
    } else if (!state.data) {
      state.loading = true
    }

    // Within TTL: keep cache, skip network unless forced
    if (!force && isUsageSummaryCacheFresh(userId.value, target, CACHE_TTL_MS)) {
      state.loading = false
      state.refreshing = false
      return
    }

    state.refreshing = true
    state.error = null
    try {
      const data = await getUsageSummary(target)
      state.data = data
      state.fromCache = false
      writeUsageSummaryCache(userId.value, target, data)
    } catch (e) {
      state.error = e instanceof Error ? e.message : "Failed to load usage summary"
      // keep cache on failure
    } finally {
      state.loading = false
      state.refreshing = false
    }
  }

  /** Load (or refresh) the currently selected range. */
  async function refresh(opts?: { refresh?: boolean }) {
    await loadDays(days.value, opts)
  }

  /** Switch range and immediately paint that range's own cache; background-refreshes if stale. */
  function setDays(next: UsageDays) {
    if (days.value === next) return
    days.value = next
    setOverviewPrefs({ days: next })
    void loadDays(next)
  }

  const summary = computed(() => byDays[days.value].data)
  const loading = computed(() => byDays[days.value].loading)
  const refreshing = computed(() => byDays[days.value].refreshing)
  const error = computed(() => byDays[days.value].error)
  const fromCache = computed(() => byDays[days.value].fromCache)

  return {
    summary,
    loading,
    refreshing,
    error,
    fromCache,
    days,
    setDays,
    setUserId,
    refresh,
    loadDays,
    CACHE_TTL_MS,
  }
}
