<script setup lang="ts">
/**
 * Providers: tabs in the sticky header — All, one per builtin pool, Custom —
 * same pattern as Models (docs/admin-ui.md § Providers page). All stacks
 * every section; a provider tab renders only that provider's card. One panel
 * at a time, real tab semantics, no anchor-scrolling.
 *
 * Data is cache-first and silent about it: the cache paints immediately, a
 * background poll keeps it warm, and neither says a word in the UI. Only the
 * Refresh the user pressed reports progress, on the button they pressed.
 *
 * The poll runs only while the page is visible — see `syncToVisibility`.
 */
import { computed, onMounted, onUnmounted, ref } from "vue"
import AddAccountDialog from "@/components/AddAccountDialog.vue"
import CustomProviderDialog from "@/components/CustomProviderDialog.vue"
import RenameAccountDialog from "@/components/RenameAccountDialog.vue"
import AccountCard from "@/components/providers/AccountCard.vue"
import CustomProviderCard from "@/components/providers/CustomProviderCard.vue"
import ActionIcon from "@/components/ui/ActionIcon.vue"
import AppButton from "@/components/ui/AppButton.vue"
import AppCard from "@/components/ui/AppCard.vue"
import Banner from "@/components/ui/Banner.vue"
import EmptyState from "@/components/ui/EmptyState.vue"
import FormField from "@/components/ui/FormField.vue"
import PageHeader from "@/components/ui/PageHeader.vue"
import SectionNav from "@/components/ui/SectionNav.vue"
import type { SectionItem } from "@/components/ui/SectionNav.vue"
import { useAccounts } from "@/composables/useAccounts"
import { useAuth } from "@/composables/useAuth"
import { useCustomProviders } from "@/composables/useCustomProviders"
import { useI18n } from "@/i18n"
import type { MessageKey } from "@/i18n"
import {
  deleteCustomProvider,
  promoteAccount,
  removeAccount,
  setProviderStrategy,
  unpauseAccount,
  unpauseCustomProvider,
} from "@/services/api"
import { getProvidersPrefs, setProvidersPrefs } from "@/services/prefs"
import {
  DEFAULT_ROUTING_STRATEGY,
  PROVIDERS,
  ROUTING_STRATEGIES,
  type CustomProvider,
  type ProviderAccount,
  type ProviderId,
  type RoutingStrategy,
} from "@/types"

const { t } = useI18n()
const { user } = useAuth()
const { byProvider, setUserId, loadAll, loadProvider, setStrategy, CACHE_TTL_MS } = useAccounts()
const customProviders = useCustomProviders()

/**
 * Provider display copy lives in the catalog, and `PROVIDERS` carries only wire
 * ids. An explicit map rather than a template literal: `` `provider.${id}.name` ``
 * widens to `string` and would not typecheck against `MessageKey`, so a
 * renamed key has to fail the build here.
 */
const NAME_KEY: Record<ProviderId, MessageKey> = {
  "claude-code": "provider.claude-code.name",
  codex: "provider.codex.name",
  grok: "provider.grok.name",
}

/**
 * Each strategy's own name and the line that says what it does to a pool. A
 * map rather than a template literal for the same reason as above — and it is
 * the seam a second strategy lands in: an entry here, an entry in
 * `ROUTING_STRATEGIES`, and the select offers it.
 */
const STRATEGY_KEY: Record<RoutingStrategy, MessageKey> = {
  ordered: "strategy.ordered",
}

const STRATEGY_HINT_KEY: Record<RoutingStrategy, MessageKey> = {
  ordered: "strategy.pool.ordered",
}

/** The custom-endpoints tab is not a `ProviderId`, so it gets its own id. */
const CUSTOM = "custom"
/** The "everything" tab. Underscored so it can never collide with a wire id. */
const ALL = "__all__"

const busyId = ref<string | null>(null)
const customBusyId = ref<string | null>(null)
const actionError = ref<string | null>(null)
const refreshing = ref(false)

const addFor = ref<ProviderId | null>(null)
const showCustomDialog = ref(false)
const editingCustomProvider = ref<CustomProvider | null>(null)
/** The row being renamed, with the upstream identity the card resolved for it. */
const renaming = ref<{
  provider: ProviderId
  account: ProviderAccount
  identity: string
} | null>(null)

/**
 * Which sections have their row actions revealed, keyed by section id (a
 * `ProviderId` or CUSTOM). One gate per section rather than per row
 * (docs/admin-ui.md § Providers page); a Set because sections are independent
 * and All shows several at once.
 */
const editingSections = ref(new Set<string>())

function isEditing(section: string): boolean {
  return editingSections.value.has(section)
}

function toggleEditing(section: string) {
  // Replaced, not mutated: Vue does not track Set membership on its own.
  const next = new Set(editingSections.value)
  if (!next.delete(section)) next.add(section)
  editingSections.value = next
}

/** What the user last picked; resolved against the known tabs below. */
const selected = ref<string>(getProvidersPrefs().tab ?? ALL)

let pollTimer: number | undefined

const tabIds = computed<string[]>(() => [ALL, ...PROVIDERS.map((p) => p.id), CUSTOM])

/** A stored tab that no longer exists resolves to All rather than an empty page. */
const activeTab = computed(() => (tabIds.value.includes(selected.value) ? selected.value : ALL))

const navItems = computed<SectionItem[]>(() => [
  {
    id: ALL,
    label: t("providers.all"),
    // null while nothing has loaded: a "0" the app is not sure about reads as
    // a fact, and an empty chip is more honest than a wrong one.
    count: allCount.value,
  },
  ...PROVIDERS.map((p) => ({
    id: p.id,
    label: t(NAME_KEY[p.id]),
    count: byProvider[p.id].data?.accounts.length ?? null,
  })),
  {
    id: CUSTOM,
    label: t("providers.group.custom"),
    count: customProviders.state.data?.length ?? null,
  },
])

/** Sum across sections, but only once every section has actually loaded. */
const allCount = computed<number | null>(() => {
  let sum = 0
  for (const p of PROVIDERS) {
    const n = byProvider[p.id].data?.accounts.length
    if (n == null) return null
    sum += n
  }
  const custom = customProviders.state.data?.length
  if (custom == null) return null
  return sum + custom
})

const visibleProviders = computed<ProviderId[]>(() => {
  if (activeTab.value === ALL) return PROVIDERS.map((p) => p.id)
  return PROVIDERS.filter((p) => p.id === activeTab.value).map((p) => p.id)
})

const showCustomSection = computed(
  () => activeTab.value === ALL || activeTab.value === CUSTOM,
)

/** Names the panel after the tab that opened it. */
const activeLabel = computed(
  () => navItems.value.find((item) => item.id === activeTab.value)?.label ?? t("providers.all"),
)

function onSelectTab(id: string) {
  selected.value = id
  setProvidersPrefs({ tab: id === ALL ? null : id })
}

/** Cache-first: paint localStorage, network only when stale. */
function loadEverything() {
  return Promise.all([loadAll(), customProviders.load()])
}

/** Idempotent: a stray double-start would leak an interval nothing can clear. */
function startPolling() {
  stopPolling()
  pollTimer = window.setInterval(() => void loadEverything(), CACHE_TTL_MS)
}

function stopPolling() {
  if (pollTimer !== undefined) window.clearInterval(pollTimer)
  pollTimer = undefined
}

/**
 * Polling upstream billing APIs for a page nobody is looking at is pure waste,
 * and the browser's own timer throttling is far too generous to rely on
 * (docs/admin-ui.md § Polling stops when the page is hidden).
 */
function syncToVisibility() {
  if (document.hidden) {
    stopPolling()
    return
  }
  void loadEverything()
  startPolling()
}

onMounted(() => {
  // Unconditional: every cache read and write is scoped to the user id, so
  // this has to land even when the page mounts hidden and loads nothing.
  setUserId(user.value?.id ?? null)
  customProviders.setUserId(user.value?.id ?? null)
  // `visibilitychange` only fires on a transition, so a tab born hidden never
  // gets one — which is exactly how Firefox restores a pinned tab at startup.
  syncToVisibility()
  document.addEventListener("visibilitychange", syncToVisibility)
  // A bfcache restore does not re-run onMounted; without this the page can
  // come back visible with its poll still stopped.
  window.addEventListener("pageshow", syncToVisibility)
})

onUnmounted(() => {
  stopPolling()
  document.removeEventListener("visibilitychange", syncToVisibility)
  window.removeEventListener("pageshow", syncToVisibility)
})

async function refreshAll() {
  actionError.value = null
  refreshing.value = true
  try {
    await Promise.all([loadAll({ refresh: true }), customProviders.load({ refresh: true })])
  } finally {
    refreshing.value = false
  }
}

async function onResume(provider: ProviderId, id: string) {
  busyId.value = id
  actionError.value = null
  try {
    await unpauseAccount(provider, id)
    await loadProvider(provider, { refresh: true })
  } catch {
    actionError.value = t("providers.error.resume")
  } finally {
    busyId.value = null
  }
}

async function onPromote(provider: ProviderId, id: string) {
  busyId.value = id
  actionError.value = null
  try {
    await promoteAccount(provider, id)
    await loadProvider(provider, { refresh: true })
  } catch {
    actionError.value = t("providers.error.promote")
  } finally {
    busyId.value = null
  }
}

async function onRemove(provider: ProviderId, id: string) {
  if (!confirm(t("providers.account.removeConfirm"))) return
  busyId.value = id
  actionError.value = null
  try {
    await removeAccount(provider, id)
    await loadProvider(provider, { refresh: true })
  } catch {
    actionError.value = t("providers.error.remove")
  } finally {
    busyId.value = null
  }
}

/**
 * The pool's current strategy. A response (or a cache entry) without the field
 * predates it and means the server's default — never a blank select.
 */
function strategyOf(provider: ProviderId): RoutingStrategy {
  return byProvider[provider].data?.strategy ?? DEFAULT_ROUTING_STRATEGY
}

/** Which pool is mid-write; its select stays put until the server answers. */
const strategyBusy = ref<ProviderId | null>(null)

/**
 * Saves on change, and paints what the server stored rather than what was
 * sent. A refusal puts the element back by hand: the native select has already
 * moved, and Vue patches nothing back because the bound value never changed.
 * The failure is reported in the page's banner, like the row actions' are —
 * nothing blocks.
 */
async function onStrategyChange(provider: ProviderId, event: Event) {
  const el = event.target as HTMLSelectElement
  const previous = strategyOf(provider)
  const next = el.value as RoutingStrategy
  if (next === previous) return

  strategyBusy.value = provider
  actionError.value = null
  try {
    setStrategy(provider, await setProviderStrategy(provider, next))
    // Same reason as the rollback below: if the stored value is not the one
    // that was sent, the bound value never changed and nothing is patched back.
    el.value = strategyOf(provider)
  } catch {
    actionError.value = t("providers.error.strategy")
    el.value = previous
  } finally {
    strategyBusy.value = null
  }
}

async function onRenamed(provider: ProviderId) {
  await loadProvider(provider, { refresh: true })
}

async function onAdded(provider: ProviderId) {
  addFor.value = null
  await loadProvider(provider, { refresh: true })
}

function openCreateCustomDialog() {
  editingCustomProvider.value = null
  showCustomDialog.value = true
}

function openEditCustomDialog(provider: CustomProvider) {
  editingCustomProvider.value = provider
  showCustomDialog.value = true
}

function closeCustomDialog() {
  showCustomDialog.value = false
  editingCustomProvider.value = null
}

async function onCustomProviderSaved() {
  await customProviders.load({ refresh: true })
}

/**
 * Reordering. The page owns the ordering logic — the row only reports intent —
 * so both paths (buttons and drag) funnel into one `moveCustomProvider`.
 *
 * `reorderableCustom` gates the controls on there being something to reorder: a
 * single endpoint with a disabled pair of arrows is two dead controls.
 */
const customOrderAnnouncement = ref("")
const draggingCustomId = ref<string | null>(null)
/** The order the drag started from — the rollback target, not render state. */
let customDragStartIds: string[] | null = null

const reorderableCustom = computed(() => (customProviders.state.data?.length ?? 0) >= 2)

async function moveCustomProvider(from: number, to: number) {
  const list = customProviders.state.data
  if (!list || from === to || to < 0 || to >= list.length) return

  const moved = list[from]
  const ids = list.map((p) => p.id)
  ids.splice(from, 1)
  ids.splice(to, 0, moved.id)

  const ok = await customProviders.reorder(ids)
  if (!ok) {
    actionError.value = t("custom.error.reorder")
    customOrderAnnouncement.value = ""
    return
  }
  actionError.value = null
  // Announced from the server's order, so what is read out is what was saved.
  const index = customProviders.state.data?.findIndex((p) => p.id === moved.id) ?? to
  customOrderAnnouncement.value = t("custom.reorder.moved", {
    name: moved.name,
    position: index + 1,
    total: customProviders.state.data?.length ?? 0,
  })
}

/**
 * A drag reorders the list locally as it crosses rows and saves **once** on
 * drop: a PUT per hovered row would fire a request per pixel of travel and
 * leave the list mid-flight when the pointer lifts.
 */
function onCustomDragStart(id: string) {
  draggingCustomId.value = id
  customDragStartIds = customProviders.state.data?.map((p) => p.id) ?? null
}

function onCustomDragEnter(index: number) {
  const list = customProviders.state.data
  const dragged = draggingCustomId.value
  if (!list || !dragged) return
  const from = list.findIndex((p) => p.id === dragged)
  if (from < 0 || from === index) return
  const next = list.slice()
  const [moved] = next.splice(from, 1)
  next.splice(index, 0, moved)
  customProviders.state.data = next
}

async function onCustomDragEnd() {
  const before = customDragStartIds
  const dragged = draggingCustomId.value
  draggingCustomId.value = null
  customDragStartIds = null

  const list = customProviders.state.data
  if (!before || !list) return
  const ids = list.map((p) => p.id)
  if (ids.join(",") === before.join(",")) return

  // Restore the pre-drag order first so `reorder` rolls back to it on failure.
  customProviders.state.data = before
    .map((id) => list.find((p) => p.id === id))
    .filter((p): p is CustomProvider => !!p)

  const from = before.indexOf(dragged ?? "")
  const to = ids.indexOf(dragged ?? "")
  if (from < 0 || to < 0) return
  await moveCustomProvider(from, to)
}

async function onResumeCustom(provider: CustomProvider) {
  customBusyId.value = provider.id
  actionError.value = null
  try {
    await unpauseCustomProvider(provider.id)
    await customProviders.load({ refresh: true })
  } catch {
    actionError.value = t("custom.error.resume")
  } finally {
    customBusyId.value = null
  }
}

async function onRemoveCustomProvider(provider: CustomProvider) {
  if (!confirm(t("custom.removeConfirm", { name: provider.name }))) return
  customBusyId.value = provider.id
  actionError.value = null
  try {
    await deleteCustomProvider(provider.id)
    await customProviders.load({ refresh: true })
  } catch {
    actionError.value = t("custom.error.remove")
  } finally {
    customBusyId.value = null
  }
}
</script>

<template>
  <div>
    <PageHeader :title="t('providers.title')">
      <template #actions>
        <!-- Icon-only: the label is a tooltip and the accessible name. -->
        <AppButton
          icon-only
          :label="t('action.refresh')"
          :loading="refreshing"
          @click="refreshAll"
        >
          <template #icon><ActionIcon name="refresh" /></template>
        </AppButton>
      </template>
      <template #nav>
        <SectionNav
          :items="navItems"
          :active="activeTab"
          :label="t('providers.title')"
          @select="onSelectTab"
        />
      </template>
    </PageHeader>

    <div v-if="actionError" class="page-alert">
      <Banner tone="error">
        {{ actionError }}
        <template #actions>
          <AppButton size="sm" variant="ghost" @click="actionError = null">
            {{ t("action.dismiss") }}
          </AppButton>
        </template>
      </Banner>
    </div>

    <!-- The panel the tabs point at: SectionNav's aria-controls is
         `panel-<id>`, and only the selected tab's sections are in the DOM. -->
    <div
      :id="`panel-${activeTab}`"
      class="sections"
      role="tabpanel"
      :aria-label="activeLabel"
    >
      <AppCard v-for="pid in visibleProviders" :key="pid" :title="t(NAME_KEY[pid])">
        <template #actions>
          <!-- Section-level config, so it appears with the section's other
               config affordances when the gate opens — never on a row. It
               renders with one option on purpose: it is the seam future
               strategies appear in, and it tells the operator the pool has a
               routing policy at all (docs/admin-ui.md § Providers page). -->
          <FormField
            v-if="isEditing(pid)"
            v-slot="field"
            class="strategy"
            :label="t('strategy.label')"
            :hint="t(STRATEGY_HINT_KEY[strategyOf(pid)])"
          >
            <select
              :id="field.id"
              class="select"
              :value="strategyOf(pid)"
              :aria-describedby="field.describedBy"
              :disabled="strategyBusy === pid"
              @change="onStrategyChange(pid, $event)"
            >
              <option v-for="option in ROUTING_STRATEGIES" :key="option" :value="option">
                {{ t(STRATEGY_KEY[option]) }}
              </option>
            </select>
          </FormField>
          <!-- Icon-only ghost: this page is read far more often than it is
               added to, so the create affordance sits at icon weight. Hidden
               while the gate is open — editing the pool and adding to it are
               different jobs, and the open gate says which one this is. -->
          <AppButton
            v-if="!isEditing(pid)"
            icon-only
            size="sm"
            variant="ghost"
            :label="t('providers.addAccount')"
            @click="addFor = pid"
          >
            <template #icon><ActionIcon name="plus" /></template>
          </AppButton>
          <!-- One gate for the whole section. aria-pressed: the same control
               opens and closes it, so its state must be audible as one. -->
          <AppButton
            v-if="byProvider[pid].data?.accounts.length"
            icon-only
            size="sm"
            variant="ghost"
            :label="
              isEditing(pid)
                ? t('providers.section.doneEditing', { section: t(NAME_KEY[pid]) })
                : t('providers.section.edit', { section: t(NAME_KEY[pid]) })
            "
            :aria-pressed="isEditing(pid)"
            @click="toggleEditing(pid)"
          >
            <template #icon><ActionIcon :name="isEditing(pid) ? 'check' : 'edit'" /></template>
          </AppButton>
        </template>

        <div class="section-body">
          <Banner v-if="byProvider[pid].error" tone="warn">
            {{ t("providers.error.load", { provider: t(NAME_KEY[pid]) }) }}
          </Banner>

          <!-- Skeletons are decoration; the status beside them is what a
               screen reader gets, same as the shell's boot loader. -->
          <div v-if="!byProvider[pid].data" class="skeleton-list">
            <span class="sr-only" role="status">{{ t("app.loading") }}</span>
            <div v-for="i in 2" :key="i" class="skeleton-row" aria-hidden="true">
              <span class="skeleton skeleton-name" />
              <span class="skeleton skeleton-bar" />
              <span class="skeleton skeleton-bar" />
            </div>
          </div>

          <div v-else-if="byProvider[pid].data!.accounts.length" class="rows">
            <AccountCard
              v-for="account in byProvider[pid].data!.accounts"
              :key="account.id"
              :account="account"
              :busy="busyId === account.id"
              :editing="isEditing(pid)"
              @resume="onResume(pid, account.id)"
              @promote="onPromote(pid, account.id)"
              @rename="renaming = { provider: pid, account, identity: $event }"
              @remove="onRemove(pid, account.id)"
            />
          </div>

          <EmptyState
            v-else
            compact
            :title="t('providers.empty.title', { provider: t(NAME_KEY[pid]) })"
            :body="t('providers.empty.body', { provider: t(NAME_KEY[pid]) })"
          />
        </div>
      </AppCard>

      <AppCard v-if="showCustomSection" :title="t('provider.custom.name')">
        <template #actions>
          <AppButton
            v-if="!isEditing(CUSTOM)"
            icon-only
            size="sm"
            variant="ghost"
            :label="t('custom.add')"
            @click="openCreateCustomDialog"
          >
            <template #icon><ActionIcon name="plus" /></template>
          </AppButton>
          <AppButton
            v-if="customProviders.state.data?.length"
            icon-only
            size="sm"
            variant="ghost"
            :label="
              isEditing(CUSTOM)
                ? t('providers.section.doneEditing', { section: t('provider.custom.name') })
                : t('providers.section.edit', { section: t('provider.custom.name') })
            "
            :aria-pressed="isEditing(CUSTOM)"
            @click="toggleEditing(CUSTOM)"
          >
            <template #icon><ActionIcon :name="isEditing(CUSTOM) ? 'check' : 'edit'" /></template>
          </AppButton>
        </template>

        <div class="section-body">
          <Banner v-if="customProviders.state.error" tone="warn">
            {{ t("custom.error.load") }}
          </Banner>

          <div v-if="!customProviders.state.data" class="skeleton-list">
            <span class="sr-only" role="status">{{ t("app.loading") }}</span>
            <div v-for="i in 2" :key="i" class="skeleton-row" aria-hidden="true">
              <span class="skeleton skeleton-name" />
              <span class="skeleton skeleton-bar" />
            </div>
          </div>

          <div v-else-if="customProviders.state.data.length" class="rows">
            <CustomProviderCard
              v-for="(provider, index) in customProviders.state.data"
              :key="provider.id"
              :provider="provider"
              :busy="customBusyId === provider.id"
              :editing="isEditing(CUSTOM)"
              :reorderable="reorderableCustom"
              :can-move-up="index > 0"
              :can-move-down="index < customProviders.state.data.length - 1"
              :dragging="draggingCustomId === provider.id"
              @resume="onResumeCustom(provider)"
              @edit="openEditCustomDialog(provider)"
              @remove="onRemoveCustomProvider(provider)"
              @move-up="moveCustomProvider(index, index - 1)"
              @move-down="moveCustomProvider(index, index + 1)"
              @drag-start="onCustomDragStart(provider.id)"
              @drag-enter="onCustomDragEnter(index)"
              @drag-end="onCustomDragEnd"
            />
          </div>

          <EmptyState
            v-else
            compact
            :title="t('custom.empty.title')"
            :body="t('custom.empty.body')"
          />

          <!-- Outside the v-if chain above, so it survives the list rerendering:
               the new position is the only feedback the move buttons give a
               screen-reader user. -->
          <span class="sr-only" role="status" aria-live="polite">
            {{ customOrderAnnouncement }}
          </span>
        </div>
      </AppCard>
    </div>

    <AddAccountDialog
      v-if="addFor"
      :provider="addFor"
      :provider-name="t(NAME_KEY[addFor])"
      @close="addFor = null"
      @added="onAdded(addFor!)"
    />

    <RenameAccountDialog
      v-if="renaming"
      :provider="renaming.provider"
      :account="renaming.account"
      :identity="renaming.identity"
      @close="renaming = null"
      @saved="onRenamed(renaming!.provider)"
    />

    <CustomProviderDialog
      v-if="showCustomDialog"
      :provider="editingCustomProvider"
      @close="closeCustomDialog"
      @saved="onCustomProviderSaved"
    />
  </div>
</template>

<style scoped>
/* No gap on the page itself: PageHeader carries its own bottom margin, and a
   flex gap on top of it would double the space under a sticky header that is
   already the tallest thing on screen. */
.page-alert {
  margin-bottom: var(--space-4);
}

.sections {
  display: grid;
  gap: var(--space-5);
}

.section-body {
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
}

/* Narrow enough that the pool's name still owns the header line, and capped by
   the header itself on small screens where the actions wrap onto their own row. */
.strategy {
  width: 200px;
  max-width: 100%;
}

/* The app's control spec, same as the other selects in the app (the key
   dialog's interval, the group dialog's re-pick). */
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

.select:disabled {
  background: var(--surface-2);
  color: var(--muted);
  cursor: not-allowed;
}

/* 16px type so iOS Safari does not zoom the page when the select takes focus. */
@media (pointer: coarse) {
  .select {
    height: 40px;
    font-size: var(--text-md);
  }
}

/* The card body's own padding already spaces the first and last row, so the
   rows trim their outer edge back to it. */
.rows > :deep(*:first-child) {
  padding-top: 0;
}

.rows > :deep(*:last-child) {
  padding-bottom: 0;
}

/* First paint with nothing to show: shaped like an account row — a name line
   over its usage bars — so the layout does not jump when the data lands. */
.skeleton-list {
  display: grid;
  gap: var(--space-5);
}

.skeleton-row {
  display: grid;
  gap: var(--space-3);
}

/* Static, not pulsing: the app's motion tokens top out at 240ms, which as a
   loop is a strobe rather than a breath, and the shape alone already reads as
   "content is coming". */
.skeleton {
  display: block;
  border-radius: var(--radius-full);
  background: var(--hover);
}

/* Matched to what replaces them: the name line is one --text-sm line box, the
   bars are the height of a UsageBar track. */
.skeleton-name {
  width: 40%;
  height: var(--text-sm);
}

.skeleton-bar {
  width: 100%;
  height: var(--space-1);
}
</style>
