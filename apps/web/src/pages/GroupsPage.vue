<script setup lang="ts">
/**
 * Model groups: the names the user invented, and the real models each one
 * stands for (docs/admin-ui.md § Groups page).
 *
 * One bounded card filling the content region, list scrolling inside it — the
 * Keys-page shape rather than the Providers edit gate: groups are few, and
 * editing them is the whole point of the page, so the pencil sits on every row
 * instead of behind a section toggle.
 *
 * The catalog is loaded alongside the list because the dialog's target picker
 * filters it client-side. Both are cache-first, and the catalog shares the
 * Models page's own cache entry, so arriving here right after Models costs
 * nothing.
 */
import { computed, onMounted, ref } from "vue"
import ModelGroupDialog from "@/components/ModelGroupDialog.vue"
import ActionIcon from "@/components/ui/ActionIcon.vue"
import AppButton from "@/components/ui/AppButton.vue"
import AppCard from "@/components/ui/AppCard.vue"
import Badge from "@/components/ui/Badge.vue"
import Banner from "@/components/ui/Banner.vue"
import DataTable from "@/components/ui/DataTable.vue"
import type { Column } from "@/components/ui/DataTable.vue"
import EmptyState from "@/components/ui/EmptyState.vue"
import PageHeader from "@/components/ui/PageHeader.vue"
import { useAuth } from "@/composables/useAuth"
import { useModelGroups } from "@/composables/useModelGroups"
import { useI18n } from "@/i18n"
import { listModels } from "@/services/api"
import {
  CACHE_TTL_MS,
  isModelsCacheFresh,
  readModelsCache,
  writeModelsCache,
} from "@/services/cache"
import type { CatalogModel, ModelGroup, ModelGroupTarget } from "@/types"

const { t, format } = useI18n()
const { user } = useAuth()
const groups = useModelGroups()

const catalog = ref<CatalogModel[]>([])
const error = ref<string | null>(null)
const showDialog = ref(false)
const editing = ref<ModelGroup | null>(null)
/** Which alias last landed on the clipboard — aliases are unique per user, so the id is enough. */
const copiedAlias = ref<string | null>(null)

let copyTimer: number | undefined

const rows = computed(() => groups.state.data ?? [])
const showSkeleton = computed(() => groups.state.loading && !groups.state.data)

/**
 * Four data columns plus the edit control at the far right, which gets a
 * declared width and its own unlabelled track rather than riding beside the
 * name (docs/admin-ui.md § Component primitives). Name is the label the user
 * gave the group; the callable ids are the Aliases beside it.
 */
const columns = computed<Column<ModelGroup>[]>(() => [
  { key: "name", header: t("groups.column.name"), width: "20%" },
  { key: "aliases", header: t("groups.column.aliases"), width: "26%" },
  { key: "targets", header: t("groups.column.targets") },
  { key: "updated", header: t("groups.column.updated"), width: "132px" },
  { key: "edit", header: "", srHeader: t("action.edit"), align: "end", width: "56px" },
])

/**
 * The same model may appear twice on two different accounts, so the row key is
 * the pair — behind its position, which is the one part that stays unique
 * whatever the list holds.
 */
function targetKey(target: ModelGroupTarget, index: number): string {
  return `${index}:${target.model}:${target.account_id ?? ""}`
}

/**
 * A pinned account the server could not resolve at read time: `account_id` set,
 * no label. The target is skipped at request time (docs/providers.md § Model
 * groups), so the row says so rather than showing the group as healthy.
 */
function isMissingAccount(target: ModelGroupTarget): boolean {
  return !!target.account_id && !target.account_label
}

onMounted(() => void load())

async function load() {
  const uid = user.value?.id ?? null
  error.value = null
  groups.setUserId(uid)

  await Promise.all([loadGroups(), loadCatalog()])
}

async function loadGroups() {
  await groups.load()
  if (groups.state.error) error.value = t("groups.error.load")
}

/**
 * Cache-first over the shared models entry: the picker only needs ids, and a
 * failure here leaves the page working — free-text entry covers every id the
 * catalog would have offered.
 */
async function loadCatalog() {
  const uid = user.value?.id ?? null
  const cached = readModelsCache(uid)
  if (cached) catalog.value = cached.data
  if (cached && isModelsCacheFresh(uid, CACHE_TTL_MS)) return

  try {
    const res = await listModels()
    catalog.value = res.data
    writeModelsCache(uid, res)
  } catch {
    /* keep whatever is painted — the dialog still takes typed ids */
  }
}

function openCreate() {
  editing.value = null
  showDialog.value = true
}

function openEdit(group: ModelGroup) {
  editing.value = group
  showDialog.value = true
}

function closeDialog() {
  showDialog.value = false
  editing.value = null
}

async function onSaved() {
  await groups.load({ refresh: true })
  error.value = groups.state.error ? t("groups.error.load") : null
}

/**
 * An alias *is* a model id a client sends, so copying one is the row's primary
 * read action — confirmed in place by the chip's icon swapping to a check, like
 * the Models page rows. The display name is a label and copies nothing.
 */
async function copyAlias(alias: string) {
  try {
    await navigator.clipboard.writeText(alias)
    copiedAlias.value = alias
    window.clearTimeout(copyTimer)
    copyTimer = window.setTimeout(() => {
      if (copiedAlias.value === alias) copiedAlias.value = null
    }, 1600)
  } catch {
    error.value = t("state.copyFailed")
  }
}
</script>

<template>
  <div class="page">
    <PageHeader :title="t('groups.title')" :subtitle="t('groups.subtitle')">
      <template #actions>
        <AppButton variant="primary" @click="openCreate">
          <template #icon><ActionIcon name="plus" /></template>
          {{ t("groups.create") }}
        </AppButton>
      </template>
    </PageHeader>

    <Banner v-if="error" tone="error" class="page-alert">
      {{ error }}
      <template #actions>
        <AppButton size="sm" variant="ghost" @click="error = null">
          {{ t("action.dismiss") }}
        </AppButton>
      </template>
    </Banner>

    <AppCard fill flush class="list">
      <!-- Skeletons are decoration; the status beside them is what a screen
           reader gets. -->
      <div v-if="showSkeleton" class="skeletons">
        <span class="sr-only" role="status">{{ t("app.loading") }}</span>
        <div v-for="i in 3" :key="i" class="skeleton-row" aria-hidden="true">
          <span class="skeleton skeleton-name" />
          <span class="skeleton skeleton-meta" />
        </div>
      </div>

      <EmptyState
        v-else-if="!rows.length"
        :title="t('groups.empty.title')"
        :body="t('groups.empty.body')"
      >
        <template #action>
          <AppButton variant="primary" @click="openCreate">
            {{ t("groups.create") }}
          </AppButton>
        </template>
      </EmptyState>

      <DataTable
        v-else
        :columns="columns"
        :rows="rows"
        :row-key="(g) => g.id"
        :caption="t('groups.title')"
      >
        <!-- A label, not an id: plain text, nothing to copy. -->
        <template #cell-name="{ row }">
          <span class="name">{{ row.name }}</span>
        </template>

        <!-- Every alias is a model id a client can send, so every chip is a
             copy control — the accessible name spells out which one it copies,
             because "Copy" repeats down the whole column. -->
        <template #cell-aliases="{ row }">
          <ul class="aliases">
            <li v-for="alias in row.aliases" :key="alias">
              <AppButton
                size="sm"
                variant="ghost"
                class="alias-copy"
                :class="{ copied: copiedAlias === alias }"
                :label="t('groups.copyAlias', { alias })"
                @click="copyAlias(alias)"
              >
                <template #icon>
                  <ActionIcon :name="copiedAlias === alias ? 'check' : 'copy'" />
                </template>
                <span class="mono alias">{{ alias }}</span>
              </AppButton>
            </li>
          </ul>
        </template>

        <!-- Priority order, numbered: the position is the routing rule, so it
             is real text in the row — visible when the card layout takes over
             below 768px, and read out where `list-style: none` costs Safari
             its list semantics.

             Each entry is position + model + its account, the account as a tag
             so the later balancing facts (weight, live usage) join it as more
             tags on the same line instead of forcing a new shape. -->
        <template #cell-targets="{ row }">
          <ol class="targets">
            <li
              v-for="(target, index) in row.targets"
              :key="targetKey(target, index)"
              class="target"
            >
              <span class="pos tabular">{{ index + 1 }}</span>
              <span class="target-body">
                <code class="mono">{{ target.model }}</code>
                <span class="facts">
                  <!-- A pin whose account is gone: warned, and told what it
                       costs — that target is skipped at request time. -->
                  <template v-if="isMissingAccount(target)">
                    <Badge tone="warn">{{ t("groups.account.missing") }}</Badge>
                    <span class="fact-note">{{ t("groups.account.skipped") }}</span>
                  </template>
                  <Badge v-else-if="target.account_label" tone="neutral">
                    {{ target.account_label }}
                  </Badge>
                  <Badge v-else tone="neutral">{{ t("groups.account.any") }}</Badge>
                </span>
              </span>
            </li>
          </ol>
        </template>

        <template #cell-updated="{ row }">
          <span :title="format.dateTime(row.updated_at)">
            {{ format.relative(row.updated_at) }}
          </span>
        </template>

        <template #cell-edit="{ row }">
          <AppButton
            size="sm"
            variant="ghost"
            icon-only
            :label="t('groups.editGroup', { name: row.name })"
            @click="openEdit(row)"
          >
            <template #icon><ActionIcon name="edit" /></template>
          </AppButton>
        </template>
      </DataTable>

      <span class="sr-only" role="status" aria-live="polite">
        {{ copiedAlias ? t("action.copied") : "" }}
      </span>
    </AppCard>

    <ModelGroupDialog
      v-if="showDialog"
      :group="editing"
      :catalog="catalog"
      @close="closeDialog"
      @saved="onSaved"
    />
  </div>
</template>

<style scoped>
/*
 * The page fills the content region exactly, which is what gives
 * `AppCard fill` a bounded box to scroll the list inside of instead of growing
 * the page (docs/admin-ui.md § Anti-scroll rules).
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

/* --- Rows --------------------------------------------------------------- */

.name {
  display: block;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text);
  font-size: var(--text-sm);
}

/* A group answers to several ids, so the cell is a list of them — wrapped, not
   truncated to the first: which id a client may send is the question this
   column exists to answer. */
.aliases {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-1);
  margin: 0;
  padding: 0;
  min-width: 0;
  list-style: none;
}

/* The button is the chip: quiet by default and full strength under the pointer,
   so a column of ids still reads as ids rather than as a stack of buttons.
   Always *present* though — never revealed on hover, which would read as the
   control having disappeared. Selected through the card so these outrank
   AppButton's own single-class rules rather than depending on style order. */
.list :deep(.alias-copy) {
  max-width: 100%;
  padding: 0 var(--space-2) 0 var(--space-1);
  color: var(--faint);
}

.list :deep(.alias-copy:hover),
.list :deep(.alias-copy.copied) {
  color: var(--text);
}

/* The label is a flex item, so it needs its own zero floor before the block
   inside it can ellipsize. */
.list :deep(.alias-copy .btn-label) {
  min-width: 0;
  overflow: hidden;
}

.alias {
  display: block;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text);
  font-size: var(--text-sm);
}

.targets {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-1) var(--space-3);
  margin: 0;
  padding: 0;
  list-style: none;
}

.target {
  display: flex;
  align-items: baseline;
  gap: var(--space-2);
  min-width: 0;
}

/* The model and its facts travel together, so a wrapped run never leaves an
   account tag stranded under the *next* target's id. */
.target-body {
  display: inline-flex;
  align-items: baseline;
  flex-wrap: wrap;
  gap: var(--space-1) var(--space-2);
  min-width: 0;
}

/* One tag today (the account); weight and live usage join it here when
   balancing lands, which is why it is a container of its own rather than a tag
   dropped straight beside the id. */
.facts {
  display: inline-flex;
  align-items: baseline;
  flex-wrap: wrap;
  gap: var(--space-1);
  min-width: 0;
}

.fact-note {
  color: var(--warn);
  font-size: var(--text-2xs);
}

.pos {
  color: var(--faint);
  font-size: var(--text-2xs);
}

/* --- First paint -------------------------------------------------------- */

.skeletons {
  display: grid;
  gap: var(--space-4);
  padding: var(--space-5) var(--space-4);
}

.skeleton-row {
  display: grid;
  grid-template-columns: minmax(0, 1fr) minmax(0, 1.4fr);
  gap: var(--space-4);
}

.skeleton {
  display: block;
  height: var(--text-sm);
  border-radius: var(--radius-full);
  background: var(--hover);
}

.skeleton-name {
  width: 60%;
}

.skeleton-meta {
  width: 85%;
}

/* Below the table breakpoint each row is a card, so the targets read as a
   numbered stack rather than a wrapped run of ids. */
@media (max-width: 768px) {
  .targets {
    flex-direction: column;
    align-items: flex-start;
    gap: var(--space-1);
  }
}
</style>
