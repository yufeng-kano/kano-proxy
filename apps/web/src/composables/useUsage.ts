import { computed, onBeforeUnmount, onMounted, reactive, ref } from "vue"
import { buildUsageRange, parseAnchor } from "@/components/overview/dateRange"
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
  const initialKind: UsageRangeKind =
    initialPrefs.rangeKind ||
    (initialPrefs.days === 1 ? "day" : initialPrefs.days === 30 ? "month" : "week")
  const rangeKind = ref<UsageRangeKind>(initialKind)
  // A pinned past period survives a reload; anything else follows today
  // (docs/admin-ui.md § Overview page, Following today). An unparseable or
  // future anchor falls back to today.
  const pinnedDate = initialPrefs.pinnedAnchor
    ? parseAnchor(initialKind, initialPrefs.pinnedAnchor)
    : null
  const activeDate = ref<Date>(pinnedDate ?? new Date())
  /** Whether the selection covers today, so it rolls over with the clock. */
  const followsToday = ref(pinnedDate === null)

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
        offsetMinutes: targetRange.offsetMinutes,
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

  /**
   * Moves a selection that follows today onto today's period. Returns whether
   * the range changed, i.e. a day / week / month boundary passed since it was
   * last set.
   */
  function rollToToday(): boolean {
    if (!followsToday.value) return false
    const before = rangeKey.value
    activeDate.value = new Date()
    return rangeKey.value !== before
  }

  /** Load or refresh the currently active range, rolling it to today first when it follows today. */
  async function refresh(opts?: { refresh?: boolean }) {
    rollToToday()
    await loadRange(range.value, opts)
  }

  /**
   * Persists the whole selection — granularity *and* whether it follows today
   * or is pinned to a past period — from the current range, so the two can
   * never drift apart in storage. `days` rides along as the legacy mirror of
   * `kind`.
   */
  function persistRange() {
    const kind = range.value.kind
    followsToday.value = range.value.anchor === buildUsageRange(kind, new Date()).anchor
    const mappedDays: UsageDays = kind === "day" ? 1 : kind === "month" ? 30 : 7
    setOverviewPrefs({
      rangeKind: kind,
      days: mappedDays,
      pinnedAnchor: followsToday.value ? null : range.value.anchor,
    })
  }

  /** Change the range granularity (day / week / month), optionally setting a date. */
  function setRangeKind(kind: UsageRangeKind, date?: Date) {
    rangeKind.value = kind
    if (date) activeDate.value = date
    else rollToToday()
    persistRange()
    void loadRange(range.value)
  }

  /** Change the active anchor date (e.g. from calendar picker). */
  function setDate(date: Date) {
    activeDate.value = date
    persistRange()
    void loadRange(range.value)
  }

  // Roll over while the page is open: a timer at the next local midnight
  // (week and month boundaries are midnights too), plus a re-check when the
  // tab becomes visible, since background tabs throttle or freeze timers.
  let midnightTimer: ReturnType<typeof setTimeout> | undefined

  function onBoundary() {
    if (rollToToday()) void loadRange(range.value)
    scheduleMidnight()
  }

  function scheduleMidnight() {
    clearTimeout(midnightTimer)
    const now = new Date()
    const next = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1)
    // One second past midnight so the new Date() inside lands on the new day.
    midnightTimer = setTimeout(onBoundary, next.getTime() - now.getTime() + 1000)
  }

  function onVisibilityChange() {
    if (document.visibilityState === "visible") onBoundary()
  }

  onMounted(() => {
    scheduleMidnight()
    document.addEventListener("visibilitychange", onVisibilityChange)
  })
  onBeforeUnmount(() => {
    clearTimeout(midnightTimer)
    document.removeEventListener("visibilitychange", onVisibilityChange)
  })

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
    setRangeKind,
    setDate,
    setUserId,
    refresh,
    loadRange,
    CACHE_TTL_MS,
  }
}
