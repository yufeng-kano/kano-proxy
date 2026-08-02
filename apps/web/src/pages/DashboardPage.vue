<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue"
import { useAuth } from "@/composables/useAuth"
import { useScrollRestore } from "@/composables/useScrollRestore"
import { useUsage } from "@/composables/useUsage"
import { getDashboardPrefs, setDashboardPrefs, type ChartView } from "@/services/prefs"
import type { ModelUsageRow, UsageDays, UsageSummary } from "@/types"
import {
  formatCompactNumber,
  formatInt,
  formatLatencyMs,
  formatPercent1,
} from "@/utils/format"

const { user } = useAuth()
const {
  summary,
  loading,
  refreshing,
  error,
  fromCache,
  days,
  setDays,
  setUserId,
  refresh,
} = useUsage()
const { markReady } = useScrollRestore()

const RANGE_OPTIONS: { value: UsageDays; label: string }[] = [
  { value: 1, label: "24h" },
  { value: 7, label: "7d" },
  { value: 30, label: "30d" },
]

const CHART_VIEW_OPTIONS: { value: ChartView; label: string }[] = [
  { value: "tokens", label: "Tokens" },
  { value: "cache-rate", label: "Cache rate" },
]

// ---------------------------------------------------------------------------
// Chart geometry — internal SVG coordinate system (viewBox units, not px).
// ---------------------------------------------------------------------------
const CHART_VBW = 960
const CHART_VBH = 300
const MARGIN = { top: 16, right: 16, bottom: 34, left: 52 }
const innerW = CHART_VBW - MARGIN.left - MARGIN.right
const innerH = CHART_VBH - MARGIN.top - MARGIN.bottom
const GROUP_GAP = 6
const BAR_GAP = 2
const MAX_BAR_W = 24

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
const OTHER_MODEL = "Other"

type ModelSeries = {
  model: string
  color: string
  /** Range-total tokens — drives both the slot assignment and the legend order. */
  total: number
}

/** One model's slice of one time bucket. */
type BucketModel = {
  model: string
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
  model: string
  color: string
  x: number
  y: number
  width: number
  height: number
  datum: BucketModel
}

type GroupGeom = ChartBucket & {
  /** Full slot — the hover/focus hit target, wider than the bars it holds. */
  hitX: number
  hitWidth: number
  bars: BarGeom[]
}

const prefs = getDashboardPrefs()
const showTable = ref(prefs.showTable)
const chartView = ref<ChartView>(prefs.chartView)
const hoveredIndex = ref<number | null>(null)

watch(showTable, (v) => setDashboardPrefs({ showTable: v }))

function setChartView(next: ChartView) {
  if (chartView.value === next) return
  chartView.value = next
  hoveredIndex.value = null
  setDashboardPrefs({ chartView: next })
}

onMounted(async () => {
  setUserId(user.value?.id ?? null)
  await refresh()
  // Content has painted (or resolved to an empty state) — only now is the
  // document tall enough for a saved scroll offset to land.
  await markReady()
})

async function onManualRefresh() {
  await refresh({ refresh: true })
}

// ---------------------------------------------------------------------------
// Totals / tiles
// ---------------------------------------------------------------------------
const totals = computed(() => summary.value?.totals ?? null)
const isEmpty = computed(() => totals.value != null && totals.value.requests === 0)

const requestsDisplay = computed(() => formatInt(totals.value?.requests))
const totalTokensDisplay = computed(() => {
  const t = totals.value
  return t ? formatCompactNumber(t.prompt_tokens + t.completion_tokens) : "—"
})
const cacheRateDisplay = computed(() => formatPercent1(totals.value?.cache_rate))
const errorsDisplay = computed(() => formatInt(totals.value?.errors))
const avgLatencyDisplay = computed(() => formatLatencyMs(totals.value?.avg_latency_ms))
const cacheMeterPct = computed(() => meterPct(totals.value?.cache_rate))
const heroCoverageText = computed(() => {
  const t = totals.value
  if (!t) return null
  if (t.cache_rate == null) return "No cache data reported yet"
  if (t.cache_known_requests < t.requests) {
    return `based on ${formatInt(t.cache_known_requests)} of ${formatInt(t.requests)} requests`
  }
  return null
})

function meterPct(ratio: number | null | undefined): number {
  if (ratio == null || Number.isNaN(ratio)) return 0
  return Math.min(100, Math.max(0, ratio * 100))
}

// ---------------------------------------------------------------------------
// Time series — zero-filled client-side over the full range (UTC bucket
// keys from the API; missing buckets filled with zeros; labels formatted in
// local time). See docs/admin-ui.md Dashboard page.
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
function buildModelSeries(s: UsageSummary): ModelSeries[] {
  const totals = new Map<string, number>()
  for (const p of s.series) {
    totals.set(p.model, (totals.get(p.model) ?? 0) + p.prompt_tokens + p.completion_tokens)
  }
  const ranked = [...totals.entries()]
    .sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1))
    .map(([model, total]) => ({ model, total }))

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
      model: OTHER_MODEL,
      color: OTHER_COLOR,
      total: folded.reduce((sum, m) => sum + m.total, 0),
    },
  ]
}

const modelSeries = computed<ModelSeries[]>(() =>
  summary.value ? buildModelSeries(summary.value) : [],
)

/** Maps a raw model id to the series it renders as ("Other" for the folded tail). */
const seriesForModel = computed<Map<string, ModelSeries>>(() => {
  const named = new Set(modelSeries.value.map((m) => m.model))
  const other = modelSeries.value.find((m) => m.model === OTHER_MODEL)
  const out = new Map<string, ModelSeries>()
  for (const m of modelSeries.value) out.set(m.model, m)
  if (other && summary.value) {
    for (const p of summary.value.series) {
      if (!named.has(p.model)) out.set(p.model, other)
    }
  }
  return out
})

function emptyBucketModel(s: ModelSeries): BucketModel {
  return {
    model: s.model,
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

function buildBuckets(s: UsageSummary): ChartBucket[] {
  const nominalCount = s.days === 1 ? 24 : s.days
  const stepMs = s.days === 1 ? 3_600_000 : 86_400_000
  const parsed = new Date(s.from)
  const raw = Number.isNaN(parsed.getTime()) ? new Date(Date.now() - nominalCount * stepMs) : parsed
  // Truncate to a clean UTC hour/day boundary so generated keys line up with
  // the API's bucket keys even if `from` carries minute/second jitter.
  const start =
    s.days === 1
      ? new Date(Date.UTC(raw.getUTCFullYear(), raw.getUTCMonth(), raw.getUTCDate(), raw.getUTCHours()))
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
    for (const series of modelSeries.value) slots.set(series.model, emptyBucketModel(series))

    for (const p of byBucket.get(key) ?? []) {
      // A model absent from `lookup` can't happen (both derive from the same
      // series array) but folding to "Other" is the safe read either way.
      const target = lookup.get(p.model)
      const slot = target ? slots.get(target.model) : undefined
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
}

const buckets = computed<ChartBucket[]>(() => (summary.value ? buildBuckets(summary.value) : []))

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

const groupGeom = computed<GroupGeom[]>(() => {
  const list = buckets.value
  const n = list.length
  if (!n) return []
  const seriesCount = Math.max(1, modelSeries.value.length)
  const slot = (innerW - GROUP_GAP * (n - 1)) / n
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
        model: m.model,
        color: m.color,
        x: groupX + j * (barW + BAR_GAP),
        y: baselineY - h,
        width: barW,
        height: h,
        datum: m,
      }
    })
    return { ...b, hitX, hitWidth: slot, bars }
  })
})

const chartMinWidth = computed(() => {
  const n = groupGeom.value.length
  const perGroup = Math.max(16, modelSeries.value.length * 8)
  return Math.max(480, n * perGroup)
})
const xLabelStep = computed(() => Math.max(1, Math.ceil(groupGeom.value.length / 8)))

function showXLabel(i: number): boolean {
  const n = groupGeom.value.length
  return i % xLabelStep.value === 0 || i === n - 1
}

function yFor(t: number): number {
  const max = chartMax.value
  return MARGIN.top + innerH - (max > 0 ? (t / max) * innerH : 0)
}

// ---------------------------------------------------------------------------
// Cache-rate curve — one line per model, fixed 0–100% y-axis. A bucket where
// no request reported cache data is a *gap*, not a zero: the line breaks
// rather than diving to the floor and inventing a cache miss.
// ---------------------------------------------------------------------------
const RATE_TICKS = [0, 0.25, 0.5, 0.75, 1]

type RatePoint = { x: number; y: number; rate: number; bucket: ChartBucket }
type RateLine = { model: string; color: string; segments: string[]; points: RatePoint[] }

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
      const m = group.models.find((bm) => bm.model === series.model)
      const rate = m?.cacheRate ?? null
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
      points.push({ x, y, rate, bucket: group })
      run.push(`${run.length === 0 ? "M" : "L"}${x.toFixed(2)},${y.toFixed(2)}`)
    })
    if (run.length > 1) segments.push(run.join(" "))
    return { model: series.model, color: series.color, segments, points }
  })
})

/**
 * A lone reading between two gaps produces no line segment (a path needs two
 * points), so its dot is the only mark carrying it — which is why dots render
 * in both views rather than only at hover.
 */

/** True when no model has two consecutive readings — dots alone carry the data. */
const rateHasAnyPoint = computed(() => rateLines.value.some((l) => l.points.length > 0))

function plainRectPath(x: number, y: number, w: number, h: number): string {
  return `M${x},${y} h${w} v${h} h${-w} Z`
}

/** Rounded top-left/top-right corners only; square bottom (baseline stays square per dataviz mark spec). */
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
  return roundedTopRectPath(bar.x, bar.y, bar.width, bar.height, 4)
}

function formatBucketLabel(date: Date): string {
  if (days.value === 1) return date.toLocaleTimeString(undefined, { hour: "numeric" })
  return date.toLocaleDateString(undefined, { month: "short", day: "numeric" })
}

function formatBucketFull(date: Date): string {
  if (days.value === 1) {
    return date.toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
    })
  }
  return date.toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric" })
}

/** Screen-reader summary of one time bucket, per active view. */
function bucketAriaLabel(group: GroupGeom): string {
  const when = formatBucketFull(group.date)
  if (chartView.value === "cache-rate") {
    const parts = group.models
      .map((m) =>
        m.cacheRate == null
          ? `${m.model}: no cache data`
          : `${m.model}: ${formatPercent1(m.cacheRate)}`,
      )
      .join(", ")
    return `${when}: ${parts}`
  }
  const active = group.models.filter((m) => m.total > 0)
  if (!active.length) return `${when}: no tokens`
  const parts = active
    .map((m) => `${m.model} ${formatCompactNumber(m.total)} tokens`)
    .join(", ")
  return `${when}: ${parts}; ${formatCompactNumber(group.total)} total, ${formatInt(group.requests)} requests`
}

const rangeBlurb = computed(() =>
  days.value === 1 ? "Last 24 hours, hourly buckets" : `Last ${days.value} days, daily buckets`,
)
const isRateView = computed(() => chartView.value === "cache-rate")
const cardTitle = computed(() => (isRateView.value ? "Cache rate over time" : "Tokens over time"))
const chartTitle = computed(() =>
  isRateView.value
    ? `Cache hit rate per model per bucket — ${rangeBlurb.value.toLowerCase()}`
    : `Tokens per model per bucket — ${rangeBlurb.value.toLowerCase()}`,
)
const chartDesc = computed(() => {
  const t = totals.value
  if (!t) return ""
  const modelCount = modelSeries.value.length
  if (isRateView.value) {
    return (
      `Line chart of cache hit rate per bucket, one line per model (${modelCount} shown), on a 0 to 100 percent axis. ` +
      `${formatPercent1(t.cache_rate)} cache hit rate over the period. Buckets where no request reported cache data ` +
      `are gaps in the line, not zeros. Use the table view for exact per-bucket values.`
    )
  }
  return (
    `Grouped column chart of tokens per bucket, one column per model (${modelCount} shown). ` +
    `${formatCompactNumber(t.prompt_tokens + t.completion_tokens)} tokens total, ` +
    `${formatPercent1(t.cache_rate)} cache hit rate over the period. Use the table view for exact per-bucket values.`
  )
})

const hoveredBucket = computed<GroupGeom | null>(() =>
  hoveredIndex.value != null ? (groupGeom.value[hoveredIndex.value] ?? null) : null,
)
const tooltipLeftPct = computed(() => {
  const b = hoveredBucket.value
  if (!b) return 50
  const centerX = b.hitX + b.hitWidth / 2
  // Clamped so a min-width:168px tooltip (translateX(-50%)) never overflows
  // .chart-wrap even at its narrowest supported render width (480px, see
  // chartMinWidth): half-width 84px / 480px ~= 17.5%, rounded up to 18%.
  return Math.min(82, Math.max(18, (centerX / CHART_VBW) * 100))
})

/**
 * Tooltip body for the hovered bucket. Only models with something to say are
 * listed — a range with six models would otherwise print four "—" rows in
 * every quiet bucket.
 */
const tooltipRows = computed<{ model: string; color: string; value: string }[]>(() => {
  const b = hoveredBucket.value
  if (!b) return []
  if (isRateView.value) {
    return b.models
      .filter((m) => m.cacheRate != null)
      .map((m) => ({ model: m.model, color: m.color, value: formatPercent1(m.cacheRate) }))
  }
  return b.models
    .filter((m) => m.total > 0)
    .map((m) => ({ model: m.model, color: m.color, value: formatCompactNumber(m.total) }))
})

// ---------------------------------------------------------------------------
// Per-model table
// ---------------------------------------------------------------------------
const modelRows = computed(() => summary.value?.models ?? [])

function modelCoverageText(m: ModelUsageRow): string | null {
  if (m.cache_known_requests < m.requests) {
    return `cache data on ${formatInt(m.cache_known_requests)} of ${formatInt(m.requests)} requests`
  }
  return null
}
</script>

<template>
  <div>
    <div class="page-header">
      <div>
        <h1 class="page-title">Dashboard</h1>
        <p class="page-sub">
          Token usage and cache hit rate over your API requests.
          <span v-if="fromCache" class="faint"> · showing cache</span>
          <span v-if="refreshing" class="faint"> · refreshing…</span>
        </p>
      </div>
      <div class="row-gap">
        <div class="segmented" role="group" aria-label="Time range">
          <button
            v-for="opt in RANGE_OPTIONS"
            :key="opt.value"
            type="button"
            class="segmented-btn"
            :class="{ active: days === opt.value }"
            :aria-pressed="days === opt.value"
            @click="setDays(opt.value)"
          >
            {{ opt.label }}
          </button>
        </div>
        <button
          type="button"
          class="btn btn-secondary"
          :disabled="loading || refreshing"
          @click="onManualRefresh"
        >
          Refresh
        </button>
      </div>
    </div>

    <div v-if="error" class="banner warn" style="margin-bottom: 16px">
      {{ error }}
      <span v-if="summary"> — showing cached data</span>
    </div>

    <div v-if="loading && !summary" class="empty">Loading…</div>

    <template v-else-if="summary">
      <div v-if="isEmpty" class="empty">No requests in the selected range.</div>

      <template v-else>
        <div class="stat-grid">
          <div class="card card-pad stat-tile">
            <div class="stat-label">Requests</div>
            <div class="stat-value">{{ requestsDisplay }}</div>
          </div>

          <div class="card card-pad stat-tile">
            <div class="stat-label">Total tokens</div>
            <div class="stat-value">{{ totalTokensDisplay }}</div>
          </div>

          <div class="card card-pad stat-tile hero">
            <div class="stat-label">Cache hit rate</div>
            <div class="stat-value hero-value">{{ cacheRateDisplay }}</div>
            <div v-if="totals?.cache_rate != null" class="cache-meter">
              <div class="cache-meter-fill" :style="{ width: cacheMeterPct + '%' }" />
            </div>
            <div v-if="heroCoverageText" class="faint stat-sub">{{ heroCoverageText }}</div>
          </div>

          <div class="card card-pad stat-tile">
            <div class="stat-label">Errors</div>
            <div class="stat-value">{{ errorsDisplay }}</div>
          </div>

          <div class="card card-pad stat-tile">
            <div class="stat-label">Avg latency</div>
            <div class="stat-value">{{ avgLatencyDisplay }}</div>
          </div>
        </div>

        <div class="card provider-card">
          <div class="provider-head">
            <div>
              <h2 class="provider-title">{{ cardTitle }}</h2>
              <p class="provider-blurb">{{ rangeBlurb }}</p>
            </div>
            <div class="row-gap">
              <div class="segmented" role="group" aria-label="Chart">
                <button
                  v-for="opt in CHART_VIEW_OPTIONS"
                  :key="opt.value"
                  type="button"
                  class="segmented-btn"
                  :class="{ active: chartView === opt.value }"
                  :aria-pressed="chartView === opt.value"
                  @click="setChartView(opt.value)"
                >
                  {{ opt.label }}
                </button>
              </div>
              <button type="button" class="btn btn-ghost btn-sm" @click="showTable = !showTable">
                {{ showTable ? "Show chart" : "Show as table" }}
              </button>
            </div>
          </div>

          <div class="provider-body">
            <template v-if="!showTable">
              <div class="chart-scroll">
                <div class="chart-wrap" :style="{ width: `max(100%, ${chartMinWidth}px)` }">
                  <div
                    v-if="hoveredBucket"
                    class="chart-tooltip"
                    :style="{ left: tooltipLeftPct + '%' }"
                  >
                    <div class="chart-tooltip-title">{{ formatBucketFull(hoveredBucket.date) }}</div>
                    <div
                      v-for="rowItem in tooltipRows"
                      :key="rowItem.model"
                      class="chart-tooltip-line"
                    >
                      <span class="tt-key" :style="{ background: rowItem.color }" />
                      <span>{{ rowItem.model }}</span>
                      <strong>{{ rowItem.value }}</strong>
                    </div>
                    <div v-if="!tooltipRows.length" class="faint chart-tooltip-requests">
                      {{ isRateView ? "No cache data in this bucket" : "No tokens in this bucket" }}
                    </div>
                    <template v-if="!isRateView && tooltipRows.length">
                      <div class="chart-tooltip-line chart-tooltip-total">
                        <span>Total</span>
                        <strong>{{ formatCompactNumber(hoveredBucket.total) }}</strong>
                      </div>
                      <div class="faint chart-tooltip-requests">
                        {{ formatCompactNumber(hoveredBucket.cached) }} cached ·
                        {{ formatCompactNumber(hoveredBucket.completion) }} completion ·
                        {{ formatInt(hoveredBucket.requests) }}
                        request{{ hoveredBucket.requests === 1 ? "" : "s" }}
                      </div>
                    </template>
                  </div>

                  <svg
                    :viewBox="`0 0 ${CHART_VBW} ${CHART_VBH}`"
                    class="chart-svg"
                    preserveAspectRatio="xMidYMid meet"
                  >
                    <title>{{ chartTitle }}</title>
                    <desc>{{ chartDesc }}</desc>

                    <template v-if="!isRateView">
                      <line
                        v-for="t in yTicks"
                        :key="`grid-${t}`"
                        :x1="MARGIN.left"
                        :x2="CHART_VBW - MARGIN.right"
                        :y1="yFor(t)"
                        :y2="yFor(t)"
                        class="chart-grid"
                      />
                      <text
                        v-for="t in yTicks"
                        :key="`ylabel-${t}`"
                        :x="MARGIN.left - 8"
                        :y="yFor(t)"
                        class="chart-axis-label"
                        text-anchor="end"
                        dominant-baseline="middle"
                      >{{ formatCompactNumber(t) }}</text>
                    </template>
                    <template v-else>
                      <line
                        v-for="t in RATE_TICKS"
                        :key="`rgrid-${t}`"
                        :x1="MARGIN.left"
                        :x2="CHART_VBW - MARGIN.right"
                        :y1="rateY(t)"
                        :y2="rateY(t)"
                        class="chart-grid"
                      />
                      <text
                        v-for="t in RATE_TICKS"
                        :key="`rylabel-${t}`"
                        :x="MARGIN.left - 8"
                        :y="rateY(t)"
                        class="chart-axis-label"
                        text-anchor="end"
                        dominant-baseline="middle"
                      >{{ Math.round(t * 100) }}%</text>
                    </template>

                    <!-- Grouped bars: one per model per bucket -->
                    <g v-if="!isRateView">
                      <g v-for="(group, i) in groupGeom" :key="group.key">
                        <rect
                          v-if="hoveredIndex === i"
                          :x="group.hitX"
                          :y="MARGIN.top"
                          :width="group.hitWidth"
                          :height="innerH"
                          class="bar-hit-highlight"
                        />
                        <g class="bar-segs" :class="{ hovered: hoveredIndex === i }">
                          <path
                            v-for="bar in group.bars"
                            :key="bar.model"
                            :d="barPath(bar)"
                            :fill="bar.color"
                          />
                        </g>
                      </g>
                    </g>

                    <!-- Cache-rate curve: one line per model; gaps stay gaps -->
                    <g v-else>
                      <rect
                        v-if="hoveredBucket"
                        :x="hoveredBucket.hitX"
                        :y="MARGIN.top"
                        :width="hoveredBucket.hitWidth"
                        :height="innerH"
                        class="bar-hit-highlight"
                      />
                      <g v-for="line in rateLines" :key="`line-${line.model}`">
                        <path
                          v-for="(d, si) in line.segments"
                          :key="si"
                          :d="d"
                          class="rate-line"
                          :stroke="line.color"
                        />
                        <circle
                          v-for="(pt, pi) in line.points"
                          :key="`dot-${pi}`"
                          :cx="pt.x"
                          :cy="pt.y"
                          :r="hoveredIndex != null && groupGeom[hoveredIndex]?.key === pt.bucket.key ? 4 : 2.5"
                          class="rate-dot"
                          :fill="line.color"
                        />
                      </g>
                    </g>

                    <!-- Hit targets last so they stay above the marks -->
                    <rect
                      v-for="(group, i) in groupGeom"
                      :key="`hit-${group.key}`"
                      :x="group.hitX"
                      :y="MARGIN.top"
                      :width="group.hitWidth"
                      :height="innerH"
                      class="bar-hit"
                      tabindex="0"
                      role="group"
                      :aria-label="bucketAriaLabel(group)"
                      @pointerenter="hoveredIndex = i"
                      @pointerleave="hoveredIndex = null"
                      @focus="hoveredIndex = i"
                      @blur="hoveredIndex = null"
                    />

                    <line
                      :x1="MARGIN.left"
                      :x2="CHART_VBW - MARGIN.right"
                      :y1="MARGIN.top + innerH"
                      :y2="MARGIN.top + innerH"
                      class="chart-baseline"
                    />
                    <template v-for="(group, i) in groupGeom" :key="`xlabel-${group.key}`">
                      <text
                        v-if="showXLabel(i)"
                        :x="group.hitX + group.hitWidth / 2"
                        :y="MARGIN.top + innerH + 18"
                        class="chart-axis-label"
                        text-anchor="middle"
                      >{{ formatBucketLabel(group.date) }}</text>
                    </template>
                  </svg>
                </div>
              </div>

              <p v-if="isRateView && !rateHasAnyPoint" class="faint chart-note">
                No request in this range reported cache data, so there is no rate to plot.
              </p>

              <div class="chart-legend">
                <span v-for="s in modelSeries" :key="s.model" class="legend-item">
                  <span class="legend-swatch" :style="{ background: s.color }" />
                  <span class="mono">{{ s.model }}</span>
                </span>
              </div>
            </template>

            <template v-else>
              <div class="table-scroll">
                <table class="key-table">
                  <caption class="sr-only">
                    {{ isRateView
                      ? "Cache hit rate per model per bucket for the selected range"
                      : "Tokens per model per bucket for the selected range" }}
                  </caption>
                  <thead>
                    <tr v-if="isRateView">
                      <th scope="col">Time</th>
                      <th v-for="s in modelSeries" :key="s.model" scope="col">{{ s.model }}</th>
                    </tr>
                    <tr v-else>
                      <th scope="col">Time</th>
                      <th v-for="s in modelSeries" :key="s.model" scope="col">{{ s.model }}</th>
                      <th scope="col">Total</th>
                      <th scope="col">Requests</th>
                    </tr>
                  </thead>
                  <tbody>
                    <tr v-for="group in groupGeom" :key="group.key">
                      <td>{{ formatBucketFull(group.date) }}</td>
                      <template v-if="isRateView">
                        <td v-for="m in group.models" :key="m.model" class="tabular">
                          {{ formatPercent1(m.cacheRate) }}
                        </td>
                      </template>
                      <template v-else>
                        <td v-for="m in group.models" :key="m.model" class="tabular">
                          {{ formatCompactNumber(m.total) }}
                        </td>
                        <td class="tabular">{{ formatCompactNumber(group.total) }}</td>
                        <td class="tabular">{{ formatInt(group.requests) }}</td>
                      </template>
                    </tr>
                  </tbody>
                </table>
              </div>
            </template>
          </div>
        </div>

        <div class="card provider-card" style="margin-top: 20px">
          <div class="provider-head">
            <div>
              <h2 class="provider-title">Per-model breakdown</h2>
              <p class="provider-blurb">Sorted by total tokens, highest first.</p>
            </div>
          </div>
          <div class="table-scroll">
            <table class="key-table">
              <thead>
                <tr>
                  <th scope="col">Model</th>
                  <th scope="col">Requests</th>
                  <th scope="col">Prompt</th>
                  <th scope="col">Cached read</th>
                  <th scope="col">Cache write</th>
                  <th scope="col">Completion</th>
                  <th scope="col">Cache rate</th>
                </tr>
              </thead>
              <tbody>
                <tr v-if="!modelRows.length">
                  <td colspan="7" class="empty">No per-model data.</td>
                </tr>
                <tr v-for="m in modelRows" :key="m.model">
                  <td class="mono">{{ m.model }}</td>
                  <td class="tabular">{{ formatInt(m.requests) }}</td>
                  <td class="tabular">{{ formatCompactNumber(m.prompt_tokens) }}</td>
                  <td class="tabular">{{ formatCompactNumber(m.cache_read_input_tokens) }}</td>
                  <td class="tabular">{{ formatCompactNumber(m.cache_creation_input_tokens) }}</td>
                  <td class="tabular">{{ formatCompactNumber(m.completion_tokens) }}</td>
                  <td>
                    <div class="cache-rate-cell">
                      <div class="cache-meter">
                        <div class="cache-meter-fill" :style="{ width: meterPct(m.cache_rate) + '%' }" />
                      </div>
                      <span class="tabular">{{ formatPercent1(m.cache_rate) }}</span>
                    </div>
                    <div v-if="modelCoverageText(m)" class="faint coverage-note">
                      {{ modelCoverageText(m) }}
                    </div>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        </div>
      </template>
    </template>
  </div>
</template>

<style scoped>
.sr-only {
  position: absolute;
  width: 1px;
  height: 1px;
  padding: 0;
  margin: -1px;
  overflow: hidden;
  clip: rect(0, 0, 0, 0);
  white-space: nowrap;
  border: 0;
}

/* Range picker */
.segmented {
  display: inline-flex;
  gap: 2px;
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-sm);
  background: var(--surface-2);
  padding: 2px;
}
.segmented-btn {
  border: none;
  background: transparent;
  color: var(--text-secondary);
  font-size: 12.5px;
  font-weight: 500;
  padding: 5px 12px;
  border-radius: calc(var(--radius-sm) - 2px);
  cursor: pointer;
}
.segmented-btn:hover:not(.active) {
  background: var(--hover);
  color: var(--text);
}
.segmented-btn.active {
  background: var(--accent);
  color: var(--accent-fg);
}

/* Stat tiles */
.stat-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
  gap: 12px;
  margin-bottom: 20px;
}
.stat-tile {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.stat-label {
  font-size: 12px;
  color: var(--muted);
  font-weight: 500;
}
.stat-value {
  font-size: 26px;
  font-weight: 650;
  letter-spacing: -0.02em;
}
.stat-tile.hero {
  grid-column: span 2;
}
.hero-value {
  font-size: 42px;
}
.stat-sub {
  font-size: 11.5px;
  margin-top: 2px;
}
@media (max-width: 560px) {
  .stat-tile.hero {
    grid-column: span 1;
  }
}

/* Cache-rate meter — neutral/positive magnitude, not a severity ramp: a
   higher fill is not "worse", so this deliberately does not reuse
   .usage-fill's amber/red escalation. */
.cache-meter {
  height: 6px;
  border-radius: 999px;
  background: var(--bar-track);
  overflow: hidden;
  margin-top: 8px;
}
.cache-meter-fill {
  height: 100%;
  border-radius: 999px;
  background: var(--chart-input);
  transition: width 0.25s ease;
}
.cache-rate-cell {
  display: flex;
  align-items: center;
  gap: 8px;
}
.cache-rate-cell .cache-meter {
  width: 64px;
  flex-shrink: 0;
  margin-top: 0;
}
.coverage-note {
  font-size: 11px;
  margin-top: 3px;
}

/* Chart */
.chart-scroll {
  overflow-x: auto;
}
.chart-wrap {
  position: relative;
}
/* Anchored inside .chart-wrap (never escapes the card's own box) — a
   floating tooltip positioned outside this element would get clipped by
   the ancestor .provider-card's `overflow: hidden`. Top-anchored so it
   stays clear of the x-axis labels; it may overlap a tall bar, same as
   most floating chart tooltips. */
.chart-tooltip {
  position: absolute;
  top: 4px;
  left: 50%;
  transform: translateX(-50%);
  z-index: 1;
  min-width: 168px;
  /* Model ids are long ("claude-code/claude-sonnet-5"); without a ceiling the
     tooltip either stretches past the card or wraps every row onto three
     lines. The name column ellipsizes instead — the full id is one row above
     in the legend and spelled out in the table view. */
  max-width: 320px;
  background: var(--surface);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-sm);
  box-shadow: var(--shadow-lg);
  padding: 8px 10px;
  font-size: 12px;
  pointer-events: none;
}
.chart-tooltip-title {
  font-weight: 600;
  margin-bottom: 4px;
}
.chart-tooltip-line {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 1px 0;
}
.chart-tooltip-line span:nth-child(2) {
  color: var(--text-secondary);
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.chart-tooltip-line strong {
  font-variant-numeric: tabular-nums;
}
.chart-tooltip-total {
  border-top: 1px solid var(--border);
  margin-top: 4px;
  padding-top: 4px;
  font-weight: 600;
}
.chart-tooltip-total span:first-child {
  flex: 1;
}
.chart-tooltip-requests {
  margin-top: 2px;
}
.tt-key {
  width: 8px;
  height: 8px;
  border-radius: 2px;
  flex-shrink: 0;
}

.chart-svg {
  width: 100%;
  height: auto;
  display: block;
}
.chart-grid {
  stroke: var(--border);
  stroke-width: 1;
}
.chart-baseline {
  stroke: var(--border-strong);
  stroke-width: 1;
}
.chart-axis-label {
  fill: var(--muted);
  font-size: 10px;
}
.bar-segs path,
.bar-hit-highlight,
.rate-line,
.rate-dot {
  pointer-events: none;
}
.bar-segs.hovered path {
  filter: brightness(1.12);
}
/* 2px line, round join/cap per the dataviz mark spec; the surface-colored
   ring keeps a dot legible where two models' curves cross. */
.rate-line {
  fill: none;
  stroke-width: 2;
  stroke-linejoin: round;
  stroke-linecap: round;
}
.rate-dot {
  stroke: var(--surface);
  stroke-width: 2;
  transition: r 0.1s ease;
}
.chart-note {
  font-size: 12px;
  margin: 10px 0 0;
}
.bar-hit-highlight {
  fill: var(--hover);
}
.bar-hit {
  fill: transparent;
  cursor: pointer;
}
.bar-hit:focus-visible {
  outline: 2px solid var(--ring-border);
  outline-offset: -2px;
}

.chart-legend {
  display: flex;
  flex-wrap: wrap;
  gap: 16px;
  margin-top: 12px;
  padding-top: 12px;
  border-top: 1px solid var(--border);
}
.legend-item {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 12.5px;
  color: var(--text-secondary);
}
.legend-swatch {
  width: 10px;
  height: 10px;
  border-radius: 3px;
  flex-shrink: 0;
}

/* Tables — .key-table is shared with KeysPage, which hides its 3rd/4th
   column below 720px/480px (see styles.css). That column-index rule isn't
   meaningful for these two tables' own columns (it would drop "Cached
   input"/"Completion" or "Prompt"/"Cached read" instead of KeysPage's
   "Created"/"Last used"), so restore all columns here; .table-scroll
   already gives narrow viewports a horizontal-scroll fallback instead. */
.tabular {
  font-variant-numeric: tabular-nums;
}
@media (max-width: 720px) {
  .key-table th:nth-child(3),
  .key-table td:nth-child(3),
  .key-table th:nth-child(4),
  .key-table td:nth-child(4) {
    display: table-cell;
  }
}

@media (max-width: 720px) {
  .stat-value {
    font-size: 22px;
  }
  .hero-value {
    font-size: 34px;
  }
}
</style>
