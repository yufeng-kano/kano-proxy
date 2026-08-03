<script setup lang="ts">
/**
 * Providers: one section per subscription pool plus the custom endpoints,
 * reachable from a section nav in the sticky header.
 *
 * The nav is the anti-scroll rule in practice (docs/admin-ui.md § Anti-scroll
 * rules): four stacked sections would otherwise make the last one a scroll
 * hunt. Selecting one scrolls it into the *content region* — the window never
 * scrolls in this app — and an IntersectionObserver rooted at that same region
 * keeps the nav marking wherever the user actually is.
 *
 * Data is cache-first and silent about it: the cache paints immediately, a
 * background poll keeps it warm, and neither says a word in the UI. Only the
 * Refresh the user pressed reports progress, on the button they pressed.
 */
import { computed, nextTick, onMounted, onUnmounted, ref } from "vue"
import type { ComponentPublicInstance } from "vue"
import AddAccountDialog from "@/components/AddAccountDialog.vue"
import CustomProviderDialog from "@/components/CustomProviderDialog.vue"
import AccountCard from "@/components/providers/AccountCard.vue"
import CustomProviderCard from "@/components/providers/CustomProviderCard.vue"
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
import { getScrollRegion, scrollIntoRegion } from "@/services/scrollRegion"
import { PROVIDERS, type CustomProvider, type ProviderId } from "@/types"

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

/** The custom endpoints section is not a `ProviderId`, so it gets its own id. */
const CUSTOM = "custom"

/**
 * How long a nav click owns the active marker. Smooth scrolling has no
 * completion event, and without a hold every section the scroll passes through
 * flashes active on the way to the one the user actually picked.
 */
const CLICK_HOLD_MS = 700

const busyId = ref<string | null>(null)
const customBusyId = ref<string | null>(null)
const actionError = ref<string | null>(null)
const refreshing = ref(false)

const addFor = ref<ProviderId | null>(null)
const showCustomDialog = ref(false)
const editingCustomProvider = ref<CustomProvider | null>(null)

const sections = ref<HTMLElement | null>(null)
/**
 * PageHeader's root element, reached through the component instance's `$el`.
 * Its height is what the scroll target and the observer's top margin have to
 * clear — measured rather than assumed, because the header grows a row when
 * the actions wrap on a narrow viewport.
 */
const header = ref<ComponentPublicInstance | null>(null)
const activeSection = ref<string>(PROVIDERS[0]?.id ?? CUSTOM)

let pollTimer: number | undefined
let observer: IntersectionObserver | undefined
/** The element the scroll listener is bound to, kept for teardown. */
let scrollRoot: HTMLElement | null = null
/** Timestamp until which the observer defers to the last nav click. */
let clickHoldUntil = 0

const sectionIds = computed<string[]>(() => [...PROVIDERS.map((p) => p.id), CUSTOM])

const navItems = computed<SectionItem[]>(() => [
  ...PROVIDERS.map((p) => ({
    id: p.id,
    label: t(NAME_KEY[p.id]),
    // null while nothing has loaded: a "0" the app is not sure about reads as
    // a fact, and an empty chip is more honest than a wrong one.
    count: byProvider[p.id].data?.accounts.length ?? null,
  })),
  {
    id: CUSTOM,
    label: t("providers.group.custom"),
    count: customProviders.state.data?.length ?? null,
  },
])

onMounted(async () => {
  setUserId(user.value?.id ?? null)
  customProviders.setUserId(user.value?.id ?? null)
  // Cache-first: paint localStorage, network only when stale.
  await Promise.all([loadAll(), customProviders.load()])
  pollTimer = window.setInterval(() => {
    void loadAll()
    void customProviders.load()
  }, CACHE_TTL_MS)

  // The shell publishes its scroll region in *its* mounted hook, which runs
  // after this one — a tick later it is there.
  await nextTick()
  observeSections()
})

onUnmounted(() => {
  if (pollTimer !== undefined) window.clearInterval(pollTimer)
  observer?.disconnect()
  scrollRoot?.removeEventListener("scroll", onRegionScroll)
  scrollRoot = null
})

function headerOffset(): number {
  const el: unknown = header.value?.$el
  return el instanceof HTMLElement ? el.offsetHeight : 0
}

function sectionEl(id: string): HTMLElement | null {
  return sections.value?.querySelector<HTMLElement>(`[data-section="${id}"]`) ?? null
}

function goToSection(id: string) {
  activeSection.value = id
  clickHoldUntil = Date.now() + CLICK_HOLD_MS
  scrollIntoRegion(sectionEl(id), headerOffset())
}

/**
 * Marks whichever section occupies the band just below the sticky header.
 * Rooted at the content region because that — not the window — is what
 * scrolls here.
 */
function observeSections() {
  const root = getScrollRegion()
  if (!root || !sections.value) return

  const visible = new Set<string>()
  observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        const id = entry.target instanceof HTMLElement ? entry.target.dataset.section : null
        if (!id) continue
        if (entry.isIntersecting) visible.add(id)
        else visible.delete(id)
      }
      if (Date.now() < clickHoldUntil) return
      // At the very bottom the last section can sit entirely below the band and
      // match nothing — scrolled as far as it goes *is* the last section, so it
      // wins outright rather than leaving the marker stuck one item back.
      if (atBottom(root)) {
        const last = sectionIds.value[sectionIds.value.length - 1]
        if (last) activeSection.value = last
        return
      }
      const first = sectionIds.value.find((id) => visible.has(id))
      if (first) activeSection.value = first
    },
    {
      root,
      // Top: clear the header. Bottom: only the upper band counts, so the
      // section the user is reading wins over the one merely peeking in.
      rootMargin: `-${headerOffset()}px 0px -60% 0px`,
      threshold: 0,
    },
  )

  for (const el of sections.value.querySelectorAll<HTMLElement>("[data-section]")) {
    observer.observe(el)
  }

  // The observer only fires on a crossing, and the bottom case above needs to
  // be re-evaluated on every scroll, not just when a section enters or leaves.
  root.addEventListener("scroll", onRegionScroll, { passive: true })
  scrollRoot = root
}

/** Within a pixel of the end — fractional scroll heights never land exactly. */
function atBottom(root: HTMLElement): boolean {
  return root.scrollHeight - root.scrollTop - root.clientHeight <= 1
}

function onRegionScroll() {
  if (!scrollRoot || Date.now() < clickHoldUntil) return
  if (!atBottom(scrollRoot)) return
  const last = sectionIds.value[sectionIds.value.length - 1]
  if (last) activeSection.value = last
}

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
    <PageHeader ref="header" :title="t('providers.title')" :subtitle="t('providers.subtitle')">
      <template #actions>
        <AppButton :loading="refreshing" @click="refreshAll">
          {{ t("action.refresh") }}
        </AppButton>
      </template>
      <template #nav>
        <SectionNav
          mode="anchors"
          :items="navItems"
          :active="activeSection"
          :label="t('providers.title')"
          @select="goToSection"
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

    <div ref="sections" class="sections">
      <AppCard
        v-for="p in PROVIDERS"
        :key="p.id"
        :data-section="p.id"
        :title="t(NAME_KEY[p.id])"
        :subtitle="t(BLURB_KEY[p.id])"
      >
        <template #actions>
          <AppButton size="sm" @click="addFor = p.id">
            {{ t("providers.addAccount") }}
          </AppButton>
        </template>

        <div class="section-body">
          <Banner v-if="byProvider[p.id].error" tone="warn">
            {{ t("providers.error.load", { provider: t(NAME_KEY[p.id]) }) }}
          </Banner>

          <!-- Skeletons are decoration; the status beside them is what a
               screen reader gets, same as the shell's boot loader. -->
          <div v-if="!byProvider[p.id].data" class="skeleton-list">
            <span class="sr-only" role="status">{{ t("app.loading") }}</span>
            <div v-for="i in 2" :key="i" class="skeleton-row" aria-hidden="true">
              <span class="skeleton skeleton-name" />
              <span class="skeleton skeleton-bar" />
              <span class="skeleton skeleton-bar" />
            </div>
          </div>

          <div v-else-if="byProvider[p.id].data!.accounts.length" class="rows">
            <AccountCard
              v-for="account in byProvider[p.id].data!.accounts"
              :key="account.id"
              :account="account"
              :busy="busyId === account.id"
              @promote="onPromote(p.id, account.id)"
              @remove="onRemove(p.id, account.id)"
            />
          </div>

          <EmptyState
            v-else
            compact
            :title="t('providers.empty.title', { provider: t(NAME_KEY[p.id]) })"
            :body="t('providers.empty.body', { provider: t(NAME_KEY[p.id]) })"
          />
        </div>
      </AppCard>

      <AppCard
        :data-section="CUSTOM"
        :title="t('provider.custom.name')"
        :subtitle="t('provider.custom.blurb')"
      >
        <template #actions>
          <AppButton size="sm" @click="openCreateCustomDialog">
            {{ t("custom.add") }}
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
  gap: var(--space-6);
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
