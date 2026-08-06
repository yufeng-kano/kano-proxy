<script setup lang="ts">
/**
 * Overview (docs/admin-ui.md § Overview page): three metric cards — Spend /
 * Requests / Tokens, each a headline + mini stacked chart + top models, each
 * expandable into a detail modal — then one Activity card whose sub-tabs
 * (Tokens / Requests / Cache / By model) share a single full-size chart
 * region. Errors and average latency ride the Activity header as compact
 * stats, so nothing the old tile row said is lost.
 *
 * Data is cache-first and silent about it: the cache paints immediately and
 * says nothing. Only the Refresh the user pressed reports progress, on the
 * button they pressed.
 */
import { computed, onMounted, ref, watch } from "vue"
import BarChart from "@/components/overview/BarChart.vue"
import MetricDetailModal from "@/components/overview/MetricDetailModal.vue"
import StatCard from "@/components/overview/StatCard.vue"
import {
  buildCacheSeries,
  buildMetricSeries,
  type MetricId,
  type MetricSeries,
} from "@/components/overview/series"
import ActionIcon from "@/components/ui/ActionIcon.vue"
import AppButton from "@/components/ui/AppButton.vue"
import AppCard from "@/components/ui/AppCard.vue"
import Banner from "@/components/ui/Banner.vue"
import DataTable, { type Column } from "@/components/ui/DataTable.vue"
import EmptyState from "@/components/ui/EmptyState.vue"
import PageHeader from "@/components/ui/PageHeader.vue"
import SectionNav, { type SectionItem } from "@/components/ui/SectionNav.vue"
import Segmented from "@/components/ui/Segmented.vue"
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

const activityView = ref<ChartView>(getOverviewPrefs().chartView)
/** Only the user's own Refresh — a background refresh shows nothing. */
const manualRefreshing = ref(false)
/** Which metric's detail modal is open. */
const expanded = ref<MetricId | null>(null)

watch(activityView, (value) => setOverviewPrefs({ chartView: value }))

const rangeOptions = computed(() => [
  { value: 1, label: t("overview.range.short.24h"), title: t("overview.range.24h") },
  { value: 7, label: t("overview.range.short.7d"), title: t("overview.range.7d") },
  { value: 30, label: t("overview.range.short.30d"), title: t("overview.range.30d") },
])

/** Segmented models a `string | number`; the range is one of three literals. */
function onRangeChange(value: string | number) {
  setDays(value as UsageDays)
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
// Series
// ---------------------------------------------------------------------------
const totals = computed(() => summary.value?.totals ?? null)
const isEmpty = computed(() => totals.value != null && totals.value.requests === 0)
/** First paint with nothing cached — the cards show their own skeletons. */
const isFirstLoad = computed(() => loading.value && !summary.value)

const EMPTY_SERIES: MetricSeries = { total: 0, models: [], buckets: [] }

function metricSeriesFor(metric: MetricId): MetricSeries {
  const s = summary.value
  if (!s) return EMPTY_SERIES
  return buildMetricSeries(s, metric, t("overview.card.others"), format)
}

const spendSeries = computed(() => metricSeriesFor("spend"))
const requestsSeries = computed(() => metricSeriesFor("requests"))
const tokensSeries = computed(() => metricSeriesFor("tokens"))
const cacheSeries = computed(() => {
  const s = summary.value
  if (!s) return EMPTY_SERIES
  return buildCacheSeries(
    s,
    { cached: t("overview.cache.cached"), uncached: t("overview.cache.uncached") },
    format,
  )
})

const formatCurrency = (n: number) => format.currency(n)
const formatInteger = (n: number) => format.integer(n)
const formatCompact = (n: number) => format.compact(n)

/**
 * Spend is an estimate, and possibly a partial one — say so on the card
 * rather than letting a lowball number read as a bill (docs/pricing.md).
 */
const spendNote = computed(() => {
  const v = totals.value
  if (!v) return null
  if (v.cost == null) return t("overview.spend.none")
  if (v.cost_known_requests < v.requests) {
    return t("overview.spend.coverage", {
      known: format.integer(v.cost_known_requests),
      total: format.integer(v.requests),
    })
  }
  return t("overview.spend.estimated")
})

type MetricCard = {
  id: MetricId
  title: string
  series: MetricSeries
  formatValue: (n: number) => string
  /** Overrides the series total as the card headline; null = use the total. */
  headline: string | null
  note: string | null
}

const cards = computed<MetricCard[]>(() => [
  {
    id: "spend",
    title: t("overview.metric.spend"),
    series: spendSeries.value,
    formatValue: formatCurrency,
    // "—", not "$0.00", when nothing in the range is priced: an unknown cost
    // is not a free range (docs/pricing.md).
    headline: totals.value?.cost == null ? format.currency(null) : null,
    note: spendNote.value,
  },
  {
    id: "requests",
    title: t("overview.metric.requests"),
    series: requestsSeries.value,
    formatValue: formatInteger,
    headline: null,
    note: null,
  },
  {
    id: "tokens",
    title: t("overview.metric.tokens"),
    series: tokensSeries.value,
    formatValue: formatCompact,
    headline: null,
    note: null,
  },
])

const expandedCard = computed(() => cards.value.find((c) => c.id === expanded.value) ?? null)

// ---------------------------------------------------------------------------
// Activity card
// ---------------------------------------------------------------------------
const activityTabs = computed<SectionItem[]>(() => [
  { id: "tokens", label: t("overview.tab.tokens") },
  { id: "requests", label: t("overview.tab.requests") },
  { id: "cache", label: t("overview.tab.cache") },
  { id: "models", label: t("overview.tab.models") },
])

function onSelectActivityTab(id: string) {
  activityView.value = (["tokens", "requests", "cache", "models"] as ChartView[]).includes(
    id as ChartView,
  )
    ? (id as ChartView)
    : "tokens"
}

const activityChart = computed<{ series: MetricSeries; formatValue: (n: number) => string } | null>(
  () => {
    if (activityView.value === "tokens") {
      return { series: tokensSeries.value, formatValue: formatCompact }
    }
    if (activityView.value === "requests") {
      return { series: requestsSeries.value, formatValue: formatInteger }
    }
    if (activityView.value === "cache") {
      return { series: cacheSeries.value, formatValue: formatCompact }
    }
    return null
  },
)

/** Errors + latency — the two former tiles, now compact header stats. */
const errorsStat = computed(() => format.integer(totals.value?.errors))
const latencyStat = computed(() => format.duration(totals.value?.avg_latency_ms))

// ---------------------------------------------------------------------------
// By model table
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
    key: "errors",
    header: t("overview.models.errors"),
    numeric: true,
    hideOnMobile: true,
    value: (row) => format.integer(row.errors),
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
  {
    key: "cost",
    header: t("overview.models.spend"),
    numeric: true,
    value: (row) => format.currency(row.cost),
  },
])

function modelRowKey(row: ModelUsageRow): string {
  return row.model
}

/** Cache coverage caveat, per model — partial data is annotated, never mixed silently. */
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
        <!-- Icon-only: the label is a tooltip and the accessible name. -->
        <AppButton
          icon-only
          :label="t('action.refresh')"
          :loading="manualRefreshing"
          @click="onRefresh"
        >
          <template #icon><ActionIcon name="refresh" /></template>
        </AppButton>
      </template>
    </PageHeader>

    <!-- Non-blocking: whatever was cached stays on screen underneath, and the
         banner offers the one control that can fix it. -->
    <div v-if="error" class="page-alert">
      <Banner tone="warn">
        {{ t("overview.error.load") }}
        <template #actions>
          <AppButton size="sm" variant="ghost" :loading="manualRefreshing" @click="onRefresh">
            {{ t("action.retry") }}
          </AppButton>
        </template>
      </Banner>
    </div>

    <AppCard v-if="isEmpty">
      <EmptyState :title="t('overview.empty.title')" :body="t('overview.empty.body')">
        <template #action>
          <AppButton variant="primary" to="/keys">
            {{ t("overview.empty.action") }}
          </AppButton>
        </template>
      </EmptyState>
    </AppCard>

    <div v-else class="region">
      <!-- The container the cards size against: the content region's width,
           not the viewport's — the sidebar collapsing changes one, not the
           other. -->
      <div class="cards">
        <StatCard
          v-for="card in cards"
          :key="card.id"
          :title="card.title"
          :series="card.series"
          :format-value="card.formatValue"
          :headline="card.headline"
          :note="card.note"
          :loading="isFirstLoad"
          @expand="expanded = card.id"
        />
      </div>

      <AppCard flush class="activity">
        <div class="activity-head">
          <SectionNav
            :items="activityTabs"
            :active="activityView"
            :label="t('overview.activity.label')"
            @select="onSelectActivityTab"
          />
          <!-- The two former tiles, as quiet header stats. -->
          <div class="activity-stats">
            <span class="stat">
              <span class="stat-label">{{ t("overview.stat.errors") }}</span>
              <span class="stat-value tabular">{{ errorsStat }}</span>
            </span>
            <span class="stat">
              <span class="stat-label">{{ t("overview.stat.latency") }}</span>
              <span class="stat-value tabular">{{ latencyStat }}</span>
            </span>
          </div>
        </div>

        <!-- The panel the tabs point at: SectionNav's aria-controls is
             `panel-<id>`. -->
        <div
          :id="`panel-${activityView}`"
          role="tabpanel"
          class="activity-body"
          :class="{ 'is-table': activityView === 'models' }"
        >
          <template v-if="activityView !== 'models'">
            <BarChart
              v-if="summary && activityChart"
              :buckets="activityChart.series.buckets"
              :format-value="activityChart.formatValue"
              :title="t('overview.activity.label')"
              :total-label="t('overview.chart.total')"
              :legend="activityChart.series.models"
            />
            <div v-else class="chart-placeholder" role="status">
              <span class="sr-only">{{ t("app.loading") }}</span>
            </div>
          </template>

          <template v-else>
            <DataTable
              v-if="modelRows.length"
              :columns="modelColumns"
              :rows="modelRows"
              :row-key="modelRowKey"
              :caption="t('overview.tab.models')"
            >
              <template #cell-model="{ row }">
                <span class="mono model-id">{{ row.model }}</span>
              </template>
              <template #cell-cacheRate="{ row }">
                <span class="tabular">{{ format.percent(row.cache_rate) }}</span>
                <span v-if="coverageNote(row)" class="coverage">{{ coverageNote(row) }}</span>
              </template>
            </DataTable>
            <EmptyState
              v-else-if="summary"
              compact
              :title="t('overview.models.empty')"
            />
            <span v-else class="sr-only" role="status">{{ t("app.loading") }}</span>
          </template>
        </div>
      </AppCard>
    </div>

    <MetricDetailModal
      v-if="expandedCard"
      :title="expandedCard.title"
      :series="expandedCard.series"
      :format-value="expandedCard.formatValue"
      @close="expanded = null"
    />
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

/* 1 → 3 across as the content region widens. Container queries, not media
   queries: the sidebar collapsing at 1080px changes this region's width
   without changing the viewport's. */
.cards {
  display: grid;
  grid-template-columns: minmax(0, 1fr);
  gap: var(--space-4);
}

@container (min-width: 760px) {
  .cards {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }
}

/* --- Activity card ------------------------------------------------------- */

.activity-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-4);
  flex-wrap: wrap;
  padding: 0 var(--space-5);
  border-bottom: 1px solid var(--border);
}

/* The tabs' active underline sits on the head's own border, not above it —
   same -1px trick as PageHeader's nav row. */
.activity-head :deep(.section-nav) {
  margin-bottom: -1px;
}

.activity-stats {
  display: flex;
  align-items: baseline;
  gap: var(--space-4);
}

.stat {
  display: inline-flex;
  align-items: baseline;
  gap: var(--space-2);
  font-size: var(--text-xs);
}

.stat-label {
  color: var(--faint);
}

.stat-value {
  color: var(--text-secondary);
  font-weight: var(--weight-medium);
}

.activity-body {
  padding: var(--space-4) var(--space-5) var(--space-5);
}

/* The table manages its own cell padding and scrolls inside a bounded box —
   ~9 rows, then the sticky header takes over. */
.activity-body.is-table {
  padding: 0;
  max-height: 420px;
  overflow: auto;
}

/* Matches BarChart's full plot height so the card does not resize when the
   data lands or the tab changes. */
.chart-placeholder {
  height: 240px;
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
