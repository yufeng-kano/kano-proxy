/**
 * Overview aggregation: turns the sparse `/api/usage/summary` series into the
 * zero-filled, model-ranked, color-assigned shapes the metric cards, the
 * activity chart, and the detail modals all draw from — one implementation,
 * so every surface tells the same story (docs/admin-ui.md § Overview page).
 */

import type { BarBucket } from "./BarChart.vue"
import type { Formatters } from "@/i18n"
import type { UsageSeriesPoint, UsageSummary } from "@/types"

export type MetricId = "spend" | "requests" | "tokens"

/**
 * Categorical slots for model identity (see the --series-* note in
 * styles.css). Six is the validated set; everything past it folds into a
 * single "Others" series rather than generating a 7th hue.
 */
const SERIES_COLORS = [
  "var(--series-1)",
  "var(--series-2)",
  "var(--series-3)",
  "var(--series-4)",
  "var(--series-5)",
  "var(--series-6)",
]
const OTHER_COLOR = "var(--series-other)"
/** Identity for the folded tail. No slash, so it can never collide with a real `provider/model` id. */
const OTHER_KEY = "__other__"

type RankedModel = {
  /** Raw `provider/model` id, or OTHER_KEY. */
  key: string
  /** What every surface prints: the canonical id, or the "Others" label for the folded tail. */
  label: string
  color: string
  /** Range total of the ranking metric. */
  total: number
}

export type MetricSeries = {
  /** Range total over every model. */
  total: number
  /** Ranked desc, "Others" last when the tail folded. */
  models: RankedModel[]
  buckets: BarBucket[]
}

/** The value one series point contributes to a metric. */
function metricOf(point: UsageSeriesPoint, metric: MetricId): number {
  if (metric === "requests") return point.requests
  if (metric === "spend") return point.cost ?? 0
  return point.prompt_tokens + point.completion_tokens
}

type TimeBucket = { key: string; date: Date }

/**
 * The API's key for the bucket containing `date`. Shifting by the bucket
 * offset before reading the UTC fields puts the key in the same calendar the
 * server grouped by — the picker's local midnights are not UTC midnights.
 */
function bucketKeyFor(grain: "hour" | "day", date: Date, offsetMs: number): string {
  const shifted = new Date(date.getTime() + offsetMs)
  const yyyy = shifted.getUTCFullYear()
  const mm = String(shifted.getUTCMonth() + 1).padStart(2, "0")
  const dd = String(shifted.getUTCDate()).padStart(2, "0")
  if (grain === "hour") {
    const hh = String(shifted.getUTCHours()).padStart(2, "0")
    return `${yyyy}-${mm}-${dd}T${hh}`
  }
  return `${yyyy}-${mm}-${dd}`
}

/**
 * The full zero-fill grid for a summary's range, oldest first: the exact
 * buckets covering `from`..`to`, keyed in the calendar the API bucketed in
 * (`offset`, minutes east of UTC — docs/admin-ui.md).
 */
function timeBuckets(summary: UsageSummary): TimeBucket[] {
  const stepMs = summary.grain === "hour" ? 3_600_000 : 86_400_000
  const offsetMs = summary.offset * 60_000
  const start = new Date(summary.from).getTime()
  // `to` is the range's last millisecond, so a 7-day week measures 1ms short
  // of 7 steps; rounding lands on the bucket count the calendar actually has.
  const spanMs = Math.max(stepMs, new Date(summary.to).getTime() - start)
  const count = Math.round(spanMs / stepMs)

  const out: TimeBucket[] = []
  for (let i = 0; i < count; i++) {
    const date = new Date(start + i * stepMs)
    out.push({ key: bucketKeyFor(summary.grain, date, offsetMs), date })
  }
  return out
}

/**
 * Models ranked by their range total of `metric`, top slots colored, tail
 * folded into "Others". Color follows the *model within this metric*: rank
 * order can differ between the three cards, but inside any one surface a
 * model keeps its hue across every bucket.
 *
 * The label is the canonical `provider/model` id itself — every Overview
 * surface names a model the same way the By-model table does, with no
 * friendly-name remap (docs/admin-ui.md § Overview page).
 */
function rankModels(
  summary: UsageSummary,
  metric: MetricId,
  otherLabel: string,
): RankedModel[] {
  const byModel = new Map<string, number>()
  for (const p of summary.series) {
    byModel.set(p.model, (byModel.get(p.model) ?? 0) + metricOf(p, metric))
  }
  const ranked = [...byModel.entries()]
    .sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1))
    .map(([key, total]) => ({ key, label: key, total }))

  if (ranked.length <= SERIES_COLORS.length) {
    return ranked.map((m, i) => ({ ...m, color: SERIES_COLORS[i]! }))
  }
  const kept = ranked.slice(0, SERIES_COLORS.length - 1)
  const folded = ranked.slice(SERIES_COLORS.length - 1)
  return [
    ...kept.map((m, i) => ({ ...m, color: SERIES_COLORS[i]! })),
    {
      key: OTHER_KEY,
      label: otherLabel,
      color: OTHER_COLOR,
      total: folded.reduce((sum, m) => sum + m.total, 0),
    },
  ]
}

/**
 * One metric's whole story: total, ranked models, and the stacked buckets.
 * Stack order matches the ranking, largest at the baseline, so the segment
 * colors read in the same order as the card's model list.
 */
export function buildMetricSeries(
  summary: UsageSummary,
  metric: MetricId,
  otherLabel: string,
  format: Formatters,
): MetricSeries {
  const models = rankModels(summary, metric, otherLabel)
  const named = new Map(models.map((m) => [m.key, m]))
  const other = named.get(OTHER_KEY) ?? null
  const hourly = summary.grain === "hour"

  const byBucket = new Map<string, Map<string, number>>()
  for (const p of summary.series) {
    const series = named.get(p.model) ?? other
    if (!series) continue
    let slots = byBucket.get(p.bucket)
    if (!slots) {
      slots = new Map()
      byBucket.set(p.bucket, slots)
    }
    slots.set(series.key, (slots.get(series.key) ?? 0) + metricOf(p, metric))
  }

  const buckets: BarBucket[] = timeBuckets(summary).map(({ key, date }) => {
    const slots = byBucket.get(key)
    return {
      key,
      label: format.bucketLabel(date, hourly),
      fullLabel: format.bucketFull(date, hourly),
      segments: models.map((m) => ({
        key: m.key,
        label: m.label,
        color: m.color,
        value: slots?.get(m.key) ?? 0,
      })),
    }
  })

  return {
    total: models.reduce((sum, m) => sum + m.total, 0),
    models,
    buckets,
  }
}

/**
 * The cache tab's two fixed series: uncached input at the baseline, cached
 * on top (docs/admin-ui.md). Not model identity — token *kind*, which is the
 * separate `--chart-input*` token family.
 */
export function buildCacheSeries(
  summary: UsageSummary,
  labels: { cached: string; uncached: string },
  format: Formatters,
): MetricSeries {
  const hourly = summary.grain === "hour"
  const byBucket = new Map<string, { cached: number; uncached: number }>()
  let cachedTotal = 0
  let uncachedTotal = 0
  for (const p of summary.series) {
    let slot = byBucket.get(p.bucket)
    if (!slot) {
      slot = { cached: 0, uncached: 0 }
      byBucket.set(p.bucket, slot)
    }
    const cached = p.cache_read_input_tokens
    const uncached = Math.max(0, p.prompt_tokens - cached)
    slot.cached += cached
    slot.uncached += uncached
    cachedTotal += cached
    uncachedTotal += uncached
  }

  const CACHED = { key: "cached", label: labels.cached, color: "var(--chart-input)" }
  const UNCACHED = { key: "uncached", label: labels.uncached, color: "var(--chart-input-soft)" }

  const buckets: BarBucket[] = timeBuckets(summary).map(({ key, date }) => {
    const slot = byBucket.get(key)
    return {
      key,
      label: format.bucketLabel(date, hourly),
      fullLabel: format.bucketFull(date, hourly),
      segments: [
        { ...UNCACHED, value: slot?.uncached ?? 0 },
        { ...CACHED, value: slot?.cached ?? 0 },
      ],
    }
  })

  return {
    total: cachedTotal + uncachedTotal,
    models: [
      { ...UNCACHED, total: uncachedTotal },
      { ...CACHED, total: cachedTotal },
    ],
    buckets,
  }
}

export type ModelRangeStats = {
  key: string
  label: string
  color: string
  min: number | null
  max: number | null
  avg: number | null
  sum: number
}

/**
 * Per-model Min / Max / Avg / Sum over the range's buckets for the detail
 * modal table. Min/max/avg run over the model's *non-zero* buckets — the
 * quiet hours would otherwise pin every Min to 0 and drag Avg toward it,
 * saying more about the range than about the model. All-zero = "—" row.
 */
export function modelRangeStats(series: MetricSeries): ModelRangeStats[] {
  return series.models.map((m) => {
    const values = series.buckets
      .map((b) => b.segments.find((s) => s.key === m.key)?.value ?? 0)
      .filter((v) => v > 0)
    if (!values.length) {
      return { key: m.key, label: m.label, color: m.color, min: null, max: null, avg: null, sum: 0 }
    }
    const sum = values.reduce((a, v) => a + v, 0)
    return {
      key: m.key,
      label: m.label,
      color: m.color,
      min: Math.min(...values),
      max: Math.max(...values),
      avg: sum / values.length,
      sum,
    }
  })
}
