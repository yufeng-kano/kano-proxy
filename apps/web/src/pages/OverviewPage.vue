<script setup lang="ts">
/**
 * Overview: the one page that must fit a single viewport (docs/admin-ui.md
 * § Anti-scroll rules). Stat row, then the time series and the per-model
 * breakdown side by side — nothing stacks below the fold, and only the
 * breakdown scrolls, inside its own card.
 *
 * Two layout decisions carry that:
 *
 * - The tiles reflow on **container** width, not viewport width. The sidebar
 *   collapses to a drawer at 1080px, which changes the content region's width
 *   without changing the viewport's — a media query would size the tiles for a
 *   region that is no longer there.
 * - The chart body is a fixed height. An aspect-ratio chart in a region this
 *   wide grows past the fold on its own.
 *
 * Data is cache-first and silent about it: the cache paints immediately and
 * says nothing. Only the Refresh the user pressed reports progress, on the
 * button they pressed.
 */
import { computed, onMounted, ref, watch } from "vue"
import UsageChart from "@/components/overview/UsageChart.vue"
import AppButton from "@/components/ui/AppButton.vue"
import AppCard from "@/components/ui/AppCard.vue"
import Banner from "@/components/ui/Banner.vue"
import DataTable, { type Column } from "@/components/ui/DataTable.vue"
import EmptyState from "@/components/ui/EmptyState.vue"
import PageHeader from "@/components/ui/PageHeader.vue"
import Segmented from "@/components/ui/Segmented.vue"
import StatTile from "@/components/ui/StatTile.vue"
import { useAuth } from "@/composables/useAuth"
import { useScrollRestore } from "@/composables/useScrollRestore"
import { useUsage } from "@/composables/useUsage"
import { useI18n } from "@/i18n"
import { getOverviewPrefs, setOverviewPrefs, type ChartView } from "@/services/prefs"
import type { ModelUsageRow, UsageDays } from "@/types"

const { t, format } = useI18n()
const { user } = useAuth()
const { summary, loading, error, days, setDays, setUserId, refresh } = useUsage()
const { markReady } = useScrollRestore()

const prefs = getOverviewPrefs()
const showTable = ref(prefs.showTable)
const chartView = ref<ChartView>(prefs.chartView)
/** Only the user's own Refresh — a background refresh shows nothing. */
const manualRefreshing = ref(false)

watch(showTable, (value) => setOverviewPrefs({ showTable: value }))
watch(chartView, (value) => setOverviewPrefs({ chartView: value }))

const rangeOptions = computed(() => [
  { value: 1, label: t("overview.range.short.24h"), title: t("overview.range.24h") },
  { value: 7, label: t("overview.range.short.7d"), title: t("overview.range.7d") },
  { value: 30, label: t("overview.range.short.30d"), title: t("overview.range.30d") },
])

const chartViewOptions = computed(() => [
  { value: "tokens", label: t("overview.chart.tokens") },
  { value: "cache-rate", label: t("overview.chart.cacheRate") },
])

/** Segmented models a `string | number`; the range is one of three literals. */
function onRangeChange(value: string | number) {
  setDays(value as UsageDays)
}

function onChartViewChange(value: string | number) {
  chartView.value = value as ChartView
}

onMounted(async () => {
  setUserId(user.value?.id ?? null)
  await refresh()
  // Content has painted (or resolved to an empty state) — only now is the
  // region tall enough for a saved scroll offset to land.
  await markReady()
})

async function onRefresh() {
  manualRefreshing.value = true
  try {
    await refresh({ refresh: true })
  } finally {
    manualRefreshing.value = false
  }
}

// ---------------------------------------------------------------------------
// Tiles
// ---------------------------------------------------------------------------
const totals = computed(() => summary.value?.totals ?? null)
const isEmpty = computed(() => totals.value != null && totals.value.requests === 0)
/** First paint with nothing cached — the tiles show their own skeletons. */
const isFirstLoad = computed(() => loading.value && !summary.value)

const requestsValue = computed(() => format.integer(totals.value?.requests))
const tokensValue = computed(() => {
  const v = totals.value
  return format.compact(v ? v.prompt_tokens + v.completion_tokens : null)
})
const cacheRateValue = computed(() => format.percent(totals.value?.cache_rate))
const errorsValue = computed(() => format.integer(totals.value?.errors))
const latencyValue = computed(() => format.duration(totals.value?.avg_latency_ms))

const cacheMeter = computed(() => {
  const rate = totals.value?.cache_rate
  if (rate == null || Number.isNaN(rate)) return null
  return Math.min(100, Math.max(0, rate * 100))
})

/**
 * How much of the range the cache figure actually covers. Requests with NULL
 * token fields count toward the request total but are skipped by the cache
 * aggregate, so an unannotated percentage would silently overclaim.
 */
const cacheNote = computed(() => {
  const v = totals.value
  if (!v) return null
  if (v.cache_rate == null) return t("overview.stat.cacheNone")
  if (v.cache_known_requests < v.requests) {
    return t("overview.stat.cacheCoverage", {
      known: format.integer(v.cache_known_requests),
      total: format.integer(v.requests),
    })
  }
  return null
})

const errorNote = computed(() => {
  const v = totals.value
  if (!v || v.errors === 0 || v.requests === 0) return null
  return t("overview.stat.errorRate", { rate: format.percent(v.errors / v.requests) })
})

// ---------------------------------------------------------------------------
// Per-model breakdown
// ---------------------------------------------------------------------------
const modelRows = computed<ModelUsageRow[]>(() => summary.value?.models ?? [])

const modelColumns = computed<Column<ModelUsageRow>[]>(() => [
  { key: "model", header: t("overview.models.model"), value: (row) => row.model },
  {
    key: "requests",
    header: t("overview.models.requests"),
    numeric: true,
    value: (row) => format.integer(row.requests),
  },
  {
    key: "input",
    header: t("overview.models.input"),
    numeric: true,
    value: (row) => format.compact(row.prompt_tokens),
  },
  {
    key: "cached",
    header: t("overview.models.cached"),
    numeric: true,
    hideOnMobile: true,
    value: (row) => format.compact(row.cache_read_input_tokens),
  },
  {
    key: "cacheWrite",
    header: t("overview.models.cacheWrite"),
    numeric: true,
    hideOnMobile: true,
    value: (row) => format.compact(row.cache_creation_input_tokens),
  },
  {
    key: "output",
    header: t("overview.models.output"),
    numeric: true,
    value: (row) => format.compact(row.completion_tokens),
  },
  {
    key: "cacheRate",
    header: t("overview.models.cacheRate"),
    numeric: true,
    value: (row) => format.percent(row.cache_rate),
  },
])

function modelRowKey(row: ModelUsageRow): string {
  return row.model
}

/** Same coverage caveat as the hero tile, per model. */
function coverageNote(row: ModelUsageRow): string | null {
  if (row.cache_known_requests >= row.requests) return null
  return t("overview.models.coverage", {
    known: format.integer(row.cache_known_requests),
    total: format.integer(row.requests),
  })
}
</script>

<template>
  <div>
    <PageHeader :title="t('overview.title')" :subtitle="t('overview.subtitle')">
      <template #actions>
        <Segmented
          :model-value="days"
          :options="rangeOptions"
          :label="t('overview.range.label')"
          @update:model-value="onRangeChange"
        />
        <AppButton :loading="manualRefreshing" @click="onRefresh">
          {{ t("action.refresh") }}
        </AppButton>
      </template>
    </PageHeader>

    <div v-if="error" class="page-alert">
      <Banner tone="warn">{{ error }}</Banner>
    </div>

    <!-- The container the tiles size against: the content region's width, not
         the viewport's. The sidebar's presence changes one and not the other. -->
    <div class="region">
      <div class="tiles">
        <StatTile
          :label="t('overview.stat.requests')"
          :value="requestsValue"
          :loading="isFirstLoad"
        />
        <StatTile :label="t('overview.stat.tokens')" :value="tokensValue" :loading="isFirstLoad" />
        <StatTile
          class="tile-hero"
          hero
          :label="t('overview.stat.cacheRate')"
          :value="cacheRateValue"
          :meter="cacheMeter"
          :note="cacheNote"
          :loading="isFirstLoad"
        />
        <StatTile
          :label="t('overview.stat.errors')"
          :value="errorsValue"
          :note="errorNote"
          :loading="isFirstLoad"
        />
        <StatTile
          :label="t('overview.stat.latency')"
          :value="latencyValue"
          :loading="isFirstLoad"
        />
      </div>

      <AppCard v-if="isEmpty" class="empty-card">
        <EmptyState :title="t('overview.empty.title')" :body="t('overview.empty.body')">
          <template #action>
            <AppButton variant="primary" to="/keys">
              {{ t("overview.empty.action") }}
            </AppButton>
          </template>
        </EmptyState>
      </AppCard>

      <div v-else class="panels">
        <AppCard fill :title="t('overview.chart.title')">
          <template #actions>
            <Segmented
              size="sm"
              :model-value="chartView"
              :options="chartViewOptions"
              :label="t('overview.chart.view')"
              @update:model-value="onChartViewChange"
            />
            <AppButton size="sm" variant="ghost" @click="showTable = !showTable">
              {{ showTable ? t("overview.chart.showChart") : t("overview.chart.showTable") }}
            </AppButton>
          </template>

          <UsageChart
            v-if="summary"
            :summary="summary"
            :view="chartView"
            :show-table="showTable"
          />
          <!-- Same height as the plot, so the card does not resize when data
               lands. -->
          <div v-else class="chart-placeholder" role="status">
            <span class="sr-only">{{ t("app.loading") }}</span>
          </div>
        </AppCard>

        <AppCard
          fill
          flush
          :title="t('overview.models.title')"
          :subtitle="t('overview.models.subtitle')"
        >
          <DataTable
            v-if="modelRows.length"
            :columns="modelColumns"
            :rows="modelRows"
            :row-key="modelRowKey"
            :caption="t('overview.models.title')"
          >
            <template #cell-model="{ row }">
              <span class="mono model-id">{{ row.model }}</span>
            </template>
            <template #cell-cacheRate="{ row }">
              <span class="tabular">{{ format.percent(row.cache_rate) }}</span>
              <span v-if="coverageNote(row)" class="coverage">{{ coverageNote(row) }}</span>
            </template>
          </DataTable>
          <EmptyState v-else compact :title="t('overview.models.empty')" />
        </AppCard>
      </div>
    </div>
  </div>
</template>

<style scoped>
/* No gap on the page itself: PageHeader carries its own bottom margin. */
.page-alert {
  margin-bottom: var(--space-4);
}

/* The query container. Everything inside sizes against *this* width, so the
   sidebar collapsing to a drawer is a layout signal rather than a surprise. */
.region {
  container-type: inline-size;
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
  min-width: 0;
}

.tiles {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: var(--space-3);
}

/* The hero spans two columns until the grid itself is only two wide. */
.tile-hero {
  grid-column: span 2;
}

@container (min-width: 640px) {
  .tiles {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }
}

@container (min-width: 960px) {
  .tiles {
    /* 5 tiles across 6 tracks — the hero takes the extra one. */
    grid-template-columns: repeat(6, minmax(0, 1fr));
  }
}

/* Chart beside breakdown. The chart column takes the slack; the breakdown is
   capped so a wide display does not hand it space the chart needs more. */
.panels {
  display: grid;
  grid-template-columns: minmax(0, 1fr);
  gap: var(--space-4);
  min-width: 0;
}

@container (min-width: 1200px) {
  .panels {
    grid-template-columns: minmax(0, 1fr) minmax(0, 420px);
    /* Both cards share one row height, which is what gives `fill` a bounded
       box to scroll the table inside of. */
    align-items: stretch;
  }
}

.empty-card {
  min-height: 0;
}

/* Matches UsageChart's plot height, so switching from placeholder to chart
   does not shift the page. */
.chart-placeholder {
  height: 260px;
}

.model-id {
  color: var(--text);
}

.coverage {
  display: block;
  margin-top: var(--space-1);
  color: var(--faint);
  font-size: var(--text-2xs);
}
</style>
