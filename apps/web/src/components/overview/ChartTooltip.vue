<!--
  The hovered / focused bucket's readout.

  Anchored inside the chart's own scrolling wrap rather than the page: a
  floating layer positioned outside it would be clipped by the card, and one
  positioned against the viewport would drift the moment the plot scrolls
  horizontally. `--tip-x` is the bucket's centre in the plot's own pixel
  space, so the tooltip tracks its bucket through a scroll for free.

  Every string arrives already translated and already formatted — this is a
  presentational shell, not a place copy or `Intl` calls live.
-->

<!-- A plain block, not a second `<script setup>`: only one setup block is
     compiled, and a type exported from a second one is silently dropped. -->
<script lang="ts">
export type TooltipRow = {
  /** Series id — stable across buckets, unlike the display label. */
  key: string
  label: string
  color: string
  value: string
}
</script>

<script setup lang="ts">
import { ref } from "vue"

defineProps<{
  /** Bucket centre in plot pixels; the tooltip centres itself on it. */
  x: number
  title: string
  rows: TooltipRow[]
  /** Bucket total — tokens view only. */
  total?: { label: string; value: string } | null
  /** Secondary lines under the total (cached / output / requests). */
  footer?: string[]
  /** Shown instead of the rows when the bucket has nothing to report. */
  empty?: string | null
}>()

/**
 * Exposed so BarChart can measure the rendered tooltip width and clamp it
 * inside the plot's bounds — the width is content-driven (128–320px, any
 * number of rows), so the parent re-clamps via a ResizeObserver as the
 * hovered bucket's content changes. Named `rootEl` rather than `el` because
 * `el` is the conventional local in the parent's setup for an unrelated
 * element.
 */
const rootEl = ref<HTMLElement | null>(null)
defineExpose({ rootEl })
</script>

<template>
  <div ref="rootEl" class="tooltip" :style="{ '--tip-x': `${x}px` }">
    <p class="tooltip-title">{{ title }}</p>

    <p v-if="!rows.length && empty" class="tooltip-empty">{{ empty }}</p>

    <div v-for="row in rows" :key="row.key" class="tooltip-row">
      <span class="swatch" :style="{ '--swatch': row.color }" />
      <span class="tooltip-label">{{ row.label }}</span>
      <span class="tooltip-value tabular">{{ row.value }}</span>
    </div>

    <div v-if="total" class="tooltip-row tooltip-total">
      <span class="tooltip-label">{{ total.label }}</span>
      <span class="tooltip-value tabular">{{ total.value }}</span>
    </div>

    <div v-if="footer?.length" class="tooltip-footer">
      <span v-for="line in footer" :key="line">{{ line }}</span>
    </div>
  </div>
</template>

<style scoped>
.tooltip {
  position: absolute;
  top: var(--space-1);
  left: var(--tip-x);
  z-index: 1;
  transform: translateX(-50%);
  min-width: 128px;
  /* Model ids run long ("claude-code/claude-sonnet-5"); without a ceiling the
     tooltip either outgrows the card or wraps every row onto three lines. The
     label column ellipsizes instead — the full id is in the legend below and
     spelled out in the table view. */
  max-width: 320px;
  padding: var(--space-2);
  background: var(--surface);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-sm);
  box-shadow: var(--shadow-lg);
  font-size: var(--text-xs);
  pointer-events: none;
}

.tooltip-title {
  margin: 0 0 var(--space-1);
  font-weight: var(--weight-semibold);
}

.tooltip-empty {
  margin: 0;
  color: var(--muted);
}

/* Label left, value right — the readout scans as a column of numbers rather
   than as ragged sentences. */
.tooltip-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-2);
  padding: 1px 0;
}

.swatch {
  width: 8px;
  height: 8px;
  flex-shrink: 0;
  border-radius: 2px;
  background: var(--swatch);
}

.tooltip-label {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text-secondary);
}

.tooltip-value {
  flex-shrink: 0;
  font-weight: var(--weight-medium);
}

.tooltip-total {
  margin-top: var(--space-1);
  padding-top: var(--space-1);
  border-top: 1px solid var(--border);
  font-weight: var(--weight-semibold);
}

.tooltip-total .tooltip-label {
  color: var(--text);
}

.tooltip-footer {
  display: flex;
  flex-direction: column;
  margin-top: var(--space-1);
  color: var(--faint);
  font-size: var(--text-2xs);
}
</style>
