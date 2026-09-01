<script setup lang="ts">
/**
 * Logs: every request the user's keys made, newest first (docs/admin-ui.md
 * § Logs page).
 *
 * The per-request companion to Overview's aggregates — the account and alias
 * the By-model table folds away live here, as does the error evidence
 * (`error_code`, `upstream_status`) that otherwise needs a D1 query. Nothing
 * is scoped to live providers: a row from a since-deleted endpoint is still
 * what happened, and hiding it would hurt the diagnosis this page exists for.
 *
 * One bounded card filling the content region, rows scrolling inside it with a
 * sticky header — the Keys/Models shape. The next page loads on approach, with
 * the Load more control at the end kept as the keyboard, no-observer, and
 * after-a-failure path to the same request. Both filters are applied
 * server-side, so changing either reloads from the first page rather than
 * narrowing what is already painted.
 *
 * The card scrolls vertically only. Twelve columns do not fit a laptop's width
 * by their content, so the table is fixed-layout: each column is a share of the
 * card and each cell is clamped to the row's two lines, with the full text on a
 * `title` for the pointer and the row detail for everything else.
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue"
import LogDetailModal from "@/components/LogDetailModal.vue"
import ActionIcon from "@/components/ui/ActionIcon.vue"
import AppButton from "@/components/ui/AppButton.vue"
import AppCard from "@/components/ui/AppCard.vue"
import Badge from "@/components/ui/Badge.vue"
import Banner from "@/components/ui/Banner.vue"
import DataTable from "@/components/ui/DataTable.vue"
import type { Column } from "@/components/ui/DataTable.vue"
import EmptyState from "@/components/ui/EmptyState.vue"
import PageHeader from "@/components/ui/PageHeader.vue"
import Segmented from "@/components/ui/Segmented.vue"
import { useAuth } from "@/composables/useAuth"
import { useCli } from "@/composables/useCli"
import { useCustomProviders } from "@/composables/useCustomProviders"
import { useLogs } from "@/composables/useLogs"
import { useI18n } from "@/i18n"
import type { MessageKey } from "@/i18n"
import { PROVIDERS, type ProviderId, type RequestLogRow } from "@/types"

const { t, format } = useI18n()
const { user } = useAuth()
const {
  state: logs,
  provider: providerFilter,
  errorsOnly,
  setUserId: setLogsUserId,
  load: loadLogs,
  loadMore,
  setProvider,
  setErrorsOnly,
} = useLogs()
const customProviders = useCustomProviders()
const cli = useCli()

/**
 * The "no provider filter" option value. Underscored so it can never collide
 * with a custom endpoint's slug, which is lowercase letters, digits, hyphens.
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

/** Which row's detail dialog is open. */
const detail = ref<RequestLogRow | null>(null)
/** Only the user's own Refresh — a background refresh shows nothing. */
const manualRefreshing = ref(false)

const rows = computed(() => logs.rows)
const showSkeleton = computed(() => logs.loading && rows.value.length === 0)
const isFiltered = computed(() => providerFilter.value !== null || errorsOnly.value)

/**
 * All + the builtins + the user's live custom slugs — the same sources the
 * Models and Providers pages filter by. A slug the user filtered to and has
 * since deleted stays in the list as its own bare option: its rows are still in
 * the log, so a select that silently dropped it would show a filter the page
 * cannot name.
 */
const providerOptions = computed(() => {
  const options = [
    { value: ALL, label: t("logs.filter.allProviders") },
    ...PROVIDERS.map((p) => ({ value: p.id as string, label: t(NAME_KEY[p.id]) })),
    ...(customProviders.state.data ?? []).map((cp) => ({ value: cp.slug, label: cp.name })),
    ...(cli.state.providers ?? []).map((cp) => ({ value: cp.slug, label: cp.name })),
  ]
  const selected = providerFilter.value
  if (selected && !options.some((o) => o.value === selected)) {
    options.push({ value: selected, label: selected })
  }
  return options
})

const showOptions = computed(() => [
  { value: "all", label: t("logs.filter.showAll") },
  { value: "errors", label: t("logs.filter.showErrors") },
])

/*
 * Every track is a percentage and the twelve of them sum to 100: under
 * `DataTable`'s fixed layout a width is the column, not a suggestion, and a
 * pixel width would stop being a share of the card the moment the sidebar
 * collapses or the window changes.
 *
 * 8 Time + 8 Model + 9 Account + 9 API key + 7 Type + 13 Status + 7 Input +
 * 7 Cache read + 7 Cache write + 7 Output + 9 Cost + 9 Latency = 100.
 *
 * The shares are sized to their *headers* first — an uppercase 11px LATENCY
 * needs more room than the "1.2 s" under it, and a header that does not fit is
 * a column nobody can name — then to the values that cannot be shortened
 * ("$0.0042" is four significant digits by design, docs/pricing.md). Time,
 * Model, Account and API key get the remainder because they are the only
 * columns whose text is open-ended, and they are the ones the two-line clamp
 * is for. Account and API key hold the 9% their headers need — ACCOUNT the
 * same seven letters LATENCY already holds at 9%, API KEY six letters plus a
 * space — and Time and Model take the rest at 8% each, where their shorter
 * headers (four and five letters, the class TYPE and INPUT already hold at
 * 7%) still fit on one line.
 *
 * **Status is sized to the longest error code**, because that cell is the one
 * value on the row that has to be read in full to be worth anything — a code
 * cut down to `upstream_unavailabl` names nothing. The full set is in
 * docs/logging.md; the longest are `upstream_unavailable` and
 * `spend_limit_exceeded` at 20 characters. The narrowest the desktop table ever
 * gets is a ~770px viewport (below 768px it is cards instead), which leaves the
 * card body ~752px: 13% of that is ~98px, ~82px inside the cell's padding, or 11
 * characters a line of `.mono` at 0.6em advance — 22 across the row's two lines,
 * against the 20 it has to hold. The three percent that bought it came from
 * Time, Model and Account, whose text was already being clamped at every width.
 *
 * Which is also why only their cells carry a `title`: a figure column is sized
 * to its own widest value, so nothing there is ever cut, and a tooltip
 * repeating the "12.3K" already on screen would fire under the pointer on
 * every row of the table for no reading the row does not already give.
 */
const columns = computed<Column<RequestLogRow>[]>(() => [
  { key: "time", header: t("logs.column.time"), width: "8%" },
  { key: "model", header: t("logs.column.model"), width: "8%" },
  { key: "account", header: t("logs.column.account"), width: "9%" },
  { key: "apiKey", header: t("logs.column.apiKey"), width: "9%" },
  { key: "type", header: t("logs.column.type"), width: "7%", hideOnMobile: true },
  { key: "status", header: t("logs.column.status"), width: "13%" },
  {
    key: "input",
    header: t("logs.column.input"),
    numeric: true,
    width: "7%",
    value: (row) => format.compact(row.prompt_tokens),
  },
  {
    key: "cacheRead",
    header: t("logs.column.cacheRead"),
    numeric: true,
    width: "7%",
    hideOnMobile: true,
    value: (row) => format.compact(row.cache_read_input_tokens),
  },
  {
    key: "cacheWrite",
    header: t("logs.column.cacheWrite"),
    numeric: true,
    width: "7%",
    hideOnMobile: true,
    value: (row) => format.compact(row.cache_creation_input_tokens),
  },
  {
    key: "output",
    header: t("logs.column.output"),
    numeric: true,
    width: "7%",
    value: (row) => format.compact(row.completion_tokens),
  },
  {
    key: "cost",
    header: t("logs.column.cost"),
    numeric: true,
    width: "9%",
    value: (row) => format.currency(row.cost),
  },
  {
    key: "latency",
    header: t("logs.column.latency"),
    numeric: true,
    width: "9%",
    value: (row) => format.duration(row.latency_ms),
  },
])

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
  // The filter list is a side dish: the rows must not wait on it, and a
  // failure there leaves the page working with the builtins alone.
  void customProviders.load()
  void cli.load()
  await loadLogs()
}

async function onRefresh() {
  manualRefreshing.value = true
  try {
    await loadLogs({ refresh: true })
  } finally {
    manualRefreshing.value = false
  }
}

function onProviderChange(event: Event) {
  const value = (event.target as HTMLSelectElement).value
  void setProvider(value === ALL ? null : value)
}

function onShowChange(value: string | number) {
  void setErrorsOnly(value === "errors")
}

/**
 * A row failed if the proxy answered 4xx/5xx **or** it carries an error code —
 * an eager stream logs `status_code: 200` with the failure only in the code
 * (docs/logging.md), so neither test alone finds every failure.
 */
function isFailure(row: RequestLogRow): boolean {
  return row.error_code !== null || row.status_code >= 400
}

/** The badge's text on a failed row: the code when there is one, else the status it answered with. */
function failureLabel(row: RequestLogRow): string {
  return row.error_code ?? String(row.status_code)
}

/**
 * An account the server could not name at read time: `account_id` set, no
 * label. It was disconnected after serving this request — which happened all
 * the same, so the row stays and says which part of it is gone.
 */
function isRemovedAccount(row: RequestLogRow): boolean {
  return row.account_id !== null && row.account_label === null
}

/** The row's visible timestamp — also its tooltip and part of its button's name. */
function timeLabel(row: RequestLogRow): string {
  return format.timestamp(row.created_at)
}

function typeLabel(row: RequestLogRow): string {
  return row.usage_type === "oauth" ? t("logs.type.oauth") : t("logs.type.api")
}
</script>

<template>
  <div class="page">
    <PageHeader :title="t('logs.title')">
      <template #actions>
        <!-- The wrapping label is the select's accessible name; the "All
             providers" option carries the same words visually. -->
        <label class="filter">
          <span class="sr-only">{{ t("logs.filter.provider") }}</span>
          <select class="select" :value="providerFilter ?? ALL" @change="onProviderChange">
            <option v-for="option in providerOptions" :key="option.value" :value="option.value">
              {{ option.label }}
            </option>
          </select>
        </label>

        <Segmented
          :model-value="errorsOnly ? 'errors' : 'all'"
          :options="showOptions"
          :label="t('logs.filter.show')"
          @update:model-value="onShowChange"
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
          :columns="columns"
          :rows="rows"
          :row-key="(row) => row.id"
          :caption="t('logs.title')"
          @row-click="detail = $event"
        >
          <template #cell-time="{ row }">
            <AppButton
              size="sm"
              variant="ghost"
              class="time"
              :label="t('logs.openDetail', { time: timeLabel(row) })"
              @click.stop="detail = row"
            >
              <span class="tabular" :title="timeLabel(row)">{{ timeLabel(row) }}</span>
            </AppButton>
          </template>

          <!-- The alias travels with the model id: it is what the client
               actually sent, and the id is what ran. Both are plain inline
               content so they flow through the cell's two lines together —
               a flex box between them would be one unclampable item. -->
          <template #cell-model="{ row }">
            <code class="mono model-id" :title="row.model">{{ row.model }}</code>
            <Badge v-if="row.group_name" tone="neutral" class="alias">
              {{ t("logs.via", { group: row.group_name }) }}
            </Badge>
          </template>

          <template #cell-account="{ row }">
            <Badge v-if="isRemovedAccount(row)" tone="warn">
              {{ t("logs.accountRemoved") }}
            </Badge>
            <span v-else-if="row.account_label" class="account" :title="row.account_label">
              {{ row.account_label }}
            </span>
            <span v-else class="none">—</span>
          </template>

          <template #cell-apiKey="{ row }">
            <Badge v-if="row.api_key_removed" tone="warn">
              {{ t("logs.keyRemoved") }}
            </Badge>
            <span v-else-if="row.api_key_name" class="api-key" :title="row.api_key_name">
              {{ row.api_key_name }}
            </span>
            <span v-else class="none">—</span>
          </template>

          <template #cell-type="{ row }">
            <Badge tone="neutral">{{ typeLabel(row) }}</Badge>
          </template>

          <!-- Never color-only: a failure is named by its code, a success is
               its status number and nothing louder. -->
          <template #cell-status="{ row }">
            <span v-if="isFailure(row)" class="error-code mono" :title="failureLabel(row)">
              {{ failureLabel(row) }}
            </span>
            <span v-else class="status tabular">{{ row.status_code }}</span>
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

/* A real width, sized to the longest provider name, and shrinkable — a fixed
   width alone contributes its full size to the flex line and pushes Refresh
   onto a row of its own (docs/admin-ui.md § Layout). */
.filter {
  display: block;
  width: 160px;
  max-width: 100%;
  min-width: 0;
}

/* The app's control spec, same as the other selects in the app (the key
   dialog's interval, the provider strategy). */
.select {
  width: 100%;
  min-width: 0;
  height: 34px;
  padding: 0 var(--space-3);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-sm);
  background: var(--surface);
  color: var(--text);
  font-size: var(--text-sm);
}

.select:focus {
  border-color: var(--ring-border);
  box-shadow: var(--ring);
  outline: none;
}

/* 16px type so iOS Safari does not zoom the page when the select takes focus. */
@media (pointer: coarse) {
  .select {
    height: 40px;
    font-size: var(--text-md);
  }
}

/* --- Rows --------------------------------------------------------------- */

/*
 * The timestamp is the row's control, so it reads as the row's first field
 * rather than as a button parked in the column: no padding of its own, and the
 * full text strength the rest of the row's identity has.
 *
 * It also has to behave like text inside the clamped cell. A button is 28px
 * tall and `nowrap` by default, which in a fixed track means one line sitting
 * 6px off every other column's first line, with the tail of the timestamp cut
 * flat at the track's edge. Here it inherits the cell's line box instead and
 * wraps into the second line the row already has.
 */
.list :deep(.time) {
  height: auto;
  padding: 0;
  justify-content: flex-start;
  color: var(--text);
  font-size: var(--text-sm);
  line-height: inherit;
  text-align: left;
  white-space: normal;
  /* Its top edge, not its baseline, is what should line up with the cell's
     first line — a two-line timestamp aligned on the baseline of its *first*
     line hangs the second one below the cell and loses it to the clip. */
  vertical-align: top;
}

.model-id {
  color: var(--text);
}

/* Inline spacing, not a flex gap: the tag flows with the id through the cell's
   two lines rather than being an item beside it. */
.alias {
  margin-left: var(--space-2);
}

.account {
  color: var(--text-secondary);
}

.api-key {
  color: var(--text-secondary);
}

/*
 * A failure names itself, as text rather than in a pill. The code word *is* the
 * identification, so the pill added a rounded border and its side padding to a
 * track sized for the code — and being one nowrap item, it could not use the
 * row's second line either, so every failing row read `upstream_unav…` with the
 * border sliced through it. Set as text it breaks inside the token instead
 * (`overflow-wrap: anywhere`, inherited from DataTable's clamped cell) and uses
 * both lines. The colour stays decoration: the word carries the meaning.
 */
.error-code {
  color: var(--danger);
}

/* A successful status is context, not a finding — the failures beside it are
   what the eye should catch. */
.status {
  color: var(--faint);
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
