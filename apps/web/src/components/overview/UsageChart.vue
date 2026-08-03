<script setup lang="ts">
/**
 * The Overview time series: grouped token columns, or one cache-rate curve per
 * model, over the same range — plus the legend, tooltip, and table twin that
 * keep every value reachable without color (docs/admin-ui.md § Accessibility
 * floor).
 *
 * Hand-rolled inline SVG, no charting dependency. Two things about the
 * coordinate system are load-bearing:
 *
 * 1. **The plot is a fixed height, never an aspect ratio.** An aspect-ratio
 *    chart in a wide region grows until it owns the viewport, which is exactly
 *    what this page must not do.
 * 2. **The viewBox is measured, so one unit is one CSS pixel.** That keeps the
 *    fixed height honest at any width: text renders at its true size, a 4px
 *    corner radius is 4px, and nothing is letterboxed or stretched.
 *
 * The aggregation below (bucket zero-fill, model ranking and the "Other" fold,
 * grouped-bar geometry, gaps-not-zeros in the rate curve) is ported from the
 * page this replaced. It is subtle and correct; the comments explain the parts
 * that look like they could be simplified but cannot.
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue"
import DataTable, { type Column } from "@/components/ui/DataTable.vue"
import { useI18n } from "@/i18n"
import type { ChartView } from "@/services/prefs"
import type { UsageDays, UsageSummary } from "@/types"
import ChartTooltip, { type TooltipRow } from "./ChartTooltip.vue"

const props = defineProps<{
  summary: UsageSummary
  view: ChartView
  showTable: boolean
}>()

const { t, format } = useI18n()

// ---------------------------------------------------------------------------
// Geometry. Plain numbers on purpose: these are coordinates, not styling — and
// with the measured viewBox below, one unit is one CSS pixel.
// ---------------------------------------------------------------------------
const PLOT_H = 260
const MARGIN = { top: 12, right: 10, bottom: 26, left: 46 }
const innerH = PLOT_H - MARGIN.top - MARGIN.bottom
const GROUP_GAP = 6
const BAR_GAP = 2
const MAX_BAR_W = 24
const BAR_RADIUS = 4
/** Half a typical tooltip — the clamp that keeps it inside the plot. */
const TOOLTIP_HALF = 84
/** Width used until the ResizeObserver reports the real one. */
const FALLBACK_PLOT_W = 640

/**
 * Categorical slots for model identity (see the --series-* note in
 * styles.css). Six is the validated set; everything past it folds into a
 * single "Other" series rather than generating a 7th hue.
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

type ModelSeries = {
  /** Raw model id, or OTHER_KEY. Stable across buckets and across a locale change. */
  key: string
  label: string
  color: string
  /** Range-total tokens — drives both the slot assignment and the legend order. */
  total: number
}

/** One model's slice of one time bucket. */
type BucketModel = {
  key: string
  label: string
  color: string
  uncached: number
  cached: number
  completion: number
  requests: number
  promptTokens: number
  cacheReadTokens: number
  cacheKnownRequests: number
  total: number
  /** Bucket cache hit rate; null when no request here reported cache data. */
  cacheRate: number | null
}

type ChartBucket = {
  key: string
  date: Date
  models: BucketModel[]
  /** Bucket totals — the sum over `models`, kept for the tooltip and table. */
  uncached: number
  cached: number
  completion: number
  requests: number
  total: number
  cacheRate: number | null
}

type BarGeom = {
  key: string
  color: string
  x: number
  y: number
  width: number
  height: number
}

type GroupGeom = ChartBucket & {
  /** Full slot — the hover/focus hit target, wider than the bars it holds. */
  hitX: number
  hitWidth: number
  bars: BarGeom[]
}

const isRateView = computed(() => props.view === "cache-rate")
const hourly = computed(() => props.summary.days === 1)

const hoveredIndex = ref<number | null>(null)
watch(() => props.view, () => (hoveredIndex.value = null))

// ---------------------------------------------------------------------------
// Measured viewBox — one unit is one CSS pixel, so the plot fills its fixed
// height exactly instead of being scaled to fit an assumed aspect ratio.
//
// The observed element is the component root, which is mounted in both views:
// observing the plot itself would go stale while the table twin is showing and
// come back wrong the moment the user switches back.
// ---------------------------------------------------------------------------
const root = ref<HTMLElement | null>(null)
const availableW = ref(0)
let resizeObserver: ResizeObserver | undefined

onMounted(() => {
  const el = root.value
  if (!el) return
  availableW.value = el.clientWidth
  if (typeof ResizeObserver === "undefined") return
  resizeObserver = new ResizeObserver(() => {
    availableW.value = el.clientWidth
  })
  resizeObserver.observe(el)
})

onBeforeUnmount(() => resizeObserver?.disconnect())

/**
 * Mirrors what the plot element actually resolves to: the available width,
 * raised by its `min-width` floor. Keeping the two in step is what makes
 * `preserveAspectRatio="none"` a no-op rather than a stretch.
 */
const plotW = computed(() =>
  Math.max(availableW.value || FALLBACK_PLOT_W, chartMinWidth.value),
)
const innerW = computed(() => Math.max(1, plotW.value - MARGIN.left - MARGIN.right))

// ---------------------------------------------------------------------------
// Time series — zero-filled client-side over the full range (UTC bucket keys
// from the API; missing buckets filled with zeros; labels formatted in local
// time). See docs/admin-ui.md § Overview page.
// ---------------------------------------------------------------------------
function bucketKeyFor(d: UsageDays, date: Date): string {
  const yyyy = date.getUTCFullYear()
  const mm = String(date.getUTCMonth() + 1).padStart(2, "0")
  const dd = String(date.getUTCDate()).padStart(2, "0")
  if (d === 1) {
    const hh = String(date.getUTCHours()).padStart(2, "0")
    return `${yyyy}-${mm}-${dd}T${hh}`
  }
  return `${yyyy}-${mm}-${dd}`
}

/**
 * Model identity, ordered by range-total tokens desc — the same order as the
 * per-model table, so the two read as one story. Color follows the *model*,
 * not its rank within a bucket, so a model keeps its hue across every bucket
 * and across a range change. Past six models the tail folds into one "Other"
 * series rather than generating a 7th hue.
 */
const modelSeries = computed<ModelSeries[]>(() => {
  const byModel = new Map<string, number>()
  for (const p of props.summary.series) {
    byModel.set(p.model, (byModel.get(p.model) ?? 0) + p.prompt_tokens + p.completion_tokens)
  }
  const ranked = [...byModel.entries()]
    .sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1))
    .map(([key, total]) => ({ key, label: key, total }))

  if (ranked.length <= SERIES_COLORS.length) {
    return ranked.map((m, i) => ({ ...m, color: SERIES_COLORS[i]! }))
  }
  // Keep the first five named, fold the rest — so "Other" is always the last
  // legend entry and never occupies a categorical slot.
  const kept = ranked.slice(0, SERIES_COLORS.length - 1)
  const folded = ranked.slice(SERIES_COLORS.length - 1)
  return [
    ...kept.map((m, i) => ({ ...m, color: SERIES_COLORS[i]! })),
    {
      key: OTHER_KEY,
      label: t("overview.chart.other"),
      color: OTHER_COLOR,
      total: folded.reduce((sum, m) => sum + m.total, 0),
    },
  ]
})

/** Maps a raw model id to the series it renders as ("Other" for the folded tail). */
const seriesForModel = computed<Map<string, ModelSeries>>(() => {
  const named = new Set(modelSeries.value.map((m) => m.key))
  const other = modelSeries.value.find((m) => m.key === OTHER_KEY)
  const out = new Map<string, ModelSeries>()
  for (const m of modelSeries.value) out.set(m.key, m)
  if (other) {
    for (const p of props.summary.series) {
      if (!named.has(p.model)) out.set(p.model, other)
    }
  }
  return out
})

function emptyBucketModel(s: ModelSeries): BucketModel {
  return {
    key: s.key,
    label: s.label,
    color: s.color,
    uncached: 0,
    cached: 0,
    completion: 0,
    requests: 0,
    promptTokens: 0,
    cacheReadTokens: 0,
    cacheKnownRequests: 0,
    total: 0,
    cacheRate: null,
  }
}

const buckets = computed<ChartBucket[]>(() => {
  const s = props.summary
  const nominalCount = s.days === 1 ? 24 : s.days
  const stepMs = s.days === 1 ? 3_600_000 : 86_400_000
  const parsed = new Date(s.from)
  const raw = Number.isNaN(parsed.getTime()) ? new Date(Date.now() - nominalCount * stepMs) : parsed
  // Truncate to a clean UTC hour/day boundary so generated keys line up with
  // the API's bucket keys even if `from` carries minute/second jitter.
  const start =
    s.days === 1
      ? new Date(
          Date.UTC(raw.getUTCFullYear(), raw.getUTCMonth(), raw.getUTCDate(), raw.getUTCHours()),
        )
      : new Date(Date.UTC(raw.getUTCFullYear(), raw.getUTCMonth(), raw.getUTCDate()))

  // +1: `from` is a rolling "now - N*bucketWidth" boundary, not a calendar
  // boundary (apps/api/src/routes/usage.ts computes it as
  // `Date.now() - days*86400000`), while bucket keys are calendar hour/day
  // truncations of each row's own created_at. So the query window straddles
  // nominalCount+1 distinct calendar buckets: a partial bucket at `from`'s
  // own hour/day, nominalCount-1 full buckets, and a partial bucket at
  // "now" (today, or the current hour). Without the +1 that last bucket —
  // the most recent, most relevant one — would be silently dropped.
  const bucketCount = nominalCount + 1

  // Series is sparse in both dimensions, so group it by bucket first; each
  // bucket then gets a full row of model slots (zero-filled) so every group
  // has the same bar count and the x positions stay stable across buckets.
  const byBucket = new Map<string, typeof s.series>()
  for (const p of s.series) {
    const list = byBucket.get(p.bucket)
    if (list) list.push(p)
    else byBucket.set(p.bucket, [p])
  }

  const lookup = seriesForModel.value
  const out: ChartBucket[] = []
  for (let i = 0; i < bucketCount; i++) {
    const date = new Date(start.getTime() + i * stepMs)
    const key = bucketKeyFor(s.days, date)

    const slots = new Map<string, BucketModel>()
    for (const series of modelSeries.value) slots.set(series.key, emptyBucketModel(series))

    for (const p of byBucket.get(key) ?? []) {
      // A model absent from `lookup` can't happen (both derive from the same
      // series array) but folding to "Other" is the safe read either way.
      const target = lookup.get(p.model)
      const slot = target ? slots.get(target.key) : undefined
      if (!slot) continue
      slot.uncached += Math.max(0, p.prompt_tokens - p.cache_read_input_tokens)
      slot.cached += p.cache_read_input_tokens
      slot.completion += p.completion_tokens
      slot.requests += p.requests
      slot.promptTokens += p.prompt_tokens
      slot.cacheReadTokens += p.cache_read_input_tokens
      slot.cacheKnownRequests += p.cache_known_requests
    }

    const models = [...slots.values()]
    for (const m of models) {
      m.total = m.uncached + m.cached + m.completion
      // Null, not 0: a bucket where nothing reported cache data is a gap in
      // the curve, not a 0% reading.
      m.cacheRate =
        m.cacheKnownRequests > 0 && m.promptTokens > 0 ? m.cacheReadTokens / m.promptTokens : null
    }

    const uncached = models.reduce((sum, m) => sum + m.uncached, 0)
    const cached = models.reduce((sum, m) => sum + m.cached, 0)
    const completion = models.reduce((sum, m) => sum + m.completion, 0)
    const promptTokens = models.reduce((sum, m) => sum + m.promptTokens, 0)
    const cacheReadTokens = models.reduce((sum, m) => sum + m.cacheReadTokens, 0)
    const cacheKnown = models.reduce((sum, m) => sum + m.cacheKnownRequests, 0)
    out.push({
      key,
      date,
      models,
      uncached,
      cached,
      completion,
      requests: models.reduce((sum, m) => sum + m.requests, 0),
      total: uncached + cached + completion,
      cacheRate: cacheKnown > 0 && promptTokens > 0 ? cacheReadTokens / promptTokens : null,
    })
  }
  return out
})

// ---------------------------------------------------------------------------
// Scales
// ---------------------------------------------------------------------------
function niceStep(rough: number): number {
  if (rough <= 0) return 1
  const magnitude = 10 ** Math.floor(Math.log10(rough))
  const residual = rough / magnitude
  const niceResidual = residual < 1.5 ? 1 : residual < 3 ? 2 : residual < 7 ? 5 : 10
  return niceResidual * magnitude
}

function buildYTicks(maxVal: number, targetCount = 4): number[] {
  if (maxVal <= 0) return [0]
  const step = niceStep(maxVal / targetCount)
  const ticks: number[] = []
  for (let v = 0; v <= maxVal + step * 0.5; v += step) ticks.push(Math.round(v))
  return ticks
}

/** Tallest single bar — grouped bars scale to the per-model max, not a bucket sum. */
const chartMaxRaw = computed(() =>
  buckets.value.reduce((m, b) => b.models.reduce((mm, s) => Math.max(mm, s.total), m), 0),
)
const yTicks = computed(() => buildYTicks(chartMaxRaw.value))
const chartMax = computed(() => yTicks.value[yTicks.value.length - 1] ?? 1)

/**
 * Floor width for the plot. Below this the groups collide, so the plot keeps
 * this width and its container scrolls horizontally instead. Derived from the
 * bucket count rather than from `groupGeom` — the geometry depends on the
 * resolved width, so reading it back here would be a cycle.
 */
const chartMinWidth = computed(() => {
  const perGroup = Math.max(16, modelSeries.value.length * 8)
  return Math.max(480, buckets.value.length * perGroup)
})

const groupGeom = computed<GroupGeom[]>(() => {
  const list = buckets.value
  const n = list.length
  if (!n) return []
  const seriesCount = Math.max(1, modelSeries.value.length)
  const slot = (innerW.value - GROUP_GAP * (n - 1)) / n
  // Bars share the slot; BAR_GAP is the surface gap between neighbours, and
  // MAX_BAR_W caps thickness so a one-model range doesn't render a slab.
  const barW = Math.min(MAX_BAR_W, Math.max(1, (slot - BAR_GAP * (seriesCount - 1)) / seriesCount))
  const groupW = barW * seriesCount + BAR_GAP * (seriesCount - 1)
  const max = chartMax.value
  const baselineY = MARGIN.top + innerH
  return list.map((b, i) => {
    const hitX = MARGIN.left + i * (slot + GROUP_GAP)
    const groupX = hitX + (slot - groupW) / 2
    const bars: BarGeom[] = b.models.map((m, j) => {
      // Floor a nonzero value at 1.5 so a small-but-real bucket stays visible.
      const h = m.total > 0 ? Math.max(1.5, max > 0 ? (m.total / max) * innerH : 0) : 0
      return {
        key: m.key,
        color: m.color,
        x: groupX + j * (barW + BAR_GAP),
        y: baselineY - h,
        width: barW,
        height: h,
      }
    })
    return { ...b, hitX, hitWidth: slot, bars }
  })
})

const xLabelStep = computed(() => Math.max(1, Math.ceil(buckets.value.length / 8)))

function showXLabel(i: number): boolean {
  return i % xLabelStep.value === 0 || i === buckets.value.length - 1
}

function yFor(value: number): number {
  const max = chartMax.value
  return MARGIN.top + innerH - (max > 0 ? (value / max) * innerH : 0)
}

// ---------------------------------------------------------------------------
// Cache-rate curve — one line per model, fixed 0–100% y-axis. A bucket where
// no request reported cache data is a *gap*, not a zero: the line breaks
// rather than diving to the floor and inventing a cache miss.
// ---------------------------------------------------------------------------
const RATE_TICKS = [0, 0.25, 0.5, 0.75, 1]

type RatePoint = { x: number; y: number; bucketKey: string }
type RateLine = { key: string; color: string; segments: string[]; points: RatePoint[] }

function rateY(rate: number): number {
  return MARGIN.top + innerH - Math.min(1, Math.max(0, rate)) * innerH
}

const rateLines = computed<RateLine[]>(() => {
  const groups = groupGeom.value
  if (!groups.length) return []
  return modelSeries.value.map((series) => {
    const points: RatePoint[] = []
    const segments: string[] = []
    let run: string[] = []
    groups.forEach((group) => {
      const rate = group.models.find((bm) => bm.key === series.key)?.cacheRate ?? null
      if (rate == null) {
        // Break the run — a gap must not be bridged by a straight line
        // through buckets that reported nothing.
        if (run.length > 1) segments.push(run.join(" "))
        run = []
        return
      }
      // Share the bar view's slot centers so a point sits under the same
      // hover band that highlights it, and the two views' x-axes line up
      // when the user flips between them.
      const x = group.hitX + group.hitWidth / 2
      const y = rateY(rate)
      points.push({ x, y, bucketKey: group.key })
      run.push(`${run.length === 0 ? "M" : "L"}${x.toFixed(2)},${y.toFixed(2)}`)
    })
    if (run.length > 1) segments.push(run.join(" "))
    return { key: series.key, color: series.color, segments, points }
  })
})

/**
 * A lone reading between two gaps produces no line segment (a path needs two
 * points), so its dot is the only mark carrying it — which is why dots render
 * in both views rather than only at hover.
 */
const rateHasAnyPoint = computed(() => rateLines.value.some((l) => l.points.length > 0))

// ---------------------------------------------------------------------------
// Bar paths
// ---------------------------------------------------------------------------
function plainRectPath(x: number, y: number, w: number, h: number): string {
  return `M${x},${y} h${w} v${h} h${-w} Z`
}

/** Rounded top corners only; square bottom — the baseline stays flat. */
function roundedTopRectPath(x: number, y: number, w: number, h: number, r: number): string {
  const radius = Math.max(0, Math.min(r, h, w / 2))
  if (radius <= 0.01) return plainRectPath(x, y, w, h)
  return [
    `M${x},${y + radius}`,
    `Q${x},${y} ${x + radius},${y}`,
    `H${x + w - radius}`,
    `Q${x + w},${y} ${x + w},${y + radius}`,
    `V${y + h}`,
    `H${x}`,
    `Z`,
  ].join(" ")
}

function barPath(bar: BarGeom): string {
  if (bar.height <= 0) return ""
  return roundedTopRectPath(bar.x, bar.y, bar.width, bar.height, BAR_RADIUS)
}

// ---------------------------------------------------------------------------
// Descriptions and labels
// ---------------------------------------------------------------------------
const chartTitle = computed(() =>
  isRateView.value ? t("overview.chart.cacheRateTitle") : t("overview.chart.tokensTitle"),
)

const chartDesc = computed(() => {
  const totals = props.summary.totals
  const models = modelSeries.value.length
  if (isRateView.value) {
    return t("overview.chart.cacheRateDesc", { models, rate: format.percent(totals.cache_rate) })
  }
  return t("overview.chart.tokensDesc", {
    models,
    tokens: format.compact(totals.prompt_tokens + totals.completion_tokens),
    rate: format.percent(totals.cache_rate),
  })
})

/** Screen-reader summary of one time bucket, per active view. */
function bucketAriaLabel(group: GroupGeom): string {
  const when = format.bucketFull(group.date, hourly.value)
  const parts = isRateView.value
    ? group.models.map((m) =>
        m.cacheRate == null
          ? t("overview.chart.modelNoRate", { model: m.label })
          : t("overview.chart.modelRate", { model: m.label, value: format.percent(m.cacheRate) }),
      )
    : group.models
        .filter((m) => m.total > 0)
        .map((m) => t("overview.chart.modelTokens", { model: m.label, value: format.compact(m.total) }))
  // A list separator, not copy: the pieces on either side are catalog strings.
  const detail = parts.length ? parts.join(", ") : t("overview.chart.bucketNoData")
  return t("overview.chart.bucketAria", { when, detail })
}

// ---------------------------------------------------------------------------
// Tooltip
// ---------------------------------------------------------------------------
const hoveredBucket = computed<GroupGeom | null>(() =>
  hoveredIndex.value != null ? (groupGeom.value[hoveredIndex.value] ?? null) : null,
)

const tooltipX = computed(() => {
  const b = hoveredBucket.value
  if (!b) return plotW.value / 2
  const center = b.hitX + b.hitWidth / 2
  return Math.min(plotW.value - TOOLTIP_HALF, Math.max(TOOLTIP_HALF, center))
})

const tooltipTitle = computed(() => {
  const b = hoveredBucket.value
  return b ? format.bucketFull(b.date, hourly.value) : ""
})

/**
 * Only models with something to say are listed — a range with six models would
 * otherwise print four "—" rows in every quiet bucket.
 */
const tooltipRows = computed<TooltipRow[]>(() => {
  const b = hoveredBucket.value
  if (!b) return []
  if (isRateView.value) {
    return b.models
      .filter((m) => m.cacheRate != null)
      .map((m) => ({ key: m.key, label: m.label, color: m.color, value: format.percent(m.cacheRate) }))
  }
  return b.models
    .filter((m) => m.total > 0)
    .map((m) => ({ key: m.key, label: m.label, color: m.color, value: format.compact(m.total) }))
})

const tooltipTotal = computed(() => {
  const b = hoveredBucket.value
  if (!b || isRateView.value || !tooltipRows.value.length) return null
  return { label: t("overview.chart.total"), value: format.compact(b.total) }
})

const tooltipFooter = computed<string[]>(() => {
  const b = hoveredBucket.value
  if (!b || isRateView.value || !tooltipRows.value.length) return []
  return [
    t("overview.chart.cached", { value: format.compact(b.cached) }),
    t("overview.chart.completion", { value: format.compact(b.completion) }),
    t("overview.chart.requests", { count: b.requests }),
  ]
})

// ---------------------------------------------------------------------------
// Table twin — the same numbers, exactly, for anyone the chart does not serve.
// One column per model, so the column set is data-driven.
// ---------------------------------------------------------------------------
const tableColumns = computed<Column<GroupGeom>[]>(() => {
  const modelColumns: Column<GroupGeom>[] = modelSeries.value.map((s) => ({
    key: s.key,
    header: s.label,
    numeric: true,
    value: (row) => {
      const m = row.models.find((bm) => bm.key === s.key)
      if (!m) return format.compact(null)
      return isRateView.value ? format.percent(m.cacheRate) : format.compact(m.total)
    },
  }))

  const columns: Column<GroupGeom>[] = [
    {
      key: "time",
      header: t("overview.table.time"),
      value: (row) => format.bucketFull(row.date, hourly.value),
    },
    ...modelColumns,
  ]

  if (!isRateView.value) {
    columns.push(
      {
        key: "total",
        header: t("overview.table.total"),
        numeric: true,
        value: (row) => format.compact(row.total),
      },
      {
        key: "requests",
        header: t("overview.table.requests"),
        numeric: true,
        value: (row) => format.integer(row.requests),
      },
    )
  }
  return columns
})

function rowKey(row: GroupGeom): string {
  return row.key
}
</script>

<template>
  <div ref="root" class="chart">
    <!-- The table twin gets the plot's own height and scrolls inside it, so
         toggling chart/table never resizes the card around it. DataTable's
         sticky header is what keeps the columns readable through that scroll. -->
    <div v-if="showTable" class="chart-table">
      <DataTable
        :columns="tableColumns"
        :rows="groupGeom"
        :row-key="rowKey"
        :caption="chartTitle"
      />
    </div>

    <div v-else class="chart-scroll">
      <div class="chart-plot" :style="{ '--plot-min': `${chartMinWidth}px` }">
        <ChartTooltip
          v-if="hoveredBucket"
          :x="tooltipX"
          :title="tooltipTitle"
          :rows="tooltipRows"
          :total="tooltipTotal"
          :footer="tooltipFooter"
          :empty="isRateView ? t('overview.chart.noCacheData') : t('overview.chart.emptyBucket')"
        />

        <!-- `preserveAspectRatio="none"` is safe *because* the viewBox is
             measured: one unit is one CSS pixel, so there is nothing to
             stretch. It is what stops the browser letterboxing the plot into
             the fixed height. -->
        <svg class="chart-svg" :viewBox="`0 0 ${plotW} ${PLOT_H}`" preserveAspectRatio="none">
          <title>{{ chartTitle }}</title>
          <desc>{{ chartDesc }}</desc>

          <!-- Horizontal grid only, no axis lines and no tick marks: vertical
               rules would compete with the bars standing on them. -->
          <template v-if="!isRateView">
            <line
              v-for="tick in yTicks"
              :key="`grid-${tick}`"
              class="grid"
              :x1="MARGIN.left"
              :x2="plotW - MARGIN.right"
              :y1="yFor(tick)"
              :y2="yFor(tick)"
            />
            <text
              v-for="tick in yTicks"
              :key="`ylabel-${tick}`"
              class="axis-label"
              :x="MARGIN.left - 8"
              :y="yFor(tick)"
              text-anchor="end"
              dominant-baseline="middle"
            >{{ format.compact(tick) }}</text>
          </template>
          <template v-else>
            <line
              v-for="tick in RATE_TICKS"
              :key="`rgrid-${tick}`"
              class="grid"
              :x1="MARGIN.left"
              :x2="plotW - MARGIN.right"
              :y1="rateY(tick)"
              :y2="rateY(tick)"
            />
            <text
              v-for="tick in RATE_TICKS"
              :key="`rylabel-${tick}`"
              class="axis-label"
              :x="MARGIN.left - 8"
              :y="rateY(tick)"
              text-anchor="end"
              dominant-baseline="middle"
            >{{ format.percent(tick, 0) }}</text>
          </template>

          <!-- Grouped bars: one per model per bucket -->
          <g v-if="!isRateView">
            <g v-for="(group, i) in groupGeom" :key="group.key">
              <rect
                v-if="hoveredIndex === i"
                class="hit-highlight"
                :x="group.hitX"
                :y="MARGIN.top"
                :width="group.hitWidth"
                :height="innerH"
              />
              <g class="bars" :class="{ hovered: hoveredIndex === i }">
                <path v-for="bar in group.bars" :key="bar.key" :d="barPath(bar)" :fill="bar.color" />
              </g>
            </g>
          </g>

          <!-- Cache-rate curve: one line per model; gaps stay gaps -->
          <g v-else>
            <rect
              v-if="hoveredBucket"
              class="hit-highlight"
              :x="hoveredBucket.hitX"
              :y="MARGIN.top"
              :width="hoveredBucket.hitWidth"
              :height="innerH"
            />
            <g v-for="line in rateLines" :key="`line-${line.key}`">
              <path
                v-for="(d, si) in line.segments"
                :key="si"
                class="rate-line"
                :d="d"
                :stroke="line.color"
              />
              <circle
                v-for="(pt, pi) in line.points"
                :key="`dot-${pi}`"
                class="rate-dot"
                :cx="pt.x"
                :cy="pt.y"
                :r="hoveredBucket?.key === pt.bucketKey ? 4 : 2.5"
                :fill="line.color"
              />
            </g>
          </g>

          <!-- Hit targets last so they stay above the marks -->
          <rect
            v-for="(group, i) in groupGeom"
            :key="`hit-${group.key}`"
            class="hit"
            :x="group.hitX"
            :y="MARGIN.top"
            :width="group.hitWidth"
            :height="innerH"
            tabindex="0"
            role="group"
            :aria-label="bucketAriaLabel(group)"
            @pointerenter="hoveredIndex = i"
            @pointerleave="hoveredIndex = null"
            @focus="hoveredIndex = i"
            @blur="hoveredIndex = null"
          />

          <template v-for="(group, i) in groupGeom" :key="`xlabel-${group.key}`">
            <text
              v-if="showXLabel(i)"
              class="axis-label"
              :x="group.hitX + group.hitWidth / 2"
              :y="MARGIN.top + innerH + 16"
              text-anchor="middle"
            >{{ format.bucketLabel(group.date, hourly) }}</text>
          </template>
        </svg>
      </div>
    </div>

    <p v-if="!showTable && isRateView && !rateHasAnyPoint" class="chart-note">
      {{ t("overview.chart.noCacheData") }}
    </p>

    <!-- The legend rides both views: it is the non-color half of model
         identity, so it must not disappear with the marks it names. -->
    <ul v-if="!showTable" class="legend">
      <li v-for="s in modelSeries" :key="s.key" class="legend-item">
        <span class="legend-swatch" :style="{ '--swatch': s.color }" />
        <span class="mono">{{ s.label }}</span>
      </li>
    </ul>
  </div>
</template>

<style scoped>
/* One height for the whole component, shared by the plot, the table twin, and
   the page's loading placeholder. Fixed on purpose: an aspect-ratio chart in a
   region this wide keeps growing until it owns the viewport, and the toggle
   between chart and table must not resize the card around it. */
.chart {
  --plot-height: 260px;

  display: flex;
  flex-direction: column;
}

/* The plot holds its width floor and scrolls sideways rather than crushing a
   30-day range into unreadable slivers. */
.chart-scroll {
  overflow-x: auto;
  overflow-y: hidden;
}

.chart-plot {
  position: relative;
  min-width: var(--plot-min);
  height: var(--plot-height);
}

/* A 31-row twin scrolls inside the same box the plot occupies. DataTable's
   sticky header keeps the columns readable through it. */
.chart-table {
  height: var(--plot-height);
  overflow: auto;
}

.chart-svg {
  display: block;
  width: 100%;
  height: 100%;
}

.grid {
  stroke: var(--border);
  stroke-width: 1;
  shape-rendering: crispEdges;
}

.axis-label {
  fill: var(--muted);
  font-size: var(--text-xs);
}

.bars path,
.hit-highlight,
.rate-line,
.rate-dot {
  pointer-events: none;
}

.bars.hovered path {
  filter: brightness(1.12);
}

/* 2px line, round join/cap; the surface-colored ring keeps a dot legible
   where two models' curves cross. */
.rate-line {
  fill: none;
  stroke-width: 2;
  stroke-linejoin: round;
  stroke-linecap: round;
}

.rate-dot {
  stroke: var(--surface);
  stroke-width: 2;
  transition: r var(--duration-fast) var(--ease);
}

.hit-highlight {
  fill: var(--hover);
}

.hit {
  fill: transparent;
  cursor: pointer;
}

.hit:focus-visible {
  outline: 2px solid var(--ring-border);
  outline-offset: -2px;
}

.chart-note {
  margin: var(--space-3) 0 0;
  color: var(--muted);
  font-size: var(--text-xs);
}

.legend {
  display: flex;
  flex-wrap: wrap;
  justify-content: center;
  gap: var(--space-2) var(--space-4);
  margin: var(--space-3) 0 0;
  padding: 0;
  list-style: none;
}

.legend-item {
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
  color: var(--text-secondary);
  font-size: var(--text-xs);
}

.legend-swatch {
  width: 8px;
  height: 8px;
  flex-shrink: 0;
  border-radius: 2px;
  background: var(--swatch);
}
</style>
