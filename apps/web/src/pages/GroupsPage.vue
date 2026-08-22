<script setup lang="ts">
/**
 * Model groups: each one a virtual endpoint — a `/g/<slug>/…` base URL of its
 * own, and the models callable on it (docs/admin-ui.md § Groups page).
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
import { groupBaseUrls, listModels } from "@/services/api"
import {
  CACHE_TTL_MS,
  isModelsCacheFresh,
  readModelsCache,
  writeModelsCache,
} from "@/services/cache"
import type {
  CatalogModel,
  ModelGroup,
  ModelGroupModel,
  ModelGroupTarget,
  ModelGroupTargetRouting,
} from "@/types"

const { t, format } = useI18n()
const { user } = useAuth()
const groups = useModelGroups()

const catalog = ref<CatalogModel[]>([])
const error = ref<string | null>(null)
const showDialog = ref(false)
const editing = ref<ModelGroup | null>(null)
/**
 * Which copyable last landed on the clipboard, keyed so a model name in one
 * group and the same name in another never share a check mark.
 */
const copiedKey = ref<string | null>(null)

let copyTimer: number | undefined

const rows = computed(() => groups.state.data ?? [])
const showSkeleton = computed(() => groups.state.loading && !groups.state.data)

/**
 * Four data columns plus the edit control at the far right, which gets a
 * declared width and its own unlabelled track rather than riding beside the
 * name (docs/admin-ui.md § Component primitives). Endpoint is v4's new
 * column: where a client points; Models is what it may send there.
 */
const columns = computed<Column<ModelGroup>[]>(() => [
  { key: "name", header: t("groups.column.name"), width: "16%" },
  { key: "endpoint", header: t("groups.column.endpoint"), width: "22%" },
  { key: "models", header: t("groups.column.models") },
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

/**
 * Current-route indicator, per model (docs/admin-ui.md § Groups page).
 * `routing` is index-aligned with the model's `targets` and computed from the
 * same stored facts dispatch uses, so it says what the next request would
 * actually do. Optional throughout: a cache entry written before the field
 * existed has none, and the rows then render without the markers.
 */
function routingFor(model: ModelGroupModel, index: number): ModelGroupTargetRouting | null {
  return model.routing?.targets?.[index] ?? null
}

function isCurrentTarget(model: ModelGroupModel, index: number): boolean {
  return model.routing?.current_target_index === index
}

function isUnusable(model: ModelGroupModel, index: number): boolean {
  return routingFor(model, index)?.usable === false
}

/**
 * Why this target cannot take a request, in the user's own terms — the text is
 * the state, the warning tone only reinforces it.
 */
function unusableReason(model: ModelGroupModel, target: ModelGroupTarget, index: number): string | null {
  const routing = routingFor(model, index)
  if (!routing) return isMissingAccount(target) ? t("groups.account.skipped") : null
  if (routing.usable) return null

  const until = routing.unusable_until
  switch (routing.reason) {
    case "limit":
      return until
        ? t("groups.route.limitUntil", { when: format.relative(until) })
        : t("groups.route.unavailable")
    case "benched":
      return until
        ? t("groups.route.pausedUntil", { when: format.relative(until) })
        : t("groups.route.unavailable")
    case "unresolved":
      return t("groups.route.unresolved")
    // A pin whose account is gone is the same fact the account tag beside it
    // already names, so that case keeps the note that says how to fix it.
    case "no_account":
      return isMissingAccount(target) ? t("groups.account.skipped") : t("groups.route.noAccount")
    default:
      return t("groups.route.unavailable")
  }
}

/** Exact recovery time behind the relative one, same as the Updated column. */
function unusableTitle(model: ModelGroupModel, index: number): string | undefined {
  const until = routingFor(model, index)?.unusable_until
  return until ? format.dateTime(until) : undefined
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
 * failure here leaves the page working.
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
    /* keep whatever is painted — the dialog picker degrades gracefully */
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
 * Two kinds of copyable per row: the endpoint base URLs (what goes in a
 * client's `base_url`) and each model name (what goes in `model`). Both are
 * confirmed in place by the chip's icon swapping to a check, like the Models
 * page rows. The display name is a label and copies nothing.
 */
async function copyValue(key: string, value: string) {
  try {
    await navigator.clipboard.writeText(value)
    copiedKey.value = key
    window.clearTimeout(copyTimer)
    copyTimer = window.setTimeout(() => {
      if (copiedKey.value === key) copiedKey.value = null
    }, 1600)
  } catch {
    error.value = t("state.copyFailed")
  }
}

/** The two base URLs a group's slug produces, labeled by wire shape. */
function endpointUrls(group: ModelGroup): Array<{ key: string; label: string; url: string }> {
  const urls = groupBaseUrls(group.slug)
  return [
    { key: `url:${group.id}:openai`, label: "OpenAI", url: urls.openai },
    { key: `url:${group.id}:anthropic`, label: "Anthropic", url: urls.anthropic },
  ]
}
</script>

<template>
  <div class="page">
    <PageHeader :title="t('groups.title')">
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

      <!-- No action slot: Create group is in the sticky header, present at every
           scroll depth and in every state of the page (docs/admin-ui.md
           § Component primitives). -->
      <EmptyState
        v-else-if="!rows.length"
        :title="t('groups.empty.title')"
        :body="t('groups.empty.body')"
      />

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

        <!-- The endpoint identity: the slug, then one copy chip per base URL —
             a base URL is exactly what goes in a client's base_url setting, so
             copying it is this column's primary action. -->
        <template #cell-endpoint="{ row }">
          <div class="endpoint">
            <code class="mono slug" :title="`/g/${row.slug}/`">{{ row.slug }}</code>
            <ul class="urls">
              <li v-for="entry in endpointUrls(row)" :key="entry.key">
                <AppButton
                  size="sm"
                  variant="ghost"
                  class="copy-chip"
                  :class="{ copied: copiedKey === entry.key }"
                  :label="t('groups.copyUrl', { url: entry.url })"
                  :title="entry.url"
                  @click="copyValue(entry.key, entry.url)"
                >
                  <template #icon>
                    <ActionIcon :name="copiedKey === entry.key ? 'check' : 'copy'" />
                  </template>
                  <span class="chip-text">{{ entry.label }}</span>
                </AppButton>
              </li>
            </ul>
          </div>
        </template>

        <!-- One line per group model: the copyable name (exactly what a client
             sends as `model` on this endpoint), then its ordered targets with
             the per-model current-route facts. -->
        <template #cell-models="{ row }">
          <ul class="models">
            <li v-for="model in row.models ?? []" :key="model.name" class="model">
              <AppButton
                size="sm"
                variant="ghost"
                class="copy-chip model-chip"
                :class="{ copied: copiedKey === `model:${row.id}:${model.name}` }"
                :label="t('groups.copyModel', { model: model.name })"
                @click="copyValue(`model:${row.id}:${model.name}`, model.name)"
              >
                <template #icon>
                  <ActionIcon
                    :name="copiedKey === `model:${row.id}:${model.name}` ? 'check' : 'copy'"
                  />
                </template>
                <span class="mono chip-text">{{ model.name }}</span>
              </AppButton>

              <ol class="targets">
                <li
                  v-for="(target, index) in model.targets"
                  :key="targetKey(target, index)"
                  class="target"
                  :class="{ unusable: isUnusable(model, index) }"
                >
                  <span class="pos tabular">{{ index + 1 }}</span>
                  <span class="target-body">
                    <code class="mono">{{ target.model }}</code>
                    <span class="facts">
                      <!-- A pin whose account is gone still has to say which
                           slot is broken — there is no label left to print. -->
                      <Badge v-if="isMissingAccount(target)" tone="warn">
                        {{ t("groups.account.missing") }}
                      </Badge>
                      <Badge v-else-if="target.account_label" tone="neutral">
                        {{ target.account_label }}
                      </Badge>
                      <Badge v-else tone="neutral">{{ t("groups.account.any") }}</Badge>

                      <!-- What the next request would actually do. -->
                      <Badge v-if="isCurrentTarget(model, index)" tone="accent">
                        {{ t("groups.route.current") }}
                      </Badge>
                      <span
                        v-if="unusableReason(model, target, index)"
                        class="fact-note"
                        :title="unusableTitle(model, index)"
                      >
                        {{ unusableReason(model, target, index) }}
                      </span>
                    </span>
                  </span>
                </li>
              </ol>
            </li>
          </ul>
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
        {{ copiedKey ? t("action.copied") : "" }}
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

/* The endpoint cell: the slug names the group's URL identity, the chips below
   it copy the two full base URLs — labeled by wire shape, full value on the
   title, so the column stays scannable at any hostname length. */
.endpoint {
  display: flex;
  flex-direction: column;
  gap: var(--space-1);
  min-width: 0;
}

.slug {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text);
  font-size: var(--text-sm);
}

.urls {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-1);
  margin: 0;
  padding: 0;
  min-width: 0;
  list-style: none;
}

/* The button is the chip: quiet by default and full strength under the pointer,
   so a column of copyables still reads as data rather than a stack of buttons.
   Always *present* though — never revealed on hover. Selected through the card
   so these outrank AppButton's own single-class rules. */
.list :deep(.copy-chip) {
  max-width: 100%;
  padding: 0 var(--space-2) 0 var(--space-1);
  color: var(--faint);
}

.list :deep(.copy-chip:hover),
.list :deep(.copy-chip.copied) {
  color: var(--text);
}

/* The label is a flex item, so it needs its own zero floor before the block
   inside it can ellipsize. */
.list :deep(.copy-chip .btn-label) {
  min-width: 0;
  overflow: hidden;
}

.chip-text {
  display: block;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text);
  font-size: var(--text-sm);
}

/* One block per group model: the copyable name, then that model's ordered
   targets indented beneath it — the indent is what keeps a multi-model row
   readable as name → its routes, not one long run of chips. */
.models {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
  margin: 0;
  padding: 0;
  min-width: 0;
  list-style: none;
}

.model {
  display: flex;
  flex-direction: column;
  gap: var(--space-1);
  min-width: 0;
}

.targets {
  display: flex;
  flex-direction: column;
  gap: var(--space-1);
  margin: 0;
  padding: 0 0 0 var(--space-4);
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

/* A target that cannot take a request reads quieter than the ones that can.
   The reason text beside it is what states that; the tone only reinforces it,
   so the row still says everything with the colors stripped out. */
.target.unusable .mono {
  color: var(--faint);
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
</style>
