/**
 * The Logs page's pure view logic (docs/admin-ui.md § Logs page): its columns,
 * the picker options built from `GET /api/logs/facets`, the dot colors that tie
 * a picker option to its rows, and the local-calendar ranges the page asks for.
 *
 * Kept out of the page so it is tested without mounting anything.
 */
import type { LogFacets, LogFacetTally, LogModelFilter, RequestLogRow } from "@/types"

export const LOG_COLUMN_KEYS = [
  "time",
  "model",
  "account",
  "apiKey",
  "status",
  "input",
  "cacheRead",
  "cacheWrite",
  "output",
  "cost",
  "latency",
] as const

export type LogColumnKey = (typeof LOG_COLUMN_KEYS)[number]

/** Always shown: a row without its time or its model says nothing. */
export const LOCKED_COLUMNS: readonly LogColumnKey[] = ["time", "model"]

/**
 * Default tracks in px. Model has none: it takes whatever the card has beyond
 * the others, down to `MODEL_MIN_WIDTH`, until the user drags it. Together they
 * fit a ~940px card (a 1280px window with the sidebar open).
 */
export const DEFAULT_WIDTHS: Record<Exclude<LogColumnKey, "model">, number> = {
  time: 80,
  account: 124,
  apiKey: 76,
  status: 60,
  input: 76,
  cacheRead: 76,
  cacheWrite: 76,
  output: 64,
  cost: 76,
  latency: 64,
}
export const MODEL_MIN_WIDTH = 150
export const MIN_COLUMN_WIDTH = 48
export const MAX_COLUMN_WIDTH = 800

/** The export fields a table column stands for, for "columns shown in the table". */
export const EXPORT_FIELDS_BY_COLUMN: Record<LogColumnKey, string[]> = {
  time: ["time"],
  model: ["provider", "model", "group"],
  account: ["account"],
  apiKey: ["api_key"],
  status: ["status"],
  input: ["input_tokens"],
  cacheRead: ["cache_read_tokens"],
  cacheWrite: ["cache_write_tokens"],
  output: ["output_tokens"],
  cost: ["cost"],
  latency: ["latency_ms"],
}

export type PickerView = "model" | "alias"

/** A row failed if it answered 4xx/5xx or carries an error code — an eager stream logs 200 with the code alone. */
export function isFailure(row: RequestLogRow): boolean {
  return row.error_code !== null || row.status_code >= 400
}

/** `claude-code/claude-opus-5` under provider `claude-code` → `claude-opus-5`; anything else unchanged. */
export function modelName(model: string, provider: string): string {
  return model.startsWith(`${provider}/`) ? model.slice(provider.length + 1) : model
}

/** One picker option. `key` is stable across loads and is what colors hang off. */
export type PickerOption = {
  key: string
  filter: LogModelFilter
  label: string
  /** The provider (By model) or the alias's model(s) (By alias). */
  sub: string | null
  tally: LogFacetTally | null
}

export function filterKey(filter: LogModelFilter): string {
  if (!filter) return "all"
  if (filter.kind === "direct") return "direct"
  return `${filter.kind}:${filter.value}`
}

/** The option key a row belongs to in the given view. */
export function rowOptionKey(row: RequestLogRow, view: PickerView): string {
  if (view === "model") return `model:${row.model}`
  return row.group_name ? `group:${row.group_name}` : "direct"
}

/**
 * The picker's options for one view, most-used first, narrowed to `provider`
 * when one is set. A filter that is still active but has no option (nothing
 * today, or another provider) is kept as a bare option, so the button can
 * always name what the table is filtered to.
 */
export function pickerOptions(
  facets: LogFacets | null,
  view: PickerView,
  provider: string | null,
  active: LogModelFilter,
  providerName: (slug: string) => string,
  noAliasLabel: string,
): PickerOption[] {
  const options: PickerOption[] = []
  if (view === "model") {
    for (const m of facets?.models ?? []) {
      if (provider && m.provider !== provider) continue
      options.push({
        key: `model:${m.model}`,
        filter: { kind: "model", value: m.model },
        label: modelName(m.model, m.provider),
        sub: providerName(m.provider),
        tally: { requests: m.requests, cost: m.cost },
      })
    }
  } else {
    for (const g of facets?.groups ?? []) {
      if (provider && !g.models.some((m) => m.startsWith(`${provider}/`))) continue
      options.push({
        key: `group:${g.group_name}`,
        filter: { kind: "group", value: g.group_name },
        label: g.group_name,
        sub: g.models.join(", "),
        tally: { requests: g.requests, cost: g.cost },
      })
    }
    // The no-alias count spans every provider, so it is only shown unnarrowed.
    options.push({
      key: "direct",
      filter: { kind: "direct" },
      label: noAliasLabel,
      sub: null,
      tally: provider || !facets ? null : facets.direct,
    })
  }
  const activeKey = filterKey(active)
  if (active && active.kind !== "direct" && !options.some((o) => o.key === activeKey)) {
    const belongs = (view === "model") === (active.kind === "model")
    if (belongs) {
      options.push({ key: activeKey, filter: active, label: active.value, sub: null, tally: null })
    }
  }
  return options
}

/**
 * Dot colors by option order: the six series hues, then the neutral one. The
 * no-alias option is neutral too — it is the absence of a choice, not one.
 */
export function optionColors(options: PickerOption[]): Map<string, string> {
  const colors = new Map<string, string>()
  let hue = 0
  for (const option of options) {
    if (option.key === "direct" || !option.tally || hue >= 6) {
      colors.set(option.key, "var(--series-other)")
      continue
    }
    hue += 1
    colors.set(option.key, `var(--series-${hue})`)
  }
  return colors
}

/** The local calendar day a timestamp falls on, as "YYYY-MM-DD". */
export function localDay(value: string | Date): string {
  const d = new Date(value)
  const pad = (n: number) => String(n).padStart(2, "0")
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
}

/** Whole local days between the row's day and `now`'s: 0 today, 1 yesterday. */
export function daysAgo(value: string, now: Date): number {
  const [y1, m1, d1] = localDay(value).split("-").map(Number)
  const [y2, m2, d2] = localDay(now).split("-").map(Number)
  return Math.round((Date.UTC(y2!, m2! - 1, d2!) - Date.UTC(y1!, m1! - 1, d1!)) / 86_400_000)
}

function localMidnight(day: Date): Date {
  return new Date(day.getFullYear(), day.getMonth(), day.getDate())
}

/** `[local midnight, now]` — what "today" means to the pickers. */
export function todayRange(now: Date): { from: string; to: string } {
  return { from: localMidnight(now).toISOString(), to: now.toISOString() }
}

export type ExportRangeKind = "today" | "7d" | "30d" | "custom"

/**
 * The export's `[from, to]`. 7 and 30 days are calendar days including today;
 * a custom range runs from the start of its first local day to the end of its
 * last. `null` when a custom date is missing or the two are reversed.
 */
export function exportRange(
  kind: ExportRangeKind,
  now: Date,
  custom?: { from: string; to: string },
): { from: string; to: string } | null {
  if (kind === "custom") {
    if (!custom?.from || !custom.to) return null
    const [fy, fm, fd] = custom.from.split("-").map(Number)
    const [ty, tm, td] = custom.to.split("-").map(Number)
    const from = new Date(fy!, fm! - 1, fd!)
    const to = new Date(ty!, tm! - 1, td! + 1, 0, 0, 0, -1)
    if (Number.isNaN(from.getTime()) || Number.isNaN(to.getTime()) || from > to) return null
    return { from: from.toISOString(), to: to.toISOString() }
  }
  const back = kind === "today" ? 0 : kind === "7d" ? 6 : 29
  const start = localMidnight(now)
  start.setDate(start.getDate() - back)
  return { from: start.toISOString(), to: now.toISOString() }
}

/** Minutes east of UTC right now — the offset the export writes local times in. */
export function offsetMinutes(now: Date): number {
  return -now.getTimezoneOffset()
}
