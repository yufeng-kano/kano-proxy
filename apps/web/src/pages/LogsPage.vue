<script setup lang="ts">
/**
 * Logs: every request the user's keys made, newest first (docs/admin-ui.md
 * § Logs page).
 *
 * The per-request companion to Overview's aggregates — the account and alias
 * the By-model table folds away live here, as does the error evidence
 * (`error_code`, `upstream_status`) that otherwise needs a database query.
 * Nothing is scoped to live providers: a row from a since-deleted endpoint is
 * still what happened, and hiding it would hurt the diagnosis this page exists
 * for.
 *
 * One bounded card filling the content region, rows scrolling inside it with a
 * sticky header and grouped under their local day. Every row is one line: the
 * columns are pixel tracks the user can hide and drag, and a cell that does
 * not fit is cut with its full text on a `title`. The pickers count today's
 * use from `/api/logs/facets`; every filter is applied server-side, so a
 * change reloads from the first page rather than narrowing what is painted.
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue"
import LogDetailModal from "@/components/LogDetailModal.vue"
import LogExportDialog from "@/components/LogExportDialog.vue"
import ActionIcon from "@/components/ui/ActionIcon.vue"
import ActionMenu from "@/components/ui/ActionMenu.vue"
import AppButton from "@/components/ui/AppButton.vue"
import AppCard from "@/components/ui/AppCard.vue"
import Badge from "@/components/ui/Badge.vue"
import Banner from "@/components/ui/Banner.vue"
import DataTable from "@/components/ui/DataTable.vue"
import type { Column } from "@/components/ui/DataTable.vue"
import EmptyState from "@/components/ui/EmptyState.vue"
import ListPicker, { type ListPickerOption } from "@/components/ui/ListPicker.vue"
import PageHeader from "@/components/ui/PageHeader.vue"
import Segmented from "@/components/ui/Segmented.vue"
import { useAuth } from "@/composables/useAuth"
import { useCli } from "@/composables/useCli"
import { useCustomProviders } from "@/composables/useCustomProviders"
import { useLogs } from "@/composables/useLogs"
import { useI18n } from "@/i18n"
import type { MessageKey } from "@/i18n"
import {
  DEFAULT_WIDTHS,
  LOCKED_COLUMNS,
  LOG_COLUMN_KEYS,
  MODEL_MIN_WIDTH,
  daysAgo,
  filterKey,
  isFailure,
  modelName,
  optionColors,
  pickerOptions,
  rowOptionKey,
  type LogColumnKey,
  type PickerView,
} from "@/services/logsView"
import { getLogsPrefs, setLogsPrefs } from "@/services/prefs"
import { PROVIDERS, type LogFacetTally, type ProviderId, type RequestLogRow } from "@/types"

const { t, format } = useI18n()
const { user } = useAuth()
const {
  state: logs,
  provider: providerFilter,
  modelFilter,
  errorsOnly,
  facets,
  loadFacets,
  setUserId: setLogsUserId,
  load: loadLogs,
  loadMore,
  setProvider,
  setModelFilter,
  setErrorsOnly,
} = useLogs()
const customProviders = useCustomProviders()
const cli = useCli()

/**
 * The "no filter" option key. Underscored so it can never collide with a
 * custom endpoint's slug, which is lowercase letters, digits, hyphens.
 */
const ALL = "__all__"

/**
 * Provider display copy lives in the catalog, and `PROVIDERS` carries only wire
 * ids. An explicit map rather than a template literal, same as the Models page:
 * a built key widens to `string` and would not fail the build when renamed.
 */
const NAME_KEY: Record<ProviderId, MessageKey> = {
  "claude-code": "provider.claude-code.name",
  codex: "provider.codex.name",
  grok: "provider.grok.name",
  antigravity: "provider.antigravity.name",
}

const COLUMN_KEY: Record<LogColumnKey, MessageKey> = {
  time: "logs.column.time",
  model: "logs.column.model",
  account: "logs.column.account",
  apiKey: "logs.column.apiKey",
  status: "logs.column.status",
  input: "logs.column.input",
  cacheRead: "logs.column.cacheRead",
  cacheWrite: "logs.column.cacheWrite",
  output: "logs.column.output",
  cost: "logs.column.cost",
  latency: "logs.column.latency",
}

/** Which row's detail dialog is open. */
const detail = ref<RequestLogRow | null>(null)
const exporting = ref(false)
/** Only the user's own Refresh — a background refresh shows nothing. */
const manualRefreshing = ref(false)
/** What "today" and "yesterday" are measured from; moved on every load. */
const now = ref(new Date())

const prefs = getLogsPrefs()
const view = ref<PickerView>(prefs.view)
const hidden = ref<LogColumnKey[]>(prefs.hidden)
const widths = ref<Partial<Record<LogColumnKey, number>>>(prefs.widths)

const rows = computed(() => logs.rows)
const showSkeleton = computed(() => logs.loading && rows.value.length === 0)
const isFiltered = computed(
  () => providerFilter.value !== null || modelFilter.value !== null || errorsOnly.value,
)

/* --- Providers ---------------------------------------------------------- */

const builtinIds = new Set<string>(PROVIDERS.map((p) => p.id))

function providerName(slug: string): string {
  if (builtinIds.has(slug)) return t(NAME_KEY[slug as ProviderId])
  return (
    customProviders.state.data?.find((cp) => cp.slug === slug)?.name ??
    cli.state.providers?.find((cp) => cp.slug === slug)?.name ??
    slug
  )
}

function tallyText(tally: LogFacetTally | null | undefined): string | null {
  if (!tally) return null
  return t("logs.picker.meta", { requests: format.integer(tally.requests), cost: format.currency(tally.cost) })
}

/**
 * All + the builtins + the user's live custom and CLI slugs — the same sources
 * the Models and Providers pages filter by — each with its kind and today's
 * count. A slug the user filtered to and has since deleted stays as its own
 * bare option: its rows are still in the log.
 */
/** Today across every provider; null until the facets land. */
const totalTally = computed<LogFacetTally | null>(() => {
  if (!facets.data) return null
  let requests = 0
  let cost: number | null = null
  for (const p of facets.data.providers) {
    requests += p.requests
    if (p.cost !== null) cost = (cost ?? 0) + p.cost
  }
  return { requests, cost }
})

const providerOptions = computed<ListPickerOption[]>(() => {
  const today = new Map((facets.data?.providers ?? []).map((p) => [p.provider, p]))
  const option = (slug: string, label: string, kind: MessageKey) => ({
    key: slug,
    label,
    sub: t(kind),
    meta: tallyText(today.get(slug)),
  })
  const options: ListPickerOption[] = [
    { key: ALL, label: t("logs.filter.allProviders"), meta: tallyText(totalTally.value) },
    ...PROVIDERS.map((p) => option(p.id, t(NAME_KEY[p.id]), "logs.picker.kind.subscription")),
    ...(customProviders.state.data ?? []).map((cp) => option(cp.slug, cp.name, "logs.picker.kind.endpoint")),
    ...(cli.state.providers ?? []).map((cp) => option(cp.slug, cp.name, "logs.picker.kind.cli")),
  ]
  const selected = providerFilter.value
  if (selected && !options.some((o) => o.key === selected)) {
    options.push({ key: selected, label: selected, meta: tallyText(today.get(selected)) })
  }
  return options
})

function onProviderPick(key: string) {
  const next = key === ALL ? null : key
  const current = modelFilter.value
  // Keep a model or alias choice only where it still has rows under the new provider.
  const keep =
    next === null ||
    current === null ||
    current.kind === "direct" ||
    (current.kind === "model" && current.value.startsWith(`${next}/`)) ||
    (current.kind === "group" &&
      !!facets.data?.groups.find((g) => g.group_name === current.value)?.models.some((m) => m.startsWith(`${next}/`)))
  void setProvider(next, keep)
}

/* --- Models and aliases ------------------------------------------------- */

const modelPickerOptions = computed(() =>
  pickerOptions(facets.data, view.value, providerFilter.value, modelFilter.value, providerName, t("logs.picker.noAlias")),
)
/** The same mapping colors the picker's dots and the rows' dots, so a color names one thing. */
const colors = computed(() => optionColors(modelPickerOptions.value))

const modelOptions = computed<ListPickerOption[]>(() => {
  // "All" under a provider filter is that provider's day, otherwise everyone's.
  const allTally = providerFilter.value
    ? facets.data?.providers.find((p) => p.provider === providerFilter.value)
    : totalTally.value
  return [
    {
      key: "all",
      label: view.value === "model" ? t("logs.picker.allModels") : t("logs.picker.allAliases"),
      meta: tallyText(allTally),
    },
    ...modelPickerOptions.value.map((o) => ({
      key: o.key,
      label: o.label,
      sub: o.sub,
      meta: tallyText(o.tally),
      color: colors.value.get(o.key),
    })),
  ]
})

const viewOptions = computed(() => [
  { value: "model", label: t("logs.picker.byModel") },
  { value: "alias", label: t("logs.picker.byAlias") },
])

function onViewChange(value: string | number) {
  view.value = value === "alias" ? "alias" : "model"
  setLogsPrefs({ view: view.value })
}

function onModelPick(key: string) {
  const option = modelPickerOptions.value.find((o) => o.key === key)
  void setModelFilter(option ? option.filter : null)
}

/* --- Columns ------------------------------------------------------------ */

const visibleKeys = computed(() => LOG_COLUMN_KEYS.filter((key) => !hidden.value.includes(key)))

function width(key: LogColumnKey): string | undefined {
  const dragged = widths.value[key]
  if (dragged) return `${dragged}px`
  return key === "model" ? undefined : `${DEFAULT_WIDTHS[key]}px`
}

const numericValue: Partial<Record<LogColumnKey, (row: RequestLogRow) => string>> = {
  input: (row) => format.compact(row.prompt_tokens),
  cacheRead: (row) => format.compact(row.cache_read_input_tokens),
  cacheWrite: (row) => format.compact(row.cache_creation_input_tokens),
  output: (row) => format.compact(row.completion_tokens),
  cost: (row) => format.currency(row.cost),
  latency: (row) => format.duration(row.latency_ms),
}

const columns = computed<Column<RequestLogRow>[]>(() =>
  visibleKeys.value.map((key) => ({
    key,
    header: t(COLUMN_KEY[key]),
    width: width(key),
    minWidth: key === "model" ? MODEL_MIN_WIDTH : undefined,
    numeric: key in numericValue,
    value: numericValue[key],
    hideOnMobile: key === "cacheRead" || key === "cacheWrite",
  })),
)

function onResize(key: string, px: number) {
  widths.value = { ...widths.value, [key]: px }
  setLogsPrefs({ widths: widths.value })
}

function toggleColumn(key: LogColumnKey) {
  hidden.value = hidden.value.includes(key) ? hidden.value.filter((k) => k !== key) : [...hidden.value, key]
  setLogsPrefs({ hidden: hidden.value })
}

function resetColumns() {
  hidden.value = []
  widths.value = {}
  setLogsPrefs({ hidden: [], widths: {} })
}

/** The Columns checklist: a small popover of its own, closed by Escape or a press outside. */
const columnsOpen = ref(false)
const columnsWrap = ref<HTMLElement | null>(null)

function onColumnsPointerDown(event: PointerEvent) {
  if (!columnsWrap.value?.contains(event.target as Node)) columnsOpen.value = false
}

watch(columnsOpen, (open) => {
  if (open) document.addEventListener("pointerdown", onColumnsPointerDown)
  else document.removeEventListener("pointerdown", onColumnsPointerDown)
})

onBeforeUnmount(() => document.removeEventListener("pointerdown", onColumnsPointerDown))

/* --- Rows --------------------------------------------------------------- */

function dayLabel(row: RequestLogRow): string {
  const ago = daysAgo(row.created_at, now.value)
  if (ago === 0) return t("logs.day.today", { date: format.date(row.created_at) })
  if (ago === 1) return t("logs.day.yesterday", { date: format.date(row.created_at) })
  return format.weekdayDate(row.created_at)
}

function rowColor(row: RequestLogRow): string {
  return colors.value.get(rowOptionKey(row, view.value)) ?? "var(--series-other)"
}

/** The cell's first, strong part: the model (By model) or the alias (By alias). */
function rowPrimary(row: RequestLogRow): string {
  if (view.value === "alias" && row.group_name) return row.group_name
  return modelName(row.model, row.provider)
}

function rowSecondary(row: RequestLogRow): string {
  if (view.value === "alias") {
    return row.group_name ? `→ ${modelName(row.model, row.provider)}` : `${t("logs.noAliasRow")} · ${providerName(row.provider)}`
  }
  const provider = providerName(row.provider)
  return row.group_name ? `${provider} · ${t("logs.via", { group: row.group_name })}` : provider
}

function rowModelTitle(row: RequestLogRow): string {
  return row.group_name ? `${row.model} (${t("logs.via", { group: row.group_name })})` : row.model
}

/** The row's visible time — also its tooltip and part of its button's name. */
function timeLabel(row: RequestLogRow): string {
  return format.timestamp(row.created_at)
}

/* --- Header ------------------------------------------------------------- */

const showOptions = computed(() => [
  { value: "all", label: t("logs.filter.showAll") },
  { value: "errors", label: t("logs.filter.showErrors") },
])

const titleMenu = computed(() => [{ key: "export", label: t("logs.export.open") }])

function onShowChange(value: string | number) {
  void setErrorsOnly(value === "errors")
}

const exportSummary = computed(() =>
  [
    providerOptions.value.find((o) => o.key === (providerFilter.value ?? ALL))?.label,
    modelOptions.value.find((o) => o.key === filterKey(modelFilter.value))?.label ??
      (view.value === "model" ? t("logs.picker.allModels") : t("logs.picker.allAliases")),
    errorsOnly.value ? t("logs.export.errorsOnly") : t("logs.export.allStatuses"),
  ].join(" · "),
)

/* --- Auto load more ------------------------------------------------------ */

/** The scrolling card, and the marker sitting after the last row inside it. */
const card = ref<InstanceType<typeof AppCard> | null>(null)
const sentinel = ref<HTMLElement | null>(null)
let observer: IntersectionObserver | null = null

/**
 * How early the next page starts loading: one card-height of look-ahead, so
 * the rows are already there by the time the scroll reaches them and the list
 * never stops at a button. A percentage rather than a pixel count because the
 * card's height is whatever the viewport left it.
 */
const APPROACH_MARGIN = "0px 0px 100% 0px"

/**
 * The observer's whole lifecycle hangs off the sentinel's existence, which is
 * the state we actually care about: it renders only while `nextCursor` is set,
 * so the last page unmounts it and the observer stops for good, and a filter
 * change (which nulls the cursor, then loads a fresh first page) unmounts and
 * remounts it — re-arming with an observer rooted on the card as it is now.
 * Watching the ref beats wiring `onMounted`/`onBeforeUnmount` plus a watcher on
 * the cursor, which would be three places to keep agreeing with each other.
 *
 * `flush: "post"` so the card's own ref is populated before we read it: both
 * are set by the same DOM patch, and a pre-flush callback can run first.
 */
watch(
  sentinel,
  (el) => {
    observer?.disconnect()
    observer = null
    // No IntersectionObserver (or nothing to watch): the Load more button is
    // the whole feature, exactly as it was.
    if (!el || typeof IntersectionObserver === "undefined") return
    observer = new IntersectionObserver(onApproach, {
      // The page does not scroll — the card's body does (AppCard `fill`), so
      // the viewport is the wrong box to measure approach against.
      root: card.value?.body ?? null,
      rootMargin: APPROACH_MARGIN,
    })
    observer.observe(el)
  },
  { flush: "post" },
)

onBeforeUnmount(() => {
  observer?.disconnect()
  observer = null
})

/**
 * `loadMore` already refuses a second call while one is in flight, so the guard
 * here is `moreError`: a failed auto-fetch leaves the sentinel on screen, and
 * without it the next scroll nudge would retry the same failing request for as
 * long as the user kept moving. After a failure the page waits for the Load
 * more button, which clears the error and re-opens this path.
 */
function onApproach(entries: IntersectionObserverEntry[]) {
  if (!entries.some((entry) => entry.isIntersecting)) return
  if (!logs.nextCursor || logs.loadingMore || logs.moreError) return
  void loadMore()
}

onMounted(() => void load())

async function load() {
  const uid = user.value?.id ?? null
  setLogsUserId(uid)
  customProviders.setUserId(uid)
  cli.setUserId(uid)
  now.value = new Date()
  // The pickers' sources are side dishes: the rows must not wait on them, and
  // a failure there leaves the pickers working without names or counts.
  void customProviders.load()
  void cli.load()
  void loadFacets()
  await loadLogs()
}

async function onRefresh() {
  manualRefreshing.value = true
  now.value = new Date()
  try {
    await Promise.all([loadLogs({ refresh: true }), loadFacets()])
  } finally {
    manualRefreshing.value = false
  }
}
</script>

<template>
  <div class="page">
    <PageHeader :title="t('logs.title')">
      <template #title>
        <ActionMenu :items="titleMenu" class="title-menu" :aria-label="t('logs.menu')" @select="exporting = true">
          {{ t("logs.title") }}
          <svg class="title-chevron" viewBox="0 0 24 24" aria-hidden="true"><path d="M6 9l6 6 6-6" /></svg>
        </ActionMenu>
      </template>
      <template #actions>
        <ListPicker
          :options="modelOptions"
          :model-value="filterKey(modelFilter)"
          :label="t('logs.picker.model')"
          @update:model-value="onModelPick"
        >
          <template #header>
            <div class="picker-head">
              <span class="picker-title">{{ t("logs.picker.usedToday") }}</span>
              <Segmented
                size="sm"
                :model-value="view"
                :options="viewOptions"
                :label="t('logs.picker.view')"
                @update:model-value="onViewChange"
              />
            </div>
          </template>
          <template #empty>{{ t("logs.picker.empty") }}</template>
        </ListPicker>

        <ListPicker
          :options="providerOptions"
          :model-value="providerFilter ?? ALL"
          :label="t('logs.filter.provider')"
          align="end"
          @update:model-value="onProviderPick"
        />

        <Segmented
          :model-value="errorsOnly ? 'errors' : 'all'"
          :options="showOptions"
          :label="t('logs.filter.show')"
          @update:model-value="onShowChange"
        />

        <div ref="columnsWrap" class="columns" @keydown.escape="columnsOpen = false">
          <AppButton
            variant="secondary"
            :aria-expanded="columnsOpen ? 'true' : 'false'"
            @click="columnsOpen = !columnsOpen"
          >
            {{ t("logs.columns.button") }}
          </AppButton>
          <div v-if="columnsOpen" class="columns-panel">
            <label v-for="key in LOG_COLUMN_KEYS" :key="key" class="columns-item">
              <input
                type="checkbox"
                :checked="!hidden.includes(key)"
                :disabled="LOCKED_COLUMNS.includes(key)"
                @change="toggleColumn(key)"
              />
              <span>{{ t(COLUMN_KEY[key]) }}</span>
            </label>
            <div class="columns-foot">
              <span>{{ t("logs.columns.note") }}</span>
              <button type="button" class="columns-reset" @click="resetColumns">{{ t("logs.columns.reset") }}</button>
            </div>
          </div>
        </div>

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

    <!-- Non-blocking: whatever was painted stays, and the banner offers the one
         control that can fix it. -->
    <Banner v-if="logs.error" tone="warn" class="page-alert">
      {{ t("logs.error.load") }}
      <template #actions>
        <AppButton size="sm" variant="ghost" :loading="manualRefreshing" @click="onRefresh">
          {{ t("action.retry") }}
        </AppButton>
      </template>
    </Banner>

    <AppCard ref="card" fill flush class="list">
      <!-- Skeletons are decoration; the status beside them is what a screen
           reader gets. -->
      <div v-if="showSkeleton" class="skeletons">
        <span class="sr-only" role="status">{{ t("app.loading") }}</span>
        <div v-for="i in 8" :key="i" class="skeleton-row" aria-hidden="true">
          <span class="skeleton skeleton-time" />
          <span class="skeleton skeleton-model" />
          <span class="skeleton skeleton-meta" />
        </div>
      </div>

      <EmptyState
        v-else-if="!rows.length && isFiltered"
        :title="t('logs.noResults.title')"
        :body="t('logs.noResults.body')"
      />

      <EmptyState
        v-else-if="!rows.length"
        :title="t('logs.empty.title')"
        :body="t('logs.empty.body')"
      />

      <template v-else>
        <!-- The whole row opens the detail; the time cell stays a real button,
             so keyboard and screen-reader users reach it the same way. -->
        <DataTable
          row-clickable
          fixed
          resizable
          :resize-label="(header) => t('logs.columns.resize', { column: header })"
          :group-by="dayLabel"
          :columns="columns"
          :rows="rows"
          :row-key="(row) => row.id"
          :caption="t('logs.title')"
          @row-click="detail = $event"
          @resize="onResize"
        >
          <template #cell-time="{ row }">
            <AppButton
              size="sm"
              variant="ghost"
              class="time"
              :label="t('logs.openDetail', { time: timeLabel(row) })"
              @click.stop="detail = row"
            >
              <span class="mono" :title="timeLabel(row)">{{ format.clock(row.created_at) }}</span>
            </AppButton>
          </template>

          <!-- Inline content, not a flex box: the ellipsis cuts the line as a
               whole, so the provider and alias go first when it runs out. -->
          <template #cell-model="{ row }">
            <span :title="rowModelTitle(row)">
              <span class="dot" :style="{ background: rowColor(row) }" aria-hidden="true" />
              <code class="mono model-id">{{ rowPrimary(row) }}</code>
              <span class="model-sub">{{ rowSecondary(row) }}</span>
            </span>
          </template>

          <!-- A removed record keeps its last name and gains the tag; a row
               older than the name snapshot has only the tag to show. A
               borrowed account is named by its lender and tagged Shared. -->
          <template #cell-account="{ row }">
            <span
              v-if="row.account_label"
              class="account"
              :title="row.account_shared_by ? t('logs.sharedBy', { owner: row.account_shared_by }) : row.account_label"
            >
              {{ row.account_label }}
            </span>
            <Badge v-if="row.account_removed" tone="warn" class="tag">
              {{ row.account_label ? t("logs.removed") : t("logs.accountRemoved") }}
            </Badge>
            <Badge v-else-if="row.account_shared_by" tone="neutral" class="tag" :title="t('logs.sharedBy', { owner: row.account_shared_by })">
              {{ t("logs.shared") }}
            </Badge>
            <span v-else-if="!row.account_label" class="none">—</span>
          </template>

          <template #cell-apiKey="{ row }">
            <span v-if="row.api_key_name" class="api-key" :title="row.api_key_name">
              {{ row.api_key_name }}
            </span>
            <Badge v-if="row.api_key_removed" tone="warn" class="tag">
              {{ row.api_key_name ? t("logs.removed") : t("logs.keyRemoved") }}
            </Badge>
            <span v-else-if="!row.api_key_name" class="none">—</span>
          </template>

          <!-- The number and nothing else; a failure is the same number in
               the danger tone, named in full on the title and in the detail. -->
          <template #cell-status="{ row }">
            <span
              class="mono"
              :class="isFailure(row) ? 'status-failed' : 'status'"
              :title="row.error_code ?? undefined"
            >{{ row.status_code }}</span>
          </template>
        </DataTable>

        <!-- The list's end: one more page, appended in place.
             The sentinel makes that automatic and the button keeps it
             reachable — by keyboard, without an IntersectionObserver, and
             after a fetch that failed. Both live under the one condition, so
             they exist exactly while there is a next page to ask for. -->
        <template v-if="logs.nextCursor">
          <div ref="sentinel" class="sentinel" aria-hidden="true" />
          <div class="more">
            <p v-if="logs.moreError" class="more-error">{{ t("logs.error.loadMore") }}</p>
            <AppButton
              size="sm"
              variant="secondary"
              :loading="logs.loadingMore"
              @click="loadMore()"
            >
              {{ t("logs.loadMore") }}
            </AppButton>
          </div>
        </template>
      </template>
    </AppCard>

    <LogDetailModal v-if="detail" :row="detail" @close="detail = null" />
    <LogExportDialog
      v-if="exporting"
      :provider="providerFilter"
      :model-filter="modelFilter"
      :errors-only="errorsOnly"
      :filter-summary="exportSummary"
      :columns="visibleKeys"
      @close="exporting = false"
    />
  </div>
</template>

<style scoped>
/*
 * The page is exactly the content region minus its padding, which is what
 * gives `AppCard fill` a bounded box to scroll the rows inside of instead of
 * growing the page (docs/admin-ui.md § Anti-scroll rules).
 *
 * Every value here is AppShell's own, inherited rather than restated: it owns
 * the padding and knows how much chrome sits above this region, and a second
 * copy would drift the first time only one of them changed breakpoint.
 */
.page {
  display: flex;
  flex-direction: column;
  height: calc(
    100dvh - var(--page-chrome, 0px) - var(--page-top, var(--space-6)) -
      var(--page-bottom, var(--space-12))
  );
}

.page-alert {
  flex-shrink: 0;
  margin-bottom: var(--space-4);
}

.list {
  flex: 1;
  min-height: 0;
}

/* The title reads as the title, with a chevron saying it opens something. */
.title-menu {
  margin-left: calc(var(--space-2) * -1);
  padding: 2px var(--space-2);
  border-radius: var(--radius-control);
  transition: background var(--duration-fast) var(--ease);
}

.title-menu:hover,
.title-menu[aria-expanded="true"] {
  background: var(--hover);
}

.title-chevron {
  width: 16px;
  height: 16px;
  fill: none;
  stroke: var(--muted);
  stroke-width: 2.2;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.picker-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-3);
}

.picker-title {
  font-size: var(--text-xs);
  font-weight: var(--weight-semibold);
  color: var(--text-secondary);
}

/* --- Columns checklist --------------------------------------------------- */

.columns {
  position: relative;
}

.columns-panel {
  position: absolute;
  top: calc(100% + 6px);
  right: 0;
  z-index: 20;
  width: 220px;
  padding: var(--space-1);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--surface);
  box-shadow: var(--shadow-lg);
}

.columns-item {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  height: 32px;
  padding: 0 var(--space-2);
  border-radius: var(--radius-sm);
  font-size: var(--text-sm);
  color: var(--text);
  cursor: pointer;
}

.columns-item:hover {
  background: var(--hover);
}

.columns-item input {
  accent-color: var(--accent);
}

.columns-item:has(input:disabled) {
  color: var(--muted);
  cursor: default;
}

.columns-foot {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-2);
  margin-top: var(--space-1);
  padding: var(--space-2) var(--space-2) var(--space-1);
  border-top: 1px solid var(--border);
  font-size: var(--text-2xs);
  color: var(--muted);
}

.columns-reset {
  padding: 0;
  border: none;
  background: none;
  color: var(--text);
  font: inherit;
  font-size: var(--text-xs);
  text-decoration: underline;
  cursor: pointer;
}

/* --- Rows --------------------------------------------------------------- */

/*
 * The timestamp is the row's control, so it reads as the row's first field
 * rather than as a button parked in the column: no padding of its own, and the
 * line box of the cell around it.
 */
.list :deep(.time) {
  height: auto;
  padding: 0;
  justify-content: flex-start;
  color: var(--text-secondary);
  font-size: var(--text-sm);
  line-height: inherit;
  vertical-align: top;
}

.dot {
  display: inline-block;
  width: 8px;
  height: 8px;
  margin-right: var(--space-2);
  border-radius: var(--radius-full);
  vertical-align: 1px;
}

.model-id {
  color: var(--text);
  font-weight: var(--weight-medium);
}

.model-sub {
  margin-left: var(--space-2);
  color: var(--muted);
  font-size: var(--text-xs);
}

.account,
.api-key {
  color: var(--text-secondary);
}

.tag {
  margin-left: var(--space-1);
}

/* A successful status is context, not a finding — the failures beside it are
   what the eye should catch. */
.status {
  color: var(--faint);
}

.status-failed {
  color: var(--danger);
  font-weight: var(--weight-medium);
}

.none {
  color: var(--faint);
}

/* --- Load more ----------------------------------------------------------- */

/* What the observer watches for. It has to occupy a box to be intersected, and
   it must not cost the layout one: the negative margin gives its pixel back. */
.sentinel {
  height: 1px;
  margin-bottom: -1px;
}

.more {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: var(--space-3);
  flex-wrap: wrap;
  padding: var(--space-4);
  border-top: 1px solid var(--border);
}

.more-error {
  margin: 0;
  color: var(--warn);
  font-size: var(--text-xs);
}

/* --- First paint -------------------------------------------------------- */

/* Shaped like a table row — a time, an id, and a run of figures — so the
   layout does not jump when the first page lands. Static, not pulsing: a
   240ms loop is a strobe. */
.skeletons {
  display: grid;
  gap: var(--space-4);
  padding: var(--space-5) var(--space-4);
}

.skeleton-row {
  display: grid;
  grid-template-columns: 120px minmax(0, 1.6fr) minmax(0, 1fr);
  gap: var(--space-4);
}

.skeleton {
  display: block;
  height: var(--text-sm);
  border-radius: var(--radius-full);
  background: var(--hover);
}

.skeleton-time {
  width: 100%;
}

.skeleton-model {
  width: 80%;
}

.skeleton-meta {
  width: 60%;
}
</style>
