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
 * sticky header and a Load more control at the end — the Keys/Models shape.
 * Both filters are applied server-side, so changing either reloads from the
 * first page rather than narrowing what is already painted.
 */
import { computed, onMounted, ref } from "vue"
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

const columns = computed<Column<RequestLogRow>[]>(() => [
  { key: "time", header: t("logs.column.time"), width: "150px" },
  { key: "model", header: t("logs.column.model") },
  { key: "account", header: t("logs.column.account"), width: "16%" },
  { key: "type", header: t("logs.column.type"), width: "72px", hideOnMobile: true },
  { key: "status", header: t("logs.column.status"), width: "112px" },
  {
    key: "input",
    header: t("logs.column.input"),
    numeric: true,
    value: (row) => format.compact(row.prompt_tokens),
  },
  {
    key: "cacheRead",
    header: t("logs.column.cacheRead"),
    numeric: true,
    hideOnMobile: true,
    value: (row) => format.compact(row.cache_read_input_tokens),
  },
  {
    key: "cacheWrite",
    header: t("logs.column.cacheWrite"),
    numeric: true,
    hideOnMobile: true,
    value: (row) => format.compact(row.cache_creation_input_tokens),
  },
  {
    key: "output",
    header: t("logs.column.output"),
    numeric: true,
    value: (row) => format.compact(row.completion_tokens),
  },
  {
    key: "cost",
    header: t("logs.column.cost"),
    numeric: true,
    value: (row) => format.currency(row.cost),
  },
  {
    key: "latency",
    header: t("logs.column.latency"),
    numeric: true,
    value: (row) => format.duration(row.latency_ms),
  },
])

onMounted(() => void load())

async function load() {
  const uid = user.value?.id ?? null
  setLogsUserId(uid)
  customProviders.setUserId(uid)
  // The filter list is a side dish: the rows must not wait on it, and a
  // failure there leaves the page working with the builtins alone.
  void customProviders.load()
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

function typeLabel(row: RequestLogRow): string {
  return row.usage_type === "oauth" ? t("logs.type.oauth") : t("logs.type.api")
}
</script>

<template>
  <div class="page">
    <PageHeader :title="t('logs.title')" :subtitle="t('logs.subtitle')">
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

    <AppCard fill flush class="list">
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
              :label="t('logs.openDetail', { time: format.timestamp(row.created_at) })"
              @click.stop="detail = row"
            >
              <span class="tabular">{{ format.timestamp(row.created_at) }}</span>
            </AppButton>
          </template>

          <!-- The alias travels with the model id: it is what the client
               actually sent, and the id is what ran. -->
          <template #cell-model="{ row }">
            <span class="model-cell">
              <code class="mono model-id" :title="row.model">{{ row.model }}</code>
              <Badge v-if="row.group_name" tone="neutral">
                {{ t("logs.via", { alias: row.group_name }) }}
              </Badge>
            </span>
          </template>

          <template #cell-account="{ row }">
            <Badge v-if="isRemovedAccount(row)" tone="warn">
              {{ t("logs.accountRemoved") }}
            </Badge>
            <span v-else-if="row.account_label" class="account">{{ row.account_label }}</span>
            <span v-else class="none">—</span>
          </template>

          <template #cell-type="{ row }">
            <Badge tone="neutral">{{ typeLabel(row) }}</Badge>
          </template>

          <!-- Never color-only: a failure is named by its code, a success is
               its status number and nothing louder. -->
          <template #cell-status="{ row }">
            <Badge v-if="isFailure(row)" tone="danger" mono>{{ failureLabel(row) }}</Badge>
            <span v-else class="status tabular">{{ row.status_code }}</span>
          </template>
        </DataTable>

        <!-- The list's end: one more page, appended in place. -->
        <div v-if="logs.nextCursor" class="more">
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

/* The timestamp is the row's control, so it reads as the row's first field
   rather than as a button parked in the column: no padding of its own, and the
   full text strength the rest of the row's identity has. */
.list :deep(.time) {
  padding: 0;
  color: var(--text);
  font-size: var(--text-sm);
}

/* The id and its alias tag wrap together rather than the tag stranding on a
   line of its own in a narrow column. */
.model-cell {
  display: inline-flex;
  align-items: baseline;
  flex-wrap: wrap;
  gap: var(--space-1) var(--space-2);
  min-width: 0;
}

.model-id {
  display: block;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text);
}

.account {
  display: block;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text-secondary);
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
