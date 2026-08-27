import { computed, reactive, ref } from "vue"
import { buildUsageRange } from "@/components/overview/dateRange"
import { getUsageSummary } from "@/services/api"
import {
  CACHE_TTL_MS,
  isUsageSummaryCacheFresh,
  readUsageSummaryCache,
  writeUsageSummaryCache,
} from "@/services/cache"
import { getOverviewPrefs, setOverviewPrefs } from "@/services/prefs"
import type { UsageDays, UsageRange, UsageRangeKind, UsageSummary } from "@/types"

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

// Module-level singleton: one state entry per rangeKey so switching ranges
// repaints that range's cached data immediately instead of flashing a blank state.
const byKey = reactive<Record<string, UsageState>>({})

function getOrInitState(key: string): UsageState {
  if (!byKey[key]) {
    byKey[key] = emptyState()
  }
  return byKey[key]!
}

export function useUsage() {
  const userId = ref<string | null>(null)

  const initialPrefs = getOverviewPrefs()
  const rangeKind = ref<UsageRangeKind>(initialPrefs.rangeKind || (initialPrefs.days === 1 ? "day" : initialPrefs.days === 30 ? "month" : "week"))
  const activeDate = ref<Date>(new Date())

  const range = computed<UsageRange>(() => buildUsageRange(rangeKind.value, activeDate.value))
  const rangeKey = computed(() => `${range.value.kind}:${range.value.anchor}`)

  function setUserId(id: string | null) {
    userId.value = id
  }

  /**
   * Cache-first load for a given range.
   * - Always paint localStorage cache immediately when present.
   * - Network fetch only if cache missing/stale (>2 min) or opts.refresh.
   */
  async function loadRange(targetRange: UsageRange, opts?: { refresh?: boolean }) {
    const key = `${targetRange.kind}:${targetRange.anchor}`
    const state = getOrInitState(key)
    const force = !!opts?.refresh

    const cached = readUsageSummaryCache(userId.value, key)
    if (cached) {
      state.data = cached
      state.fromCache = true
      state.loading = false
    } else if (!state.data) {
      state.loading = true
    }

    if (!force && isUsageSummaryCacheFresh(userId.value, key, CACHE_TTL_MS)) {
      state.loading = false
      state.refreshing = false
      return
    }

    state.refreshing = true
    state.error = null
    try {
      const data = await getUsageSummary({
        from: targetRange.from,
        to: targetRange.to,
        grain: targetRange.grain,
      })
      state.data = data
      state.fromCache = false
      writeUsageSummaryCache(userId.value, key, data)
    } catch (e) {
      state.error = e instanceof Error ? e.message : "Failed to load usage summary"
    } finally {
      state.loading = false
      state.refreshing = false
    }
  }

  /** Load or refresh the currently active range. */
  async function refresh(opts?: { refresh?: boolean }) {
    await loadRange(range.value, opts)
  }

  /** Change the range granularity (day / week / month), optionally setting a date. */
  function setRangeKind(kind: UsageRangeKind, date?: Date) {
    rangeKind.value = kind
    if (date) activeDate.value = date
    const mappedDays: UsageDays = kind === "day" ? 1 : kind === "month" ? 30 : 7
    setOverviewPrefs({ rangeKind: kind, days: mappedDays })
    void loadRange(range.value)
  }

  /** Change the active anchor date (e.g. from calendar picker). */
  function setDate(date: Date) {
    activeDate.value = date
    void loadRange(range.value)
  }

  /** Backward-compatible adapter for days (1 -> day, 7 -> week, 30 -> month). */
  const days = computed<UsageDays>(() => (rangeKind.value === "day" ? 1 : rangeKind.value === "month" ? 30 : 7))

  function setDays(next: UsageDays) {
    const kind: UsageRangeKind = next === 1 ? "day" : next === 30 ? "month" : "week"
    setRangeKind(kind)
  }

  const currentState = computed(() => getOrInitState(rangeKey.value))
  const summary = computed(() => currentState.value.data)
  const loading = computed(() => currentState.value.loading)
  const refreshing = computed(() => currentState.value.refreshing)
  const error = computed(() => currentState.value.error)
  const fromCache = computed(() => currentState.value.fromCache)

  return {
    summary,
    loading,
    refreshing,
    error,
    fromCache,
    rangeKind,
    activeDate,
    range,
    rangeKey,
    days,
    setRangeKind,
    setDate,
    setDays,
    setUserId,
    refresh,
    loadRange,
    CACHE_TTL_MS,
  }
}
