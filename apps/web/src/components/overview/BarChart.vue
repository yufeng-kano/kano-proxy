<!--
  The Overview's one chart primitive: stacked columns per time bucket, one
  segment per series (docs/admin-ui.md § Overview page). Hand-rolled inline
  SVG, no charting dependency.

  Two modes off the same geometry:

  - full: dotted horizontal grid, nice y ticks, thinned x labels (never
    rotated), a hover/focus tooltip, and per-bucket keyboard hits. The plot
    keeps a width floor and scrolls sideways inside its wrap rather than
    crushing 31 buckets into slivers.
  - mini: axis-free spark version for the metric cards. Decorative on
    purpose (aria-hidden): every value it summarizes is reachable as text in
    the card's own model list and in the expand modal's table, so the mini
    plot carries no information of its own.

  The viewBox is measured (one unit = one CSS pixel) and the plot height is
  fixed, never an aspect ratio — the two rules the previous chart established
  and this one keeps.
-->

<script lang="ts">
export type BarSegment = {
  /** Series id — stable across buckets; also the stack and legend identity. */
  key: string
  label: string
  color: string
  value: number
}

export type BarBucket = {
  key: string
  /** Axis tick text. */
  label: string
  /** Tooltip / aria title — spelled out, unlike the tick. */
  fullLabel: string
  /** Stack order: first segment sits at the baseline. */
  segments: BarSegment[]
}
</script>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue"
import { useI18n } from "@/i18n"
import ChartTooltip, { type TooltipRow } from "./ChartTooltip.vue"

const props = withDefaults(
  defineProps<{
    buckets: BarBucket[]
    /** Formats a y value for ticks, the tooltip, and aria text. */
    formatValue: (n: number) => string
    mini?: boolean
    /** Chart title for assistive tech (full mode). */
    title?: string
    /** Tooltip total row label; omitted = no total row. */
    totalLabel?: string
    /** Tooltip body when the hovered bucket is empty. */
    emptyText?: string
    /** Legend entries under the plot (full mode). */
    legend?: { key: string; label: string; color: string }[]
  }>(),
  { mini: false },
)

const { t } = useI18n()

// ---------------------------------------------------------------------------
// Geometry — plain numbers on purpose: coordinates, not styling.
// ---------------------------------------------------------------------------
const FULL_H = 240
const MINI_H = 88
const FULL_MARGIN = { top: 10, right: 8, bottom: 24, left: 46 }
const MINI_MARGIN = { top: 2, right: 0, bottom: 2, left: 0 }
const MAX_BAR_W = 28
const BAR_RADIUS = 3
const TOOLTIP_HALF = 84
const FALLBACK_PLOT_W = 640

const plotH = computed(() => (props.mini ? MINI_H : FULL_H))
const margin = computed(() => (props.mini ? MINI_MARGIN : FULL_MARGIN))
const innerH = computed(() => plotH.value - margin.value.top - margin.value.bottom)

const hoveredIndex = ref<number | null>(null)

// Measured viewBox — the observed element is the component root, mounted in
// both modes, so the width never goes stale.
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
 * Floor width below which full-mode groups collide; the wrap scrolls
 * sideways instead. Mini mode has no floor — a card is never wide enough to
 * scroll, and thin bars are the accepted trade.
 */
const chartMinWidth = computed(() =>
  props.mini ? 0 : Math.max(420, props.buckets.length * 13),
)

const plotW = computed(() =>
  Math.max(availableW.value || FALLBACK_PLOT_W, chartMinWidth.value),
)
const innerW = computed(() => Math.max(1, plotW.value - margin.value.left - margin.value.right))

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

const bucketTotals = computed(() =>
  props.buckets.map((b) => b.segments.reduce((sum, s) => sum + Math.max(0, s.value), 0)),
)

const maxRaw = computed(() => bucketTotals.value.reduce((m, v) => Math.max(m, v), 0))

const yTicks = computed<number[]>(() => {
  if (maxRaw.value <= 0) return [0]
  const step = niceStep(maxRaw.value / 4)
  const ticks: number[] = []
  for (let v = 0; v <= maxRaw.value + step * 0.5; v += step) ticks.push(v)
  return ticks
})

const chartMax = computed(() => yTicks.value[yTicks.value.length - 1] || 1)

function yFor(value: number): number {
  return margin.value.top + innerH.value - (chartMax.value > 0 ? (value / chartMax.value) * innerH.value : 0)
}

// ---------------------------------------------------------------------------
// Stack geometry
// ---------------------------------------------------------------------------
type StackRect = { key: string; color: string; x: number; y: number; width: number; height: number }

type Group = {
  bucket: BarBucket
  total: number
  hitX: number
  hitWidth: number
  rects: StackRect[]
}

const groups = computed<Group[]>(() => {
  const list = props.buckets
  const n = list.length
  if (!n) return []
  const slot = innerW.value / n
  const barW = Math.min(MAX_BAR_W, Math.max(1, slot * 0.62))
  const baseline = margin.value.top + innerH.value
  const max = chartMax.value

  return list.map((bucket, i) => {
    const hitX = margin.value.left + i * slot
    const x = hitX + (slot - barW) / 2
    const total = bucketTotals.value[i] ?? 0
    const rects: StackRect[] = []
    let yCursor = baseline
    for (const seg of bucket.segments) {
      const v = Math.max(0, seg.value)
      if (v <= 0) continue
      // Floor a nonzero segment at 1 device pixel so a small-but-real value
      // stays visible in the stack.
      const h = Math.max(1, max > 0 ? (v / max) * innerH.value : 0)
      yCursor -= h
      rects.push({ key: seg.key, color: seg.color, x, y: yCursor, width: barW, height: h })
    }
    return { bucket, total, hitX, hitWidth: slot, rects }
  })
})

/**
 * The stack's top corners round; inner seams stay square. The top rect is the
 * last one pushed (stacks build bottom-up).
 */
function rectPath(rect: StackRect, isTop: boolean): string {
  const { x, y, width: w, height: h } = rect
  if (!isTop) return `M${x},${y} h${w} v${h} h${-w} Z`
  const r = Math.max(0, Math.min(BAR_RADIUS, h, w / 2))
  if (r <= 0.01) return `M${x},${y} h${w} v${h} h${-w} Z`
  return [
    `M${x},${y + r}`,
    `Q${x},${y} ${x + r},${y}`,
    `H${x + w - r}`,
    `Q${x + w},${y} ${x + w},${y + r}`,
    `V${y + h}`,
    `H${x}`,
    `Z`,
  ].join(" ")
}

const xLabelStep = computed(() => Math.max(1, Math.ceil(props.buckets.length / 8)))

function showXLabel(i: number): boolean {
  return i % xLabelStep.value === 0 || i === props.buckets.length - 1
}

// ---------------------------------------------------------------------------
// Tooltip + aria (full mode)
// ---------------------------------------------------------------------------
const hoveredGroup = computed<Group | null>(() =>
  hoveredIndex.value != null ? (groups.value[hoveredIndex.value] ?? null) : null,
)

const tooltipX = computed(() => {
  const g = hoveredGroup.value
  if (!g) return plotW.value / 2
  const center = g.hitX + g.hitWidth / 2
  return Math.min(plotW.value - TOOLTIP_HALF, Math.max(TOOLTIP_HALF, center))
})

/** Largest segment first — the tooltip reads as a ranking, like the cards. */
const tooltipRows = computed<TooltipRow[]>(() => {
  const g = hoveredGroup.value
  if (!g) return []
  return [...g.bucket.segments]
    .filter((s) => s.value > 0)
    .sort((a, b) => b.value - a.value)
    .map((s) => ({ key: s.key, label: s.label, color: s.color, value: props.formatValue(s.value) }))
})

const tooltipTotal = computed(() => {
  const g = hoveredGroup.value
  if (!g || !props.totalLabel || tooltipRows.value.length < 2) return null
  return { label: props.totalLabel, value: props.formatValue(g.total) }
})

function bucketAriaLabel(group: Group): string {
  const parts = group.bucket.segments
    .filter((s) => s.value > 0)
    .map((s) => `${s.label} ${props.formatValue(s.value)}`)
  const detail = parts.length ? parts.join(", ") : t("overview.chart.bucketNoData")
  return t("overview.chart.bucketAria", { when: group.bucket.fullLabel, detail })
}
</script>

<template>
  <div ref="root" class="chart" :class="{ mini }" :aria-hidden="mini ? 'true' : undefined">
    <div class="chart-scroll">
      <div class="chart-plot" :style="{ '--plot-min': `${chartMinWidth}px`, '--plot-height': `${plotH}px` }">
        <ChartTooltip
          v-if="!mini && hoveredGroup"
          :x="tooltipX"
          :title="hoveredGroup.bucket.fullLabel"
          :rows="tooltipRows"
          :total="tooltipTotal"
          :empty="emptyText ?? t('overview.chart.emptyBucket')"
        />

        <svg
          class="chart-svg"
          :viewBox="`0 0 ${plotW} ${plotH}`"
          preserveAspectRatio="none"
          :role="mini ? undefined : 'img'"
        >
          <template v-if="!mini">
            <title>{{ title }}</title>

            <!-- Dotted horizontal grid only — no axis lines, no tick marks,
                 nothing vertical competing with the columns. -->
            <line
              v-for="tick in yTicks"
              :key="`grid-${tick}`"
              class="grid"
              :x1="margin.left"
              :x2="plotW - margin.right"
              :y1="yFor(tick)"
              :y2="yFor(tick)"
            />
            <text
              v-for="tick in yTicks"
              :key="`ylabel-${tick}`"
              class="axis-label"
              :x="margin.left - 8"
              :y="yFor(tick)"
              text-anchor="end"
              dominant-baseline="middle"
            >{{ formatValue(tick) }}</text>
          </template>

          <g v-for="(group, i) in groups" :key="group.bucket.key">
            <rect
              v-if="!mini && hoveredIndex === i"
              class="hit-highlight"
              :x="group.hitX"
              :y="margin.top"
              :width="group.hitWidth"
              :height="innerH"
            />
            <g class="stack" :class="{ hovered: hoveredIndex === i }">
              <path
                v-for="(rect, ri) in group.rects"
                :key="rect.key"
                :d="rectPath(rect, ri === group.rects.length - 1)"
                :fill="rect.color"
              />
            </g>
          </g>

          <template v-if="!mini">
            <!-- Hit targets last, above the marks. -->
            <rect
              v-for="(group, i) in groups"
              :key="`hit-${group.bucket.key}`"
              class="hit"
              :x="group.hitX"
              :y="margin.top"
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

            <template v-for="(group, i) in groups" :key="`xlabel-${group.bucket.key}`">
              <text
                v-if="showXLabel(i)"
                class="axis-label"
                :x="group.hitX + group.hitWidth / 2"
                :y="margin.top + innerH + 15"
                text-anchor="middle"
              >{{ group.bucket.label }}</text>
            </template>
          </template>
        </svg>
      </div>
    </div>

    <ul v-if="!mini && legend?.length" class="legend">
      <li v-for="s in legend" :key="s.key" class="legend-item">
        <span class="legend-swatch" :style="{ '--swatch': s.color }" />
        <span class="legend-label">{{ s.label }}</span>
      </li>
    </ul>
  </div>
</template>

<style scoped>
.chart {
  display: flex;
  flex-direction: column;
  min-width: 0;
}

/* Full mode holds its width floor and scrolls sideways rather than crushing
   a 30-day range into slivers; mini never scrolls. `min-width: 0` is what
   makes it scroll instead of grow: as a flex child it would otherwise be sized
   by the plot's floor and pass that width up to its container. */
.chart-scroll {
  min-width: 0;
  overflow-x: auto;
  overflow-y: hidden;
}

.mini .chart-scroll {
  overflow: hidden;
}

.chart-plot {
  position: relative;
  min-width: var(--plot-min);
  height: var(--plot-height);
}

.chart-svg {
  display: block;
  width: 100%;
  height: 100%;
}

/* Dotted, not solid: the grid is a reading aid, not a frame. */
.grid {
  stroke: var(--border);
  stroke-width: 1;
  stroke-dasharray: 1 4;
  stroke-linecap: round;
}

.axis-label {
  fill: var(--muted);
  font-size: var(--text-2xs);
}

.stack path,
.hit-highlight {
  pointer-events: none;
}

.stack.hovered path {
  filter: brightness(1.1);
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

.legend-label {
  overflow-wrap: anywhere;
}
</style>
