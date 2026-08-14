import { computed, reactive, ref } from "vue"
import { listLogs } from "@/services/api"
import {
  CACHE_TTL_MS,
  isLogsCacheFresh,
  readLogsCache,
  writeLogsCache,
} from "@/services/cache"
import { getLogsPrefs, setLogsPrefs } from "@/services/prefs"
import type { RequestLogRow } from "@/types"

type LogsState = {
  rows: RequestLogRow[]
  /** Opaque keyset token for the next page; null = the list ends here. */
  nextCursor: string | null
  /** First paint with nothing to show yet — the page renders skeletons. */
  loading: boolean
  refreshing: boolean
  loadingMore: boolean
  error: string | null
  moreError: string | null
}

function emptyState(): LogsState {
  return {
    rows: [],
    nextCursor: null,
    loading: false,
    refreshing: false,
    loadingMore: false,
    error: null,
    moreError: null,
  }
}

// Module-level singleton, same pattern as useUsage's `byDays`: leaving the page
// and coming back repaints what was already loaded — including the pages the
// user pressed Load more for, which no cache holds.
const state = reactive<LogsState>(emptyState())

/** Filters, kept beside the rows they produced so a reload never mixes two views. */
const provider = ref<string | null>(getLogsPrefs().provider)
const errorsOnly = ref(false)

/** Server default; stated here because the cached page has to match the fetched one. */
const PAGE_SIZE = 50

export function useLogs() {
  const userId = ref<string | null>(null)

  function setUserId(id: string | null) {
    userId.value = id
  }

  /** Only the unfiltered view is cached — a filtered one is one of arbitrarily many. */
  const isUnfiltered = computed(() => provider.value === null && !errorsOnly.value)

  /**
   * Loads the first page.
   *
   * Cache-first for the unfiltered view: paint whatever is on disk, then go to
   * the network only when it is stale or the user asked. A filtered view always
   * fetches — there is nothing cached to paint, and a stale filter result would
   * be a different question's answer.
   */
  async function load(opts?: { refresh?: boolean }) {
    const force = !!opts?.refresh
    const cacheable = isUnfiltered.value

    if (cacheable) {
      const cached = readLogsCache(userId.value)
      if (cached) {
        applyPage(cached.rows, cached.next_cursor)
        state.loading = false
      }
      if (!force && cached && isLogsCacheFresh(userId.value, CACHE_TTL_MS)) {
        state.refreshing = false
        return
      }
    }

    if (!state.rows.length) state.loading = true
    state.refreshing = true
    state.error = null
    state.moreError = null
    try {
      const page = await listLogs({
        limit: PAGE_SIZE,
        provider: provider.value ?? undefined,
        errorsOnly: errorsOnly.value,
      })
      applyPage(page.rows, page.next_cursor)
      if (cacheable) writeLogsCache(userId.value, page)
    } catch (e) {
      state.error = e instanceof Error ? e.message : "Failed to load logs"
      // Keep whatever is painted; the page surfaces a non-blocking banner.
    } finally {
      state.loading = false
      state.refreshing = false
    }
  }

  /**
   * Appends the next page. Never cached: the cache holds the first page of the
   * unfiltered view, and writing a grown list into it would keep re-painting a
   * page the user has to scroll past to reach anything new.
   */
  async function loadMore() {
    const cursor = state.nextCursor
    if (!cursor || state.loadingMore) return
    state.loadingMore = true
    state.moreError = null
    try {
      const page = await listLogs({
        limit: PAGE_SIZE,
        cursor,
        provider: provider.value ?? undefined,
        errorsOnly: errorsOnly.value,
      })
      state.rows = [...state.rows, ...page.rows]
      state.nextCursor = page.next_cursor
    } catch (e) {
      state.moreError = e instanceof Error ? e.message : "Failed to load more logs"
    } finally {
      state.loadingMore = false
    }
  }

  /** Server-side filter, so a change reloads from the first page. */
  async function setProvider(next: string | null) {
    if (provider.value === next) return
    provider.value = next
    setLogsPrefs({ provider: next })
    resetRows()
    await load()
  }

  async function setErrorsOnly(next: boolean) {
    if (errorsOnly.value === next) return
    errorsOnly.value = next
    resetRows()
    await load()
  }

  /**
   * A filter change asks a different question, so the old answer goes rather
   * than staying on screen until the new page lands and replacing it row by
   * row — including the Load-more pages, which belong to the old filter.
   */
  function resetRows() {
    state.rows = []
    state.nextCursor = null
    state.error = null
    state.moreError = null
  }

  function applyPage(rows: RequestLogRow[], nextCursor: string | null) {
    state.rows = rows
    state.nextCursor = nextCursor
  }

  return {
    state,
    provider: computed(() => provider.value),
    errorsOnly: computed(() => errorsOnly.value),
    setUserId,
    load,
    loadMore,
    setProvider,
    setErrorsOnly,
    CACHE_TTL_MS,
  }
}
