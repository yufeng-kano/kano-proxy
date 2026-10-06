<script setup lang="ts">
/**
 * Model groups: each one a virtual endpoint — a `/g/<slug>/…` base URL of its
 * own, and the models callable on it (docs/admin-ui.md § Groups page).
 *
 * Master–detail with no card around it: the group list on the left, the
 * selected group on the right with one row per model and its failover targets
 * laid out left to right. The body runs edge to edge under the page header and
 * each pane scrolls on its own. The selection lives in `?g=<slug>`.
 *
 * The catalog is loaded alongside the list because the dialog's target picker
 * filters it client-side. Both are cache-first, and the catalog shares the
 * Models page's own cache entry, so arriving here right after Models costs
 * nothing.
 */
import { computed, nextTick, onMounted, ref } from "vue"
import { useRoute, useRouter } from "vue-router"
import ModelGroupDialog from "@/components/ModelGroupDialog.vue"
import ActionIcon from "@/components/ui/ActionIcon.vue"
import AppButton from "@/components/ui/AppButton.vue"
import Banner from "@/components/ui/Banner.vue"
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

const route = useRoute()
const router = useRouter()
const listEl = ref<HTMLElement | null>(null)
/**
 * Below 768px only one pane shows at a time; this says which. A deep link with
 * `?g=` lands on the detail it names.
 */
const detailOpen = ref(typeof route.query.g === "string")

const selectedSlug = computed(() => (typeof route.query.g === "string" ? route.query.g : null))

/** The query's group, else the first — a stale slug falls back rather than blanking the pane. */
const selectedGroup = computed<ModelGroup | null>(
  () => rows.value.find((g) => g.slug === selectedSlug.value) ?? rows.value[0] ?? null,
)

function selectGroup(group: ModelGroup) {
  detailOpen.value = true
  if (group.slug === selectedSlug.value) return
  void router.replace({ query: { ...route.query, g: group.slug } })
}

function closeDetail() {
  detailOpen.value = false
}

/** ↑/↓ walk the list, moving focus with the selection. */
function onListKeydown(event: KeyboardEvent) {
  if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return
  const current = selectedGroup.value
  if (!current) return
  const index = rows.value.findIndex((g) => g.id === current.id)
  const next = rows.value[index + (event.key === "ArrowDown" ? 1 : -1)]
  if (!next) return
  event.preventDefault()
  selectGroup(next)
  void nextTick(() => {
    listEl.value?.querySelector<HTMLElement>(`[data-group-id="${next.id}"]`)?.focus()
  })
}

/** Targets that cannot take a request right now, across every model in the group. */
function issueCount(group: ModelGroup): number {
  let count = 0
  for (const model of group.models ?? []) {
    model.targets.forEach((target, index) => {
      if (isUnusable(model, index) || isMissingAccount(target)) count++
    })
  }
  return count
}

/** `provider/` and the rest, split on the first slash so the provider can read quieter. */
function splitTarget(model: string): { provider: string; rest: string } {
  const slash = model.indexOf("/")
  return slash < 0
    ? { provider: "", rest: model }
    : { provider: model.slice(0, slash + 1), rest: model.slice(slash + 1) }
}

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

/**
 * Keeps the saved group selected: the edited one by id (its slug may have just
 * changed), a new one as the id that was not there before. A deleted group
 * matches neither, so the selection falls back to the first.
 */
async function onSaved() {
  const editedId = editing.value?.id ?? null
  const before = new Set(rows.value.map((g) => g.id))
  await groups.load({ refresh: true })
  error.value = groups.state.error ? t("groups.error.load") : null

  const saved = editedId
    ? rows.value.find((g) => g.id === editedId)
    : rows.value.find((g) => !before.has(g.id))
  if (saved) {
    void router.replace({ query: { ...route.query, g: saved.slug } })
  } else {
    const query = { ...route.query }
    delete query.g
    detailOpen.value = false
    void router.replace({ query })
  }
}

/**
 * Two kinds of copyable: the endpoint base URLs (what goes in a client's
 * `base_url`) and each model name (what goes in `model`). Both are confirmed
 * in place by the icon swapping to a check, like the Models page rows. The
 * display name is a label and copies nothing.
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

    <!-- Skeletons are decoration; the status beside them is what a screen
         reader gets. -->
    <section v-if="showSkeleton" class="body">
      <span class="sr-only" role="status">{{ t("app.loading") }}</span>
      <div class="list-pane" aria-hidden="true">
        <div v-for="i in 3" :key="i" class="skeleton-item">
          <span class="skeleton skeleton-name" />
          <span class="skeleton skeleton-meta" />
        </div>
      </div>
      <div class="detail-pane" aria-hidden="true">
        <div class="skeleton-rows">
          <div v-for="i in 4" :key="i" class="skeleton-row">
            <span class="skeleton skeleton-name" />
            <span class="skeleton skeleton-meta" />
          </div>
        </div>
      </div>
    </section>

    <!-- No action slot: Create group is in the sticky header, present at every
         scroll depth and in every state of the page (docs/admin-ui.md
         § Component primitives). -->
    <section v-else-if="!rows.length" class="body body-empty">
      <EmptyState :title="t('groups.empty.title')" :body="t('groups.empty.body')" />
    </section>

    <section v-else class="body" :class="{ 'detail-open': detailOpen }">
      <nav class="list-pane" :aria-label="t('groups.title')">
        <p class="list-label">{{ t("groups.count", { count: rows.length }) }}</p>
        <ul ref="listEl" class="group-list" @keydown="onListKeydown">
          <li v-for="group in rows" :key="group.id">
            <button
              type="button"
              class="group-item"
              :class="{ selected: group.id === selectedGroup?.id }"
              :aria-current="group.id === selectedGroup?.id ? 'true' : undefined"
              :data-group-id="group.id"
              @click="selectGroup(group)"
            >
              <span class="group-text">
                <span class="group-name">{{ group.name }}</span>
                <span class="group-meta">
                  <span class="mono group-slug">{{ group.slug }}</span>
                  <span aria-hidden="true">·</span>
                  <span>{{ t("models.count", { count: group.models?.length ?? 0 }) }}</span>
                </span>
              </span>
              <template v-if="issueCount(group)">
                <span
                  class="issue-dot"
                  aria-hidden="true"
                  :title="t('groups.issues', { count: issueCount(group) })"
                />
                <span class="sr-only">{{ t("groups.issues", { count: issueCount(group) }) }}</span>
              </template>
            </button>
          </li>
        </ul>
      </nav>

      <div v-if="selectedGroup" class="detail-pane">
        <header class="detail-head">
          <div class="detail-title-row">
            <AppButton
              size="sm"
              variant="ghost"
              icon-only
              class="back"
              :label="t('groups.back')"
              @click="closeDetail"
            >
              <template #icon><ActionIcon name="chevron-left" /></template>
            </AppButton>
            <h2 class="detail-title">{{ selectedGroup.name }}</h2>
            <span class="detail-updated" :title="format.dateTime(selectedGroup.updated_at)">
              {{ t("groups.updated", { when: format.relative(selectedGroup.updated_at) }) }}
            </span>
            <AppButton
              size="sm"
              variant="secondary"
              class="detail-edit"
              :label="t('groups.editGroup', { name: selectedGroup.name })"
              @click="openEdit(selectedGroup)"
            >
              <template #icon><ActionIcon name="edit" /></template>
              {{ t("action.edit") }}
            </AppButton>
          </div>

          <!-- A base URL is exactly what goes in a client's base_url setting,
               so each one is a full, copyable field rather than a label. -->
          <ul class="endpoints">
            <li v-for="entry in endpointUrls(selectedGroup)" :key="entry.key" class="endpoint">
              <span class="endpoint-label">{{ entry.label }}</span>
              <code class="mono endpoint-url" :title="entry.url">{{ entry.url }}</code>
              <button
                type="button"
                class="icon-button"
                :aria-label="t('groups.copyUrl', { url: entry.url })"
                :title="t('groups.copyUrl', { url: entry.url })"
                @click="copyValue(entry.key, entry.url)"
              >
                <ActionIcon :name="copiedKey === entry.key ? 'check' : 'copy'" />
              </button>
            </li>
          </ul>
        </header>

        <!-- One row per group model: the copyable name (exactly what a client
             sends as `model` on this endpoint), then its ordered targets left
             to right with the per-model current-route facts. -->
        <div class="models" role="table" :aria-label="selectedGroup.name">
          <div class="models-head" role="row">
            <span role="columnheader">{{ t("groups.column.model") }}</span>
            <span role="columnheader">{{ t("groups.column.route") }}</span>
          </div>

          <div
            v-for="model in selectedGroup.models ?? []"
            :key="model.name"
            class="model-row"
            role="row"
          >
            <div role="cell" class="model-cell">
              <button
                type="button"
                class="model-copy"
                :class="{ copied: copiedKey === `model:${selectedGroup.id}:${model.name}` }"
                :aria-label="t('groups.copyModel', { model: model.name })"
                @click="copyValue(`model:${selectedGroup.id}:${model.name}`, model.name)"
              >
                <ActionIcon
                  :name="copiedKey === `model:${selectedGroup.id}:${model.name}` ? 'check' : 'copy'"
                />
                <span class="mono model-name">{{ model.name }}</span>
              </button>
            </div>

            <div role="cell" class="route-cell">
              <ol class="route">
                <li
                  v-for="(target, index) in model.targets"
                  :key="targetKey(target, index)"
                  class="route-step"
                >
                  <span
                    class="target"
                    :class="{
                      current: isCurrentTarget(model, index),
                      unusable: isUnusable(model, index) || isMissingAccount(target),
                    }"
                    :title="target.model"
                  >
                    <span class="pos tabular">{{ index + 1 }}</span>
                    <span class="target-body">
                      <code class="mono target-id">
                        <span class="target-provider">{{ splitTarget(target.model).provider }}</span>
                        <span>{{ splitTarget(target.model).rest }}</span>
                      </code>
                      <!-- What the next request would actually do: a reason
                           replaces the account line when the target is skipped. -->
                      <span
                        v-if="unusableReason(model, target, index)"
                        class="target-sub target-reason"
                        :title="unusableTitle(model, index)"
                      >
                        {{ unusableReason(model, target, index) }}
                      </span>
                      <span v-else-if="isMissingAccount(target)" class="target-sub target-reason">
                        {{ t("groups.account.missing") }}
                      </span>
                      <span v-else class="target-sub">
                        {{ target.account_label ?? t("groups.account.any") }}
                      </span>
                    </span>
                    <span v-if="isCurrentTarget(model, index)" class="sr-only">
                      {{ t("groups.route.current") }}
                    </span>
                  </span>
                  <!-- After the chip, not before the next one, so a wrapped route
                       ends its line on the arrow instead of starting one. -->
                  <ActionIcon
                    v-if="index < model.targets.length - 1"
                    name="chevron-right"
                    class="route-arrow"
                  />
                </li>
              </ol>
            </div>
          </div>
        </div>
      </div>
    </section>

    <span class="sr-only" role="status" aria-live="polite">
      {{ copiedKey ? t("action.copied") : "" }}
    </span>

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
 * The page fills the content region exactly and runs edge to edge: it cancels
 * the shell's gutter and bottom padding the way PageHeader cancels the top, so
 * the two panes reach the region's edges with no card around them
 * (docs/admin-ui.md § Groups page). The page never scrolls; each pane does.
 *
 * Every value here is AppShell's own, inherited rather than restated: it owns
 * the padding and knows how much chrome sits above this region, and a second
 * copy would drift the first time only one of them changed breakpoint.
 */
.page {
  --gutter: var(--page-gutter, var(--space-2));
  --bottom: var(--page-bottom, var(--space-12));

  display: flex;
  flex-direction: column;
  height: calc(100dvh - var(--page-chrome, 0px) - var(--page-top, var(--space-6)));
  margin: 0 calc(var(--gutter) * -1) calc(var(--bottom) * -1);
}

/* PageHeader cancels the gutter itself; this page already has, so the header
   only keeps its top cancel. It sits flush on the panes, its bottom rule their
   top edge. */
.page > .page-header {
  margin: calc(var(--page-top, var(--space-6)) * -1) 0 0;
}

.page-alert {
  flex-shrink: 0;
  margin: var(--space-3) var(--gutter) 0;
}

.body {
  flex: 1;
  min-height: 0;
  display: grid;
  grid-template-columns: 256px minmax(0, 1fr);
}

.body-empty {
  display: block;
  overflow: auto;
}

/* --- Left: group list --------------------------------------------------- */

.list-pane {
  min-height: 0;
  overflow: auto;
  padding: var(--space-2) var(--space-2) calc(var(--space-2) + env(safe-area-inset-bottom, 0px));
  border-right: 1px solid var(--border);
}

.list-label {
  margin: 0;
  padding: var(--space-2) 10px 6px;
  font-size: var(--text-2xs);
  font-weight: var(--weight-semibold);
  text-transform: uppercase;
  letter-spacing: var(--tracking-wide);
  color: var(--muted);
}

.group-list {
  display: flex;
  flex-direction: column;
  gap: 2px;
  margin: 0;
  padding: 0;
  list-style: none;
}

.group-item {
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  padding: 10px;
  border: none;
  border-radius: var(--radius-sm);
  background: transparent;
  color: inherit;
  font: inherit;
  text-align: left;
  cursor: pointer;
  transition:
    background-color var(--duration-fast) var(--ease),
    box-shadow var(--duration-fast) var(--ease);
}

.group-item:hover:not(.selected) {
  background: var(--hover);
}

.group-item.selected {
  background: var(--surface);
  box-shadow:
    0 0 0 1px var(--border),
    0 1px 2px rgb(0 0 0 / 4%);
}

.group-item:focus-visible {
  outline: none;
  box-shadow:
    0 0 0 1px var(--ring-border),
    var(--ring);
}

.group-text {
  display: flex;
  flex: 1;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.group-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: var(--text-sm);
  font-weight: var(--weight-medium);
  color: var(--text);
}

.group-meta {
  display: flex;
  gap: 6px;
  min-width: 0;
  font-size: var(--text-xs);
  color: var(--faint);
  white-space: nowrap;
}

.group-slug {
  overflow: hidden;
  text-overflow: ellipsis;
}

.issue-dot {
  flex-shrink: 0;
  width: 7px;
  height: 7px;
  border-radius: var(--radius-full);
  background: var(--warn);
}

/* --- Right: detail ------------------------------------------------------ */

.detail-pane {
  display: flex;
  flex-direction: column;
  min-width: 0;
  min-height: 0;
  background: var(--surface);
}

.detail-head {
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
  padding: var(--space-4) var(--space-5);
  border-bottom: 1px solid var(--border);
}

.detail-title-row {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
}

/* Only the phone layout has a list to go back to. */
.back {
  display: none;
}

.detail-title {
  min-width: 0;
  margin: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: var(--text-md);
  font-weight: var(--weight-semibold);
  letter-spacing: var(--tracking-tight);
}

.detail-updated {
  flex-shrink: 0;
  font-size: var(--text-xs);
  color: var(--faint);
}

.detail-edit {
  flex-shrink: 0;
  margin-left: auto;
}

.endpoints {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: var(--space-2);
  margin: 0;
  padding: 0;
  list-style: none;
}

.endpoint {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
  height: 32px;
  padding: 0 6px 0 10px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--surface-2);
}

.endpoint-label {
  flex-shrink: 0;
  font-size: var(--text-2xs);
  font-weight: var(--weight-semibold);
  text-transform: uppercase;
  letter-spacing: var(--tracking-wide);
  color: var(--muted);
}

.endpoint-url {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: var(--text-xs);
  color: var(--text-secondary);
}

.icon-button {
  display: grid;
  flex-shrink: 0;
  place-items: center;
  width: 24px;
  height: 24px;
  padding: 0;
  border: none;
  border-radius: var(--radius-xs);
  background: transparent;
  color: var(--faint);
  cursor: pointer;
  transition: color var(--duration-fast) var(--ease);
}

.icon-button:hover {
  color: var(--text);
}

.icon-button svg,
.model-copy svg {
  width: 14px;
  height: 14px;
}

/* --- Models table ------------------------------------------------------- */

.models {
  flex: 1;
  min-height: 0;
  overflow: auto;
  padding-bottom: env(safe-area-inset-bottom, 0px);
}

.models-head,
.model-row {
  display: grid;
  grid-template-columns: 240px minmax(0, 1fr);
  align-items: center;
  gap: var(--space-3);
  padding: 0 var(--space-5);
}

/* Same type as DataTable's `th`, opaque and ruled by an inset shadow so rows
   never smear under it and the rule never scrolls away from it. */
.models-head {
  position: sticky;
  top: 0;
  z-index: 1;
  height: 36px;
  font-size: var(--text-2xs);
  font-weight: var(--weight-semibold);
  text-transform: uppercase;
  letter-spacing: var(--tracking-wide);
  color: var(--muted);
  white-space: nowrap;
  background: var(--surface-2);
  box-shadow: inset 0 -1px 0 var(--border);
}

.model-row {
  padding-block: 10px;
  border-bottom: 1px solid var(--border);
}

.model-cell,
.route-cell {
  min-width: 0;
}

/* Quiet until pointed at, but always present — never revealed on hover. */
.model-copy {
  display: flex;
  align-items: center;
  gap: 6px;
  max-width: 100%;
  padding: 2px 4px;
  margin-left: -4px;
  border: none;
  border-radius: var(--radius-xs);
  background: transparent;
  color: var(--faint);
  font: inherit;
  cursor: pointer;
  transition: color var(--duration-fast) var(--ease);
}

.model-copy:hover,
.model-copy.copied {
  color: var(--text);
}

.model-copy svg {
  flex-shrink: 0;
}

.model-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: var(--text-sm);
  color: var(--text);
}

.route {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
  min-width: 0;
  margin: 0;
  padding: 0;
  list-style: none;
}

.route-step {
  display: flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
}

.route-arrow {
  flex-shrink: 0;
  width: 12px;
  height: 12px;
  color: var(--faint);
}

/* --- Target chip -------------------------------------------------------- */

.target {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  min-width: 0;
  padding: 4px 10px 4px 5px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--surface-2);
}

.pos {
  display: grid;
  flex-shrink: 0;
  place-items: center;
  width: 18px;
  height: 18px;
  border-radius: var(--radius-xs);
  background: var(--border);
  color: var(--muted);
  font-size: var(--text-2xs);
  font-weight: var(--weight-semibold);
}

.target-body {
  display: flex;
  flex-direction: column;
  min-width: 0;
  line-height: 1.35;
}

.target-id {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: var(--text-xs);
  color: var(--text);
}

.target-provider {
  color: var(--faint);
}

.target-sub {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: var(--text-2xs);
  color: var(--faint);
}

.target.current {
  border-color: var(--accent);
  background: var(--surface);
}

.target.current .pos {
  background: var(--accent);
  color: var(--accent-fg);
}

.target.current .target-sub {
  color: var(--text-secondary);
}

/* A target that cannot take a request: the reason text states it, the tone
   and the strike only reinforce it. */
.target.unusable {
  border-color: var(--warn-border);
  background: var(--warn-bg);
}

.target.unusable .pos {
  background: var(--warn-border);
  color: var(--warn);
}

.target.unusable .target-id {
  color: var(--faint);
  text-decoration: line-through;
}

.target-reason {
  color: var(--warn);
}

/* --- First paint -------------------------------------------------------- */

.skeleton-item {
  display: grid;
  gap: var(--space-2);
  padding: 10px;
}

.skeleton-rows {
  display: grid;
  gap: var(--space-5);
  padding: var(--space-5);
}

.skeleton-row {
  display: grid;
  grid-template-columns: 240px minmax(0, 1fr);
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

/* --- Responsive --------------------------------------------------------- */

@media (max-width: 1080px) {
  .body {
    grid-template-columns: 220px minmax(0, 1fr);
  }

  .models-head,
  .model-row,
  .skeleton-row {
    grid-template-columns: 200px minmax(0, 1fr);
  }
}

/* One pane at a time: the list, or the group it opened with a way back. */
@media (max-width: 767px) {
  .body {
    grid-template-columns: minmax(0, 1fr);
  }

  .body .detail-pane,
  .body.detail-open .list-pane {
    display: none;
  }

  .body.detail-open .detail-pane {
    display: flex;
  }

  .list-pane {
    border-right: none;
  }

  .back {
    display: inline-flex;
    margin-left: calc(var(--space-2) * -1);
  }

  .detail-head {
    padding: var(--space-3) var(--space-4);
  }

  .endpoints {
    grid-template-columns: minmax(0, 1fr);
  }

  .models-head {
    display: none;
  }

  .model-row {
    grid-template-columns: minmax(0, 1fr);
    gap: var(--space-2);
    padding-inline: var(--space-4);
  }

  .detail-updated {
    display: none;
  }
}
</style>
