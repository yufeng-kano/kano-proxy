/**
 * View preferences — what the *user picked*, persisted across tab closes.
 *
 * `localStorage`, like services/cache.ts — but a separate module on purpose.
 * That one holds server payloads: user-id-scoped, wrapped in versioned data
 * envelopes, swept on logout. This one holds only enum-ish UI choices and
 * integers — last route, scroll offsets, the Overview range / chart view /
 * table toggle, the Models and Logs provider filters — under a single unscoped
 * key that survives sign-out, so a reopened tab lands where the user left off
 * (see docs/admin-ui.md § View preferences).
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

import type { UsageDays, UsageRangeKind } from "@/types"

const PREFS_KEY = "kano-proxy:prefs"

/** Which sub-tab the Overview activity card is showing. */
export type ChartView = "tokens" | "requests" | "cache" | "models"

const CHART_VIEWS: ChartView[] = ["tokens", "requests", "cache", "models"]
const USAGE_DAYS: UsageDays[] = [1, 7, 30]
const USAGE_RANGE_KINDS: UsageRangeKind[] = ["day", "week", "month"]
/** "YYYY-MM-DD" (Day/Week) or "YYYY-MM" (Month) — shape only; the range module owns the calendar validation. */
const ANCHOR_RE = /^\d{4}-\d{2}(-\d{2})?$/

export type Prefs = {
  /** Router path to restore on next boot, e.g. "/overview". */
  lastPath: string | null
  /** Scroll offset of the shell's content region, in px, keyed by router path. */
  scroll: Record<string, number>
  overview: {
    days: UsageDays
    rangeKind: UsageRangeKind
    /**
     * Which day / week / month the picker is on: "YYYY-MM-DD" for Day and
     * Week (the week's Monday), "YYYY-MM" for Month. null = follow today.
     */
    anchor: string | null
    chartView: ChartView
  }
  models: {
    /** Provider group the Models page is filtered to; null = all. Free-form because a custom endpoint's slug is user-defined. */
    provider: string | null
  }
  providers: {
    /** Provider tab the Providers page is on; null = all. Free-form like models.provider. */
    tab: string | null
  }
  logs: {
    /** Provider the Logs page is filtered to; null = all. Free-form like models.provider. */
    provider: string | null
  }
}

function defaults(): Prefs {
  return {
    lastPath: null,
    scroll: {},
    // `days` and `rangeKind` are two views of one choice — keep them in step.
    overview: { days: 7, rangeKind: "week", anchor: null, chartView: "tokens" },
    models: { provider: null },
    providers: { tab: null },
    logs: { provider: null },
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
    const { days, rangeKind, anchor, chartView } = parsed.overview
    if (USAGE_DAYS.includes(days as UsageDays)) base.overview.days = days as UsageDays
    // The pre-2.1 "cache-rate" value (and the removed showTable flag) simply
    // fail these checks and fall back — exactly the degradation this parser
    // promises for a stale schema. A pre-Day/Week/Month blob has no rangeKind
    // at all, so its `days` names the granularity it meant.
    if (USAGE_RANGE_KINDS.includes(rangeKind as UsageRangeKind)) {
      base.overview.rangeKind = rangeKind as UsageRangeKind
    } else if (days === 1) {
      base.overview.rangeKind = "day"
    } else if (days === 7) {
      base.overview.rangeKind = "week"
    } else if (days === 30) {
      base.overview.rangeKind = "month"
    }
    if (typeof anchor === "string" && ANCHOR_RE.test(anchor)) {
      base.overview.anchor = anchor
    }
    if (CHART_VIEWS.includes(chartView as ChartView)) {
      base.overview.chartView = chartView as ChartView
    }
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

  if (isRecord(parsed.providers)) {
    const { tab } = parsed.providers
    if (typeof tab === "string" && tab.length > 0 && tab.length <= 64) {
      base.providers.tab = tab
    }
  }

  if (isRecord(parsed.logs)) {
    const { provider } = parsed.logs
    // Same shape-and-length check as models.provider: a slug is user-defined,
    // and one deleted since this was written resolves to "all" at the page.
    if (typeof provider === "string" && provider.length > 0 && provider.length <= 64) {
      base.logs.provider = provider
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

export function setProvidersPrefs(patch: Partial<Prefs["providers"]>): void {
  patchPrefs((p) => ({ ...p, providers: { ...p.providers, ...patch } }))
}

export function getProvidersPrefs(): Prefs["providers"] {
  return readPrefs().providers
}

export function setLogsPrefs(patch: Partial<Prefs["logs"]>): void {
  patchPrefs((p) => ({ ...p, logs: { ...p.logs, ...patch } }))
}

export function getLogsPrefs(): Prefs["logs"] {
  return readPrefs().logs
}

/**
 * Clears persisted view state on sign-out. The impersonal view choices
 * (Overview range / chart view, Models filter) survive — but the last route
 * and its scroll offsets say where *that* user was, so they go.
 */
export function clearNavigationPrefs(): void {
  patchPrefs((p) => ({ ...p, lastPath: null, scroll: {} }))
}
