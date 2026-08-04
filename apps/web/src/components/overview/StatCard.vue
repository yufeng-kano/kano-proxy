<script setup lang="ts">
/**
 * One Overview metric card (docs/admin-ui.md § Overview page): the range
 * total as the headline, a mini stacked chart of the metric over time, and
 * the top models by that metric with their values. The expand control opens
 * the metric's detail modal — full chart plus the Min/Max/Avg/Sum table —
 * which is also where every number in the mini chart is reachable as text.
 */
import ActionIcon from "@/components/ui/ActionIcon.vue"
import AppButton from "@/components/ui/AppButton.vue"
import { useI18n } from "@/i18n"
import BarChart from "./BarChart.vue"
import type { MetricSeries } from "./series"

defineProps<{
  title: string
  series: MetricSeries
  formatValue: (n: number) => string
  /**
   * Overrides the computed headline — Spend passes "—" when nothing in the
   * range is priced, where a formatted 0 would read as a real $0.00.
   */
  headline?: string | null
  /** Note under the headline — spend's estimate/coverage caveat. */
  note?: string | null
  loading?: boolean
}>()

const emit = defineEmits<{ expand: [] }>()

const { t } = useI18n()

/** Top 4 named models; everything beyond folds into one visual "Others" line. */
const LIST_ROWS = 4

function listRows(series: MetricSeries) {
  const rows = series.models.filter((m) => m.total > 0)
  if (rows.length <= LIST_ROWS + 1) return { named: rows, othersTotal: null }
  const named = rows.slice(0, LIST_ROWS)
  const othersTotal = rows.slice(LIST_ROWS).reduce((sum, m) => sum + m.total, 0)
  return { named, othersTotal }
}
</script>

<template>
  <section class="stat-card" :aria-label="title">
    <header class="head">
      <div class="head-text">
        <h2 class="title">{{ title }}</h2>
        <p v-if="loading" class="value-skeleton" aria-hidden="true"></p>
        <p v-else class="value tabular">{{ headline ?? formatValue(series.total) }}</p>
        <p v-if="note && !loading" class="note">{{ note }}</p>
      </div>
      <AppButton
        icon-only
        size="sm"
        variant="ghost"
        :label="t('overview.card.expand', { metric: title })"
        @click="emit('expand')"
      >
        <template #icon><ActionIcon name="expand" /></template>
      </AppButton>
    </header>

    <div class="plot">
      <BarChart v-if="!loading" mini :buckets="series.buckets" :format-value="formatValue" />
    </div>

    <ul class="models" :aria-label="t('overview.card.topModels', { metric: title })">
      <template v-if="!loading">
        <li
          v-for="m in listRows(series).named"
          :key="m.key"
          class="model-row"
        >
          <span class="swatch" :style="{ '--swatch': m.color }" aria-hidden="true" />
          <span class="model-label" :title="m.label">{{ m.label }}</span>
          <span class="model-value tabular">{{ formatValue(m.total) }}</span>
        </li>
        <li v-if="listRows(series).othersTotal != null" class="model-row">
          <span class="swatch others" aria-hidden="true" />
          <span class="model-label">{{ t("overview.card.others") }}</span>
          <span class="model-value tabular">{{ formatValue(listRows(series).othersTotal!) }}</span>
        </li>
        <li v-if="!series.models.some((m) => m.total > 0)" class="model-row empty">
          {{ t("overview.card.noActivity") }}
        </li>
      </template>
      <template v-else>
        <li v-for="i in 3" :key="i" class="model-row" aria-hidden="true">
          <span class="row-skeleton" />
        </li>
      </template>
    </ul>
  </section>
</template>

<style scoped>
.stat-card {
  display: flex;
  flex-direction: column;
  min-width: 0;
  padding: var(--space-4) var(--space-5);
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  box-shadow: var(--shadow);
}

.head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--space-2);
}

.head-text {
  min-width: 0;
}

.title {
  margin: 0;
  font-size: var(--text-xs);
  font-weight: var(--weight-medium);
  color: var(--muted);
}

.value {
  margin: var(--space-1) 0 0;
  font-size: var(--text-xl);
  font-weight: var(--weight-semibold);
  letter-spacing: var(--tracking-tighter);
  line-height: 1.15;
}

.note {
  margin: var(--space-1) 0 0;
  color: var(--faint);
  font-size: var(--text-2xs);
}

.value-skeleton {
  width: 90px;
  height: calc(var(--text-xl) * 1.15);
  margin: var(--space-1) 0 0;
  border-radius: var(--radius-xs);
  background: var(--surface-2);
}

/* Fixed slot: the card is the same height empty, loading, and full, so the
   grid never reflows when a metric lands. */
.plot {
  height: 88px;
  margin-top: var(--space-2);
}

.models {
  display: flex;
  flex-direction: column;
  gap: var(--space-1);
  margin: var(--space-3) 0 0;
  padding: var(--space-3) 0 0;
  border-top: 1px solid var(--border);
  list-style: none;
}

.model-row {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  min-height: 22px;
  font-size: var(--text-xs);
}

.model-row.empty {
  color: var(--faint);
}

.swatch {
  width: 8px;
  height: 8px;
  flex-shrink: 0;
  border-radius: var(--radius-full);
  background: var(--swatch);
}

.swatch.others {
  background: var(--series-other);
}

.model-label {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text-secondary);
}

.model-value {
  flex-shrink: 0;
  color: var(--text);
  font-weight: var(--weight-medium);
}

.row-skeleton {
  display: block;
  width: 100%;
  height: var(--text-xs);
  border-radius: var(--radius-full);
  background: var(--hover);
}
</style>
