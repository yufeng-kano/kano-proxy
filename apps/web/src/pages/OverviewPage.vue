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
import ActionIcon from "@/components/ui/ActionIcon.vue"
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
        <!-- Icon-only: the label is a tooltip and the accessible name, so the
             control keeps its meaning without spending header width on a word
             that repeats on every page. -->
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
         banner offers the one control that can fix it. Named in the user's
         terms rather than echoing the transport's error — there is nothing
         actionable in a status code here, unlike a custom endpoint's own
         upstream message, which the user needs verbatim to fix their config. -->
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

      <AppCard v-if="isEmpty">
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

        <div class="models-slot">
          <AppCard
            fill
            flush
            class="models-card"
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
            <!-- Only once the range has actually resolved: before that, "no
                 model activity" is a claim the page cannot yet make. -->
            <EmptyState
              v-else-if="summary"
              compact
              :title="t('overview.models.empty')"
            />
            <span v-else class="sr-only" role="status">{{ t("app.loading") }}</span>
          </AppCard>
        </div>
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

/* 5 across → 3 → 2 as the *content region* narrows. Container queries, not
   media queries: the sidebar collapsing to a drawer at 1080px changes this
   region's width without changing the viewport's, and a media query would
   keep sizing the tiles for a sidebar that is no longer there. */
.tiles {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: var(--space-3);
}

@container (min-width: 620px) {
  .tiles {
    /* 3 wide: the hero takes a full row of its own rather than leaving a
       ragged hole beside it. */
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }

  .tile-hero {
    grid-column: 1 / -1;
  }
}

@container (min-width: 960px) {
  .tiles {
    /* 5 tiles over 6 tracks — the hero is the one that takes two. */
    grid-template-columns: repeat(6, minmax(0, 1fr));
  }

  .tile-hero {
    grid-column: span 2;
  }
}

/* Chart beside breakdown. The chart column takes the slack; the breakdown is
   capped so a wide display does not hand it room the chart wants more.

   A viewport media query here, unlike the tiles above, and deliberately: the
   split is specified against the viewport (docs/admin-ui.md § Responsive), and
   at the 1440px target the content region is ~1128px wide — a *container*
   query at 1200px would never fire there and would stack the two cards on
   exactly the viewport this page is measured against. */
.panels {
  display: grid;
  grid-template-columns: minmax(0, 1fr);
  gap: var(--space-4);
  min-width: 0;
}

@media (min-width: 1200px) {
  .panels {
    grid-template-columns: minmax(0, 1fr) minmax(0, 420px);
  }

  /* Seven numeric columns do not fit 420px, so the breakdown scrolls sideways
     inside its own card — hiding exactly the columns it exists to show. Once
     the display is wide enough to pay for it, the track widens to the table's
     natural width instead.

     1700px is where that stops costing the chart its lead: the content region
     is then ~1388px, so a 680px breakdown still leaves the chart the larger
     column. Widening any earlier makes the breakdown the wider of the two,
     which is the opposite of what this page emphasizes. */
  @media (min-width: 1700px) {
    .panels {
      grid-template-columns: minmax(0, 1fr) minmax(0, 680px);
    }
  }

  /* The row must be as tall as the *chart*, not as tall as the model list —
     otherwise a 40-model range sets the row height and pushes the page past
     the fold, which is exactly what this page exists not to do. Taking the
     breakdown out of flow makes its slot contribute nothing; the slot then
     stretches to the chart's height and the card fills it, which is what
     finally gives `fill` a bounded body to scroll the table inside.
     (`.region` is a container-query container, so it is already a containing
     block for absolutes — the slot's own `relative` is what keeps the card
     anchored to its column rather than to the whole region.) */
  .models-slot {
    position: relative;
  }

  .models-card {
    position: absolute;
    inset: 0;
  }
}

/* Matches UsageChart's plot height, so the card is the same size before and
   after the first paint — no shift when the data lands. */
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
