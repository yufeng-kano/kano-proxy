<script setup lang="ts">
/**
 * One upstream usage window (5h, Week, …) as a labelled meter.
 *
 * `utilization` is always a percent (0–100), never a 0–1 fraction — adapters
 * normalize upstream values to that scale, so this renders it directly with no
 * rescaling heuristic.
 *
 * Unlike the dashboard's cache meter, the fill here escalates through amber to
 * red: on a quota, a higher number genuinely is worse. The percentage is
 * always printed beside the bar, so the escalation is emphasis, not the only
 * signal.
 *
 * The columns belong to the parent list (AccountCard's `.windows`), which this
 * joins as a subgrid: every bar in a list ends at the same x however wide one
 * amount is. Outside a grid it would collapse to one column.
 */
import { computed } from "vue"
import { useI18n } from "@/i18n"
import type { UsageWindow } from "@/types"

const props = defineProps<{ window: UsageWindow }>()

const { t, format } = useI18n()

const pct = computed(() => {
  const u = props.window.utilization
  if (u == null || Number.isNaN(u)) return null
  return Math.max(0, Math.min(100, Math.round(u)))
})

const level = computed(() => {
  if (pct.value == null) return ""
  if (pct.value >= 90) return "full"
  if (pct.value >= 70) return "high"
  return ""
})

const resetText = computed(() =>
  props.window.resets_at
    ? t("providers.usage.resets", { when: format.relative(props.window.resets_at) })
    : null,
)
</script>

<template>
  <div class="usage">
    <span class="usage-label">{{ window.label }}</span>
    <div
      class="track"
      role="meter"
      :aria-valuenow="pct ?? undefined"
      aria-valuemin="0"
      aria-valuemax="100"
      :aria-label="window.label"
    >
      <div class="fill" :class="level" :style="{ width: `${pct ?? 0}%` }" />
    </div>
    <span class="usage-value tabular">{{ window.value ?? format.percentValue(pct) }}</span>
    <span v-if="resetText" class="usage-reset">{{ resetText }}</span>
  </div>
</template>

<style scoped>
/* Columns and the gap between them come from the list; only the gap between
   the meter line and its reset line is this component's. */
.usage {
  display: grid;
  grid-column: 1 / -1;
  grid-template-columns: subgrid;
  align-items: center;
  row-gap: var(--space-3);
}

.usage-label {
  font-size: var(--text-xs);
  font-weight: var(--weight-medium);
  color: var(--text-secondary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.track {
  height: 5px;
  border-radius: var(--radius-full);
  background: var(--bar-track);
  overflow: hidden;
}

.fill {
  height: 100%;
  border-radius: var(--radius-full);
  background: var(--bar-fill);
  transition: width var(--duration-slow) var(--ease);
}

.fill.high {
  background: var(--bar-high);
}

.fill.full {
  background: var(--bar-full);
}

.usage-value {
  font-size: var(--text-xs);
  color: var(--text-secondary);
  text-align: right;
}

.usage-reset {
  grid-column: 2 / -1;
  margin-top: calc(var(--space-1) * -1);
  font-size: var(--text-2xs);
  color: var(--faint);
}

@media (max-width: 640px) {
  .usage {
    row-gap: var(--space-2);
  }
}
</style>
