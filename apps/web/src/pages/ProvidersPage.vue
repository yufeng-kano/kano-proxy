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
import PageHeader from "@/components/ui/PageHeader.vue"
import SectionNav from "@/components/ui/SectionNav.vue"
import type { SectionItem } from "@/components/ui/SectionNav.vue"
import { useAccounts } from "@/composables/useAccounts"
import { useAuth } from "@/composables/useAuth"
import { useCustomProviders } from "@/composables/useCustomProviders"
import { useI18n } from "@/i18n"
import type { MessageKey } from "@/i18n"
import { deleteCustomProvider, promoteAccount, removeAccount } from "@/services/api"
import { getProvidersPrefs, setProvidersPrefs } from "@/services/prefs"
import { PROVIDERS, type CustomProvider, type ProviderAccount, type ProviderId } from "@/types"

const { t } = useI18n()
const { user } = useAuth()
const { byProvider, setUserId, loadAll, loadProvider, CACHE_TTL_MS } = useAccounts()
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

const BLURB_KEY: Record<ProviderId, MessageKey> = {
  "claude-code": "provider.claude-code.blurb",
  codex: "provider.codex.blurb",
  grok: "provider.grok.blurb",
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

onMounted(async () => {
  setUserId(user.value?.id ?? null)
  customProviders.setUserId(user.value?.id ?? null)
  // Cache-first: paint localStorage, network only when stale.
  await Promise.all([loadAll(), customProviders.load()])
  pollTimer = window.setInterval(() => {
    void loadAll()
    void customProviders.load()
  }, CACHE_TTL_MS)
})

onUnmounted(() => {
  if (pollTimer !== undefined) window.clearInterval(pollTimer)
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
    <PageHeader :title="t('providers.title')" :subtitle="t('providers.subtitle')">
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
      <AppCard
        v-for="pid in visibleProviders"
        :key="pid"
        :title="t(NAME_KEY[pid])"
        :subtitle="t(BLURB_KEY[pid])"
      >
        <template #actions>
          <!-- Icon-only ghost: this page is read far more often than it is
               added to, so the create affordance sits at icon weight. -->
          <AppButton
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

      <AppCard
        v-if="showCustomSection"
        :title="t('provider.custom.name')"
        :subtitle="t('provider.custom.blurb')"
      >
        <template #actions>
          <AppButton
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
              v-for="provider in customProviders.state.data"
              :key="provider.id"
              :provider="provider"
              :busy="customBusyId === provider.id"
              :editing="isEditing(CUSTOM)"
              @edit="openEditCustomDialog(provider)"
              @remove="onRemoveCustomProvider(provider)"
            />
          </div>

          <EmptyState
            v-else
            compact
            :title="t('custom.empty.title')"
            :body="t('custom.empty.body')"
          />
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
