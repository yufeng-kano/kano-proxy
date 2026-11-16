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
  const range = computed<UsageRange>(() => buildUsageRange(rangeKind.value, activeDate.value))
  const rangeKey = computed(() => `${range.value.kind}:${range.value.anchor}`)

  function isCurrentPeriod(): boolean {
    return range.value.anchor === buildUsageRange(rangeKind.value, new Date()).anchor
  }

  /**
   * Whether the selection covers today, so it rolls over with the clock. The
   * same test `persistRange` applies, so a stored pin that happens to cover
   * today (a clock or timezone change) follows too.
   */
  const followsToday = ref(isCurrentPeriod())

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
    // Assign only on a real change: a fresh Date on every check would re-fire
    // the selected-date watchers (the open calendar snaps back to its month)
    // and move a picked weekday inside a week that has not ended.
    if (!followsToday.value || isCurrentPeriod()) return false
    activeDate.value = new Date()
    return true
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
    followsToday.value = isCurrentPeriod()
    const mappedDays: UsageDays = kind === "day" ? 1 : kind === "month" ? 30 : 7
    setOverviewPrefs({
      rangeKind: kind,
      days: mappedDays,
      pinnedAnchor: followsToday.value ? null : range.value.anchor,
    })
  }

  /** Change the range granularity (day / week / month), optionally setting a date. */
  function setRangeKind(kind: UsageRangeKind, date?: Date) {
    // Catch up a followed selection the clock has passed before switching, in
    // the granularity it was chosen in; a pinned day stays put, so Day -> Week
    // -> Day returns to the same day.
    rollToToday()
    rangeKind.value = kind
    if (date) activeDate.value = date
    persistRange()
    void loadRange(range.value)
  }

  /** Change the active anchor date (e.g. from calendar picker). */
  function setDate(date: Date) {
    activeDate.value = date
    persistRange()
    void loadRange(range.value)
  }

  // Roll over while the page is open. A once-a-minute check rather than a
  // timer aimed at midnight: a timeout's clock can stall while the machine
  // sleeps, so a laptop woken at 8 AM with the tab in front would sit on
  // yesterday for hours. The check is one Date and a string compare. Tab
  // focus re-checks at once, since background tabs throttle timers.
  function onTick() {
    if (rollToToday()) void loadRange(range.value)
  }

  function onVisibilityChange() {
    if (document.visibilityState === "visible") onTick()
  }

  let ticker: ReturnType<typeof setInterval> | undefined
  onMounted(() => {
    ticker = setInterval(onTick, 60_000)
    document.addEventListener("visibilitychange", onVisibilityChange)
  })
  onBeforeUnmount(() => {
    clearInterval(ticker)
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
