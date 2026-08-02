<script setup lang="ts">
import { computed, onMounted, ref } from "vue"
import { useAuth } from "@/composables/useAuth"
import { useUsage } from "@/composables/useUsage"
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

const RANGE_OPTIONS: { value: UsageDays; label: string }[] = [
  { value: 1, label: "24h" },
  { value: 7, label: "7d" },
  { value: 30, label: "30d" },
]

// ---------------------------------------------------------------------------
// Chart geometry — internal SVG coordinate system (viewBox units, not px).
// ---------------------------------------------------------------------------
const CHART_VBW = 960
const CHART_VBH = 300
const MARGIN = { top: 16, right: 16, bottom: 34, left: 52 }
const innerW = CHART_VBW - MARGIN.left - MARGIN.right
const innerH = CHART_VBH - MARGIN.top - MARGIN.bottom
const BAR_GAP = 4
const MAX_BAR_W = 24
const SEG_GAP = 2

type ChartBucket = {
  key: string
  date: Date
  /** prompt_tokens - cache_read_input_tokens, floored at 0. */
  uncached: number
  cached: number
  completion: number
  requests: number
}

type SegRect = { y: number; h: number; roundedTop: boolean }

type BarGeom = ChartBucket & {
  /** Full slot (for the hover/focus hit target — wider than the visual bar). */
  hitX: number
  hitWidth: number
  barX: number
  barWidth: number
  /** Bottom-to-top: [uncached, cached, completion]. */
  segs: SegRect[]
  total: number
}

const showTable = ref(false)
const hoveredIndex = ref<number | null>(null)

onMounted(async () => {
  setUserId(user.value?.id ?? null)
  await refresh()
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

  const byKey = new Map(s.series.map((p) => [p.bucket, p]))
  const out: ChartBucket[] = []
  for (let i = 0; i < bucketCount; i++) {
    const date = new Date(start.getTime() + i * stepMs)
    const point = byKey.get(bucketKeyFor(s.days, date))
    const prompt = point?.prompt_tokens ?? 0
    const cachedRead = point?.cache_read_input_tokens ?? 0
    out.push({
      key: bucketKeyFor(s.days, date),
      date,
      uncached: Math.max(0, prompt - cachedRead),
      cached: cachedRead,
      completion: point?.completion_tokens ?? 0,
      requests: point?.requests ?? 0,
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

const chartMaxRaw = computed(() =>
  buckets.value.reduce((m, b) => Math.max(m, b.uncached + b.cached + b.completion), 0),
)
const yTicks = computed(() => buildYTicks(chartMaxRaw.value))
const chartMax = computed(() => yTicks.value[yTicks.value.length - 1] ?? 1)

/** Stack bottom -> top, skipping zero-height segments; the outermost nonzero segment gets the rounded "data end". */
function stackSegments(
  values: number[],
  scale: (v: number) => number,
  baselineY: number,
  segGap: number,
): SegRect[] {
  const heights = values.map((v) => (v > 0 ? Math.max(1.5, scale(v)) : 0))
  let lastNonZero = -1
  for (let i = heights.length - 1; i >= 0; i--) {
    if (heights[i] > 0) {
      lastNonZero = i
      break
    }
  }
  let cursorY = baselineY
  let placed = false
  const out: SegRect[] = []
  for (let i = 0; i < heights.length; i++) {
    const h = heights[i]
    if (h <= 0) {
      out.push({ y: cursorY, h: 0, roundedTop: false })
      continue
    }
    if (placed) cursorY -= segGap
    const topY = cursorY - h
    out.push({ y: topY, h, roundedTop: i === lastNonZero })
    cursorY = topY
    placed = true
  }
  return out
}

const barGeom = computed<BarGeom[]>(() => {
  const list = buckets.value
  const n = list.length
  if (!n) return []
  const slot = (innerW - BAR_GAP * (n - 1)) / n
  const barW = Math.min(MAX_BAR_W, Math.max(1, slot))
  const max = chartMax.value
  const scale = (v: number) => (max > 0 ? (v / max) * innerH : 0)
  const baselineY = MARGIN.top + innerH
  return list.map((b, i) => {
    const hitX = MARGIN.left + i * (slot + BAR_GAP)
    const barX = hitX + (slot - barW) / 2
    const segs = stackSegments([b.uncached, b.cached, b.completion], scale, baselineY, SEG_GAP)
    return {
      ...b,
      hitX,
      hitWidth: slot,
      barX,
      barWidth: barW,
      segs,
      total: b.uncached + b.cached + b.completion,
    }
  })
})

const chartMinWidth = computed(() => Math.max(480, barGeom.value.length * 16))
const xLabelStep = computed(() => Math.max(1, Math.ceil(barGeom.value.length / 8)))

function showXLabel(i: number): boolean {
  const n = barGeom.value.length
  return i % xLabelStep.value === 0 || i === n - 1
}

function yFor(t: number): number {
  const max = chartMax.value
  return MARGIN.top + innerH - (max > 0 ? (t / max) * innerH : 0)
}

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

function segPath(bar: BarGeom, idx: number): string {
  const seg = bar.segs[idx]
  if (!seg || seg.h <= 0) return ""
  return seg.roundedTop
    ? roundedTopRectPath(bar.barX, seg.y, bar.barWidth, seg.h, 4)
    : plainRectPath(bar.barX, seg.y, bar.barWidth, seg.h)
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

function bucketAriaLabel(bar: BarGeom): string {
  return (
    `${formatBucketFull(bar.date)}: ${formatCompactNumber(bar.uncached)} uncached input tokens, ` +
    `${formatCompactNumber(bar.cached)} cached input tokens, ${formatCompactNumber(bar.completion)} ` +
    `completion tokens, ${formatCompactNumber(bar.total)} total, ${formatInt(bar.requests)} requests`
  )
}

const rangeBlurb = computed(() =>
  days.value === 1 ? "Last 24 hours, hourly buckets" : `Last ${days.value} days, daily buckets`,
)
const chartTitle = computed(() => `Tokens per bucket — ${rangeBlurb.value.toLowerCase()}`)
const chartDesc = computed(() => {
  const t = totals.value
  if (!t) return ""
  return (
    `Stacked bar chart of uncached input, cached input, and completion tokens per bucket. ` +
    `${formatCompactNumber(t.prompt_tokens + t.completion_tokens)} tokens total, ` +
    `${formatPercent1(t.cache_rate)} cache hit rate over the period. Use the table view for exact per-bucket values.`
  )
})

const hoveredBucket = computed<BarGeom | null>(() =>
  hoveredIndex.value != null ? (barGeom.value[hoveredIndex.value] ?? null) : null,
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
              <h2 class="provider-title">Tokens over time</h2>
              <p class="provider-blurb">{{ rangeBlurb }}</p>
            </div>
            <button type="button" class="btn btn-ghost btn-sm" @click="showTable = !showTable">
              {{ showTable ? "Show chart" : "Show as table" }}
            </button>
          </div>

          <div class="provider-body">
            <template v-if="!showTable">
              <div class="chart-scroll">
                <div class="chart-wrap" :style="{ width: `max(100%, ${chartMinWidth}px)` }">
                  <div v-if="hoveredBucket" class="chart-tooltip" :style="{ left: tooltipLeftPct + '%' }">
                    <div class="chart-tooltip-title">{{ formatBucketFull(hoveredBucket.date) }}</div>
                    <div class="chart-tooltip-line">
                      <span class="tt-key" style="background: var(--chart-input-soft)" />
                      <span>Uncached input</span>
                      <strong>{{ formatCompactNumber(hoveredBucket.uncached) }}</strong>
                    </div>
                    <div class="chart-tooltip-line">
                      <span class="tt-key" style="background: var(--chart-input)" />
                      <span>Cached input</span>
                      <strong>{{ formatCompactNumber(hoveredBucket.cached) }}</strong>
                    </div>
                    <div class="chart-tooltip-line">
                      <span class="tt-key" style="background: var(--chart-completion)" />
                      <span>Completion</span>
                      <strong>{{ formatCompactNumber(hoveredBucket.completion) }}</strong>
                    </div>
                    <div class="chart-tooltip-line chart-tooltip-total">
                      <span>Total</span>
                      <strong>{{ formatCompactNumber(hoveredBucket.total) }}</strong>
                    </div>
                    <div class="faint chart-tooltip-requests">
                      {{ formatInt(hoveredBucket.requests) }}
                      request{{ hoveredBucket.requests === 1 ? "" : "s" }}
                    </div>
                  </div>

                  <svg
                    :viewBox="`0 0 ${CHART_VBW} ${CHART_VBH}`"
                    class="chart-svg"
                    preserveAspectRatio="xMidYMid meet"
                  >
                    <title>{{ chartTitle }}</title>
                    <desc>{{ chartDesc }}</desc>

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

                    <g v-for="(bar, i) in barGeom" :key="bar.key">
                      <rect
                        v-if="hoveredIndex === i"
                        :x="bar.hitX"
                        :y="MARGIN.top"
                        :width="bar.hitWidth"
                        :height="innerH"
                        class="bar-hit-highlight"
                      />
                      <g class="bar-segs" :class="{ hovered: hoveredIndex === i }">
                        <path v-if="bar.segs[0].h > 0" :d="segPath(bar, 0)" fill="var(--chart-input-soft)" />
                        <path v-if="bar.segs[1].h > 0" :d="segPath(bar, 1)" fill="var(--chart-input)" />
                        <path v-if="bar.segs[2].h > 0" :d="segPath(bar, 2)" fill="var(--chart-completion)" />
                      </g>
                      <rect
                        :x="bar.hitX"
                        :y="MARGIN.top"
                        :width="bar.hitWidth"
                        :height="innerH"
                        class="bar-hit"
                        tabindex="0"
                        role="group"
                        :aria-label="bucketAriaLabel(bar)"
                        @pointerenter="hoveredIndex = i"
                        @pointerleave="hoveredIndex = null"
                        @focus="hoveredIndex = i"
                        @blur="hoveredIndex = null"
                      />
                    </g>

                    <line
                      :x1="MARGIN.left"
                      :x2="CHART_VBW - MARGIN.right"
                      :y1="MARGIN.top + innerH"
                      :y2="MARGIN.top + innerH"
                      class="chart-baseline"
                    />
                    <template v-for="(bar, i) in barGeom" :key="`xlabel-${bar.key}`">
                      <text
                        v-if="showXLabel(i)"
                        :x="bar.hitX + bar.hitWidth / 2"
                        :y="MARGIN.top + innerH + 18"
                        class="chart-axis-label"
                        text-anchor="middle"
                      >{{ formatBucketLabel(bar.date) }}</text>
                    </template>
                  </svg>
                </div>
              </div>

              <div class="chart-legend">
                <span class="legend-item">
                  <span class="legend-swatch" style="background: var(--chart-input-soft)" />
                  Uncached input
                </span>
                <span class="legend-item">
                  <span class="legend-swatch" style="background: var(--chart-input)" />
                  Cached input
                </span>
                <span class="legend-item">
                  <span class="legend-swatch" style="background: var(--chart-completion)" />
                  Completion
                </span>
              </div>
            </template>

            <template v-else>
              <div class="table-scroll">
                <table class="key-table">
                  <caption class="sr-only">Tokens per bucket for the selected range</caption>
                  <thead>
                    <tr>
                      <th scope="col">Time</th>
                      <th scope="col">Uncached input</th>
                      <th scope="col">Cached input</th>
                      <th scope="col">Completion</th>
                      <th scope="col">Total</th>
                      <th scope="col">Requests</th>
                    </tr>
                  </thead>
                  <tbody>
                    <tr v-for="bar in barGeom" :key="bar.key">
                      <td>{{ formatBucketFull(bar.date) }}</td>
                      <td class="tabular">{{ formatCompactNumber(bar.uncached) }}</td>
                      <td class="tabular">{{ formatCompactNumber(bar.cached) }}</td>
                      <td class="tabular">{{ formatCompactNumber(bar.completion) }}</td>
                      <td class="tabular">{{ formatCompactNumber(bar.total) }}</td>
                      <td class="tabular">{{ formatInt(bar.requests) }}</td>
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
.bar-hit-highlight {
  pointer-events: none;
}
.bar-segs.hovered path {
  filter: brightness(1.12);
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
