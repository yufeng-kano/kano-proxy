<script setup lang="ts">
/**
 * A metric card's expanded view (docs/admin-ui.md § Overview page): the
 * full-size stacked chart over the range, then per-model Min / Max / Avg /
 * Sum across the range's buckets — the table twin that keeps every charted
 * value reachable as text. Sorted by Sum desc, same order as the stacks.
 */
import { computed } from "vue"
import DataTable, { type Column } from "@/components/ui/DataTable.vue"
import Modal from "@/components/ui/Modal.vue"
import { useI18n } from "@/i18n"
import BarChart from "./BarChart.vue"
import { modelRangeStats, type MetricSeries, type ModelRangeStats } from "./series"

const props = defineProps<{
  title: string
  series: MetricSeries
  formatValue: (n: number) => string
}>()

defineEmits<{ close: [] }>()

const { t } = useI18n()

const stats = computed<ModelRangeStats[]>(() => modelRangeStats(props.series))

const fmt = (n: number | null) => (n == null ? "—" : props.formatValue(n))

const columns = computed<Column<ModelRangeStats>[]>(() => [
  { key: "model", header: t("overview.detail.model"), value: (r) => r.label },
  { key: "min", header: t("overview.detail.min"), numeric: true, value: (r) => fmt(r.min) },
  { key: "max", header: t("overview.detail.max"), numeric: true, value: (r) => fmt(r.max) },
  { key: "avg", header: t("overview.detail.avg"), numeric: true, value: (r) => fmt(r.avg) },
  { key: "sum", header: t("overview.detail.sum"), numeric: true, value: (r) => props.formatValue(r.sum) },
])
</script>

<template>
  <Modal :title="title" size="lg" @close="$emit('close')">
    <div class="detail">
      <BarChart
        :buckets="series.buckets"
        :format-value="formatValue"
        :title="title"
        :total-label="t('overview.chart.total')"
        :legend="series.models"
      />

      <DataTable
        :columns="columns"
        :rows="stats"
        :row-key="(r) => r.key"
        :caption="t('overview.detail.caption', { metric: title })"
      >
        <template #cell-model="{ row }">
          <span class="model-cell">
            <span class="swatch" :style="{ '--swatch': row.color }" aria-hidden="true" />
            <span class="model-label" :title="row.label">{{ row.label }}</span>
          </span>
        </template>
      </DataTable>
    </div>
  </Modal>
</template>

<style scoped>
.detail {
  display: flex;
  flex-direction: column;
  gap: var(--space-5);
  min-width: 0;
}

.model-cell {
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
  min-width: 0;
  max-width: 100%;
}

.swatch {
  width: 8px;
  height: 8px;
  flex-shrink: 0;
  border-radius: var(--radius-full);
  background: var(--swatch);
}

.model-label {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>
