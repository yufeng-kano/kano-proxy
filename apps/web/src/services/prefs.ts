/**
 * View preferences — what the *user picked*, persisted across tab closes.
 *
 * `localStorage`, like services/cache.ts — but a separate module on purpose.
 * That one holds server payloads: user-id-scoped, wrapped in versioned data
 * envelopes, swept on logout. This one holds only enum-ish UI choices and
 * integers — last route, scroll offsets, the Overview range / chart view /
 * table toggle, the Models provider filter — under a single unscoped key that
 * survives sign-out, so a reopened tab lands where the user left off (see
 * docs/admin-ui.md § View preferences).
 *
 * Nothing user-identifying goes in here: no tokens, no session state, no
 * email, no server response. That is why it needs neither the user-id scoping
 * nor the logout sweep the data caches get — there is nothing here to leak
 * between two people on one machine beyond a route name.
 *
 * Every read is validated against the current allowed values and falls back
 * to the default, so a stale schema, a removed route, or a hand-edited blob
 * degrades to "first visit" instead of crashing a page.
 */

import type { UsageDays } from "@/types"

const PREFS_KEY = "kano-proxy:prefs"

/** Which time-series view the Dashboard chart card is showing. */
export type ChartView = "tokens" | "cache-rate"

const CHART_VIEWS: ChartView[] = ["tokens", "cache-rate"]
const USAGE_DAYS: UsageDays[] = [1, 7, 30]

export type Prefs = {
  /** Router path to restore on next boot, e.g. "/overview". */
  lastPath: string | null
  /** Scroll offset of the shell's content region, in px, keyed by router path. */
  scroll: Record<string, number>
  overview: {
    days: UsageDays
    chartView: ChartView
    /** Chart card's chart-vs-table toggle. */
    showTable: boolean
  }
  models: {
    /** Provider group the Models page is filtered to; null = all. Free-form because a custom endpoint's slug is user-defined. */
    provider: string | null
  }
}

function defaults(): Prefs {
  return {
    lastPath: null,
    scroll: {},
    overview: { days: 7, chartView: "tokens", showTable: false },
    models: { provider: null },
  }
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v)
}

/** A finite, non-negative, sane scroll offset — anything else is dropped. */
function readScroll(raw: unknown): Record<string, number> {
  if (!isRecord(raw)) return {}
  const out: Record<string, number> = {}
  for (const [path, value] of Object.entries(raw)) {
    if (typeof path !== "string" || !path.startsWith("/")) continue
    if (typeof value !== "number" || !Number.isFinite(value) || value < 0) continue
    out[path] = Math.round(value)
  }
  return out
}

function parse(raw: string): Prefs {
  const base = defaults()
  const parsed: unknown = JSON.parse(raw)
  if (!isRecord(parsed)) return base

  const lastPath = parsed.lastPath
  if (typeof lastPath === "string" && lastPath.startsWith("/")) {
    base.lastPath = lastPath
  }
  base.scroll = readScroll(parsed.scroll)

  if (isRecord(parsed.overview)) {
    const { days, chartView, showTable } = parsed.overview
    if (USAGE_DAYS.includes(days as UsageDays)) base.overview.days = days as UsageDays
    if (CHART_VIEWS.includes(chartView as ChartView)) {
      base.overview.chartView = chartView as ChartView
    }
    if (typeof showTable === "boolean") base.overview.showTable = showTable
  }

  if (isRecord(parsed.models)) {
    const { provider } = parsed.models
    // A slug is user-defined, so there is no enum to check against — only a
    // shape and a length bound. A provider deleted since this was written
    // resolves to "all" at the page, not to an empty catalog.
    if (typeof provider === "string" && provider.length > 0 && provider.length <= 64) {
      base.models.provider = provider
    }
  }
  return base
}

export function readPrefs(): Prefs {
  if (typeof localStorage === "undefined") return defaults()
  try {
    const raw = localStorage.getItem(PREFS_KEY)
    if (!raw) return defaults()
    return parse(raw)
  } catch {
    // Malformed blob or private-mode read failure: discard, never trust.
    return defaults()
  }
}

function write(prefs: Prefs): void {
  if (typeof localStorage === "undefined") return
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify(prefs))
  } catch {
    /* quota / private mode — preferences are best-effort */
  }
}

/** Read-modify-write so two independent callers (router, page) never clobber each other's slice. */
export function patchPrefs(patch: (current: Prefs) => Prefs): void {
  write(patch(readPrefs()))
}

export function setLastPath(path: string): void {
  patchPrefs((p) => ({ ...p, lastPath: path }))
}

/**
 * Remembers at most `MAX_SCROLL_ENTRIES` paths — the app has a handful of
 * routes, so an unbounded map here would only ever grow from junk paths.
 */
const MAX_SCROLL_ENTRIES = 12

export function setScroll(path: string, offset: number): void {
  patchPrefs((p) => {
    const scroll = { ...p.scroll, [path]: Math.max(0, Math.round(offset)) }
    const keys = Object.keys(scroll)
    if (keys.length > MAX_SCROLL_ENTRIES) {
      for (const k of keys.slice(0, keys.length - MAX_SCROLL_ENTRIES)) delete scroll[k]
    }
    return { ...p, scroll }
  })
}

export function getScroll(path: string): number {
  return readPrefs().scroll[path] ?? 0
}

export function setOverviewPrefs(patch: Partial<Prefs["overview"]>): void {
  patchPrefs((p) => ({ ...p, overview: { ...p.overview, ...patch } }))
}

export function getOverviewPrefs(): Prefs["overview"] {
  return readPrefs().overview
}

export function setModelsPrefs(patch: Partial<Prefs["models"]>): void {
  patchPrefs((p) => ({ ...p, models: { ...p.models, ...patch } }))
}

export function getModelsPrefs(): Prefs["models"] {
  return readPrefs().models
}

/**
 * Clears persisted view state on sign-out. The impersonal view choices
 * (Overview range / chart view, Models filter) survive — but the last route
 * and its scroll offsets say where *that* user was, so they go.
 */
export function clearNavigationPrefs(): void {
  patchPrefs((p) => ({ ...p, lastPath: null, scroll: {} }))
}
