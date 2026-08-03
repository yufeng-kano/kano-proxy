<script setup lang="ts">
/**
 * Models: every `provider/model` id the user can call right now.
 *
 * Two filters, both client-side over already-loaded data (docs/admin-ui.md
 * § Models page): the tabs narrow to one provider group, the search box
 * narrows across ids and display names. Neither costs a request — a keystroke
 * must never hit the network.
 *
 * The catalog lives in one bounded card that scrolls inside itself, so the
 * header (search, Refresh, tabs) stays reachable at any depth instead of
 * scrolling away above a thousand-row list.
 *
 * Groups are built from whatever the response lists, not from a fixed set: the
 * three builtins in their declared order, then one group per custom endpoint,
 * then — defensively — any prefix belonging to neither, so a stale cache right
 * after an endpoint was deleted still renders its models instead of dropping
 * them silently.
 */
import { computed, onMounted, ref } from "vue"
import AppButton from "@/components/ui/AppButton.vue"
import AppCard from "@/components/ui/AppCard.vue"
import Badge from "@/components/ui/Badge.vue"
import Banner from "@/components/ui/Banner.vue"
import DataTable from "@/components/ui/DataTable.vue"
import type { Column } from "@/components/ui/DataTable.vue"
import EmptyState from "@/components/ui/EmptyState.vue"
import PageHeader from "@/components/ui/PageHeader.vue"
import SectionNav from "@/components/ui/SectionNav.vue"
import type { SectionItem } from "@/components/ui/SectionNav.vue"
import TextInput from "@/components/ui/TextInput.vue"
import { useAuth } from "@/composables/useAuth"
import { useCustomProviders } from "@/composables/useCustomProviders"
import { useI18n } from "@/i18n"
import type { MessageKey } from "@/i18n"
import { listModels } from "@/services/api"
import {
  CACHE_TTL_MS,
  isModelsCacheFresh,
  readModelsCache,
  writeModelsCache,
} from "@/services/cache"
import { getModelsPrefs, setModelsPrefs } from "@/services/prefs"
import {
  PROVIDERS,
  type CatalogModel,
  type ModelsResponse,
  type ProviderId,
} from "@/types"

type EmptyKind = "codex" | "custom" | "generic"

type ModelGroup = {
  key: string
  name: string
  /** Subtitle under the group name — a builtin's blurb, or `slug/*` for a custom endpoint. */
  blurb: string | null
  formatBadge: string | null
  models: CatalogModel[]
  /** Upstream error text: server data, shown verbatim, not catalog copy. */
  error: string | null
  emptyKind: EmptyKind
}

const { t } = useI18n()
const { user } = useAuth()
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

/** Why a group is empty decides what it tells the user to do about it. */
const GROUP_EMPTY_KEY: Record<EmptyKind, MessageKey> = {
  codex: "models.group.emptyCodex",
  custom: "models.group.emptyCustom",
  generic: "models.group.emptyGeneric",
}

/**
 * The "everything" tab's id. Underscored so it can never collide with a custom
 * endpoint's slug, which is restricted to lowercase letters, digits, and
 * hyphens.
 */
const ALL = "__all__"

const models = ref<CatalogModel[]>([])
const providerMeta = ref<ModelsResponse["providers"]>([])
const loading = ref(true)
const refreshing = ref(false)
const error = ref<string | null>(null)
const query = ref("")
const copiedId = ref<string | null>(null)
/** What the user last picked; resolved against the live groups below. */
const selected = ref<string>(getModelsPrefs().provider ?? ALL)

let copyTimer: number | undefined

/** Split on the first "/" only — an upstream id may itself contain further "/". */
function prefixOf(id: string): string {
  const i = id.indexOf("/")
  return i === -1 ? id : id.slice(0, i)
}

const groups = computed<ModelGroup[]>(() => {
  const byPrefix = new Map<string, CatalogModel[]>()
  for (const m of models.value) {
    const key = prefixOf(m.id)
    const list = byPrefix.get(key)
    if (list) list.push(m)
    else byPrefix.set(key, [m])
  }
  const metaFor = (key: string) => providerMeta.value?.find((x) => x.provider === key)

  const out: ModelGroup[] = []
  const seen = new Set<string>()

  // 1. The builtin providers, in their declared order.
  for (const p of PROVIDERS) {
    seen.add(p.id)
    out.push({
      key: p.id,
      name: t(NAME_KEY[p.id]),
      blurb: t(BLURB_KEY[p.id]),
      formatBadge: null,
      models: byPrefix.get(p.id) ?? [],
      error: metaFor(p.id)?.error ?? null,
      emptyKind: p.id === "codex" ? "codex" : "generic",
    })
  }

  // 2. One group per custom endpoint the user has defined, in API order.
  for (const cp of customProviders.state.data ?? []) {
    seen.add(cp.slug)
    out.push({
      key: cp.slug,
      name: cp.name,
      blurb: `${cp.slug}/*`,
      formatBadge:
        cp.format === "anthropic"
          ? t("custom.dialog.formatAnthropic")
          : t("custom.dialog.formatOpenAI"),
      models: byPrefix.get(cp.slug) ?? [],
      error: metaFor(cp.slug)?.error ?? null,
      emptyKind: "custom",
    })
  }

  // 3. Defensive: a prefix matching neither a builtin nor a known endpoint
  // (a stale cache right after a deletion) still renders.
  for (const [key, list] of byPrefix) {
    if (seen.has(key)) continue
    out.push({
      key,
      name: key,
      blurb: null,
      formatBadge: null,
      models: list,
      error: metaFor(key)?.error ?? null,
      emptyKind: "generic",
    })
  }

  return out
})

const trimmedQuery = computed(() => query.value.trim())
const hasQuery = computed(() => trimmedQuery.value.length > 0)

/** Every group with the search applied to its rows. */
const searched = computed<ModelGroup[]>(() => {
  const q = trimmedQuery.value.toLowerCase()
  if (!q) return groups.value
  return groups.value.map((group) => ({
    ...group,
    models: group.models.filter(
      (m) =>
        m.id.toLowerCase().includes(q) || m.display_name.toLowerCase().includes(q),
    ),
  }))
})

/**
 * A stored provider that no longer exists resolves to "all" rather than an
 * empty page — but the stored value itself is left alone, so a filter picked
 * before the catalog loaded survives the first paint.
 */
const activeTab = computed(() =>
  groups.value.some((g) => g.key === selected.value) ? selected.value : ALL,
)

/** The groups the active tab covers, before empty ones are dropped. */
const scoped = computed<ModelGroup[]>(() =>
  activeTab.value === ALL
    ? searched.value
    : searched.value.filter((g) => g.key === activeTab.value),
)

const shownCount = computed(() =>
  scoped.value.reduce((n, g) => n + g.models.length, 0),
)

/** A search hides the groups it did not match; without one, empty groups explain themselves. */
const visibleGroups = computed(() =>
  hasQuery.value ? scoped.value.filter((g) => g.models.length > 0) : scoped.value,
)

const tabs = computed<SectionItem[]>(() => [
  {
    id: ALL,
    label: t("models.all"),
    count: searched.value.reduce((n, g) => n + g.models.length, 0),
  },
  ...searched.value.map((g) => ({ id: g.key, label: g.name, count: g.models.length })),
])

const showSkeleton = computed(() => loading.value && models.value.length === 0)
const noResults = computed(() => hasQuery.value && shownCount.value === 0)
/**
 * Nothing anywhere, on the tab that covers everything — the one case where
 * "connect a provider" is the whole answer. A single group being empty is a
 * different question, and its own empty state answers it.
 */
const noModels = computed(
  () => !hasQuery.value && models.value.length === 0 && activeTab.value === ALL,
)

const columns = computed<Column<CatalogModel>[]>(() => [
  { key: "id", header: t("models.column.id"), width: "46%" },
  { key: "name", header: t("models.column.name"), value: (m) => m.display_name },
  { key: "copy", header: t("action.copy") },
])

onMounted(() => void load())

function onSelect(id: string) {
  selected.value = id
  setModelsPrefs({ provider: id === ALL ? null : id })
}

async function onRefresh() {
  refreshing.value = true
  try {
    await load({ force: true })
  } finally {
    refreshing.value = false
  }
}

/**
 * Cache-first: paint whatever is on disk, then go to the network only when it
 * is stale or the user asked. Silent about all of it — freshness is the app's
 * job, not something the page narrates.
 */
async function load(opts?: { force?: boolean }) {
  const uid = user.value?.id ?? null
  error.value = null
  customProviders.setUserId(uid)

  if (!opts?.force) {
    const cached = readModelsCache(uid)
    if (cached) {
      applyResponse(cached)
      loading.value = false
      if (isModelsCacheFresh(uid, CACHE_TTL_MS)) {
        void customProviders.load()
        return
      }
    }
  }

  if (!models.value.length) loading.value = true
  try {
    const [res] = await Promise.all([
      listModels({ refresh: !!opts?.force }),
      customProviders.load({ refresh: !!opts?.force }),
    ])
    applyResponse(res)
    writeModelsCache(uid, res)
  } catch {
    // Keep whatever is already painted; the banner is non-blocking.
    error.value = t("models.error.load")
  } finally {
    loading.value = false
  }
}

function applyResponse(res: ModelsResponse) {
  models.value = res.data
  providerMeta.value = res.providers ?? []
}

async function copyId(id: string) {
  try {
    await navigator.clipboard.writeText(id)
    copiedId.value = id
    window.clearTimeout(copyTimer)
    copyTimer = window.setTimeout(() => {
      if (copiedId.value === id) copiedId.value = null
    }, 1600)
  } catch {
    error.value = t("state.copyFailed")
  }
}
</script>

<template>
  <div class="page">
    <PageHeader :title="t('models.title')" :subtitle="t('models.subtitle')">
      <template #actions>
        <!-- The wrapping label is the field's accessible name; the placeholder
             carries the same words visually. -->
        <label class="search">
          <span class="sr-only">{{ t("action.search") }}</span>
          <TextInput
            v-model="query"
            type="search"
            :placeholder="t('models.searchPlaceholder')"
          />
        </label>
        <AppButton :loading="refreshing" @click="onRefresh">
          {{ t("action.refresh") }}
        </AppButton>
      </template>

      <template #nav>
        <SectionNav
          :items="tabs"
          :active="activeTab"
          :label="t('models.title')"
          @select="onSelect"
        />
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

    <AppCard fill flush class="catalog">
      <!-- Skeletons are decoration; the status beside them is what a screen
           reader gets. -->
      <div v-if="showSkeleton" class="skeletons">
        <span class="sr-only" role="status">{{ t("app.loading") }}</span>
        <div v-for="i in 6" :key="i" class="skeleton-row" aria-hidden="true">
          <span class="skeleton skeleton-id" />
          <span class="skeleton skeleton-name" />
        </div>
      </div>

      <EmptyState
        v-else-if="noResults"
        :title="t('models.noResults.title')"
        :body="t('models.noResults.body', { query: trimmedQuery })"
      />

      <EmptyState
        v-else-if="noModels"
        :title="t('models.empty.title')"
        :body="t('models.empty.body')"
      >
        <template #action>
          <AppButton variant="primary" to="/providers">
            {{ t("models.empty.action") }}
          </AppButton>
        </template>
      </EmptyState>

      <template v-else>
        <section v-for="group in visibleGroups" :key="group.key" class="group">
          <div class="group-head">
            <h2 class="group-name">{{ group.name }}</h2>
            <Badge v-if="group.formatBadge">{{ group.formatBadge }}</Badge>
            <span v-if="group.blurb" class="group-blurb mono">{{ group.blurb }}</span>
            <span class="group-count tabular">
              {{ t("models.count", { count: group.models.length }) }}
            </span>
          </div>

          <div v-if="group.error" class="group-alert">
            <Banner tone="warn">{{ group.error }}</Banner>
          </div>

          <DataTable
            v-if="group.models.length"
            :columns="columns"
            :rows="group.models"
            :row-key="(m) => m.id"
            :caption="group.name"
          >
            <template #cell-id="{ row }">
              <code class="mono model-id" :title="row.id">{{ row.id }}</code>
            </template>
            <template #cell-copy="{ row }">
              <span class="row-action">
                <AppButton size="sm" @click="copyId(row.id)">
                  {{ copiedId === row.id ? t("action.copied") : t("models.copyId") }}
                </AppButton>
              </span>
            </template>
          </DataTable>

          <EmptyState
            v-else
            compact
            :title="t('models.empty.title')"
            :body="t(GROUP_EMPTY_KEY[group.emptyKind])"
          />
        </section>
      </template>

      <span class="sr-only" role="status" aria-live="polite">
        {{ copiedId ? t("action.copied") : "" }}
      </span>
    </AppCard>
  </div>
</template>

<style scoped>
/*
 * The page is exactly the content region minus its padding, so the catalog
 * card can bound itself and scroll internally instead of growing the page
 * (docs/admin-ui.md § Anti-scroll rules). The subtracted values are the
 * region's own padding tokens, mirrored per breakpoint below.
 */
.page {
  display: flex;
  flex-direction: column;
  height: calc(100dvh - var(--space-6) - var(--space-12));
}

.page-alert {
  flex-shrink: 0;
  margin-bottom: var(--space-4);
}

.catalog {
  flex: 1;
  min-height: 0;
}

.search {
  display: block;
  flex: 1 1 200px;
  min-width: 0;
  max-width: 260px;
}

/* --- Groups ------------------------------------------------------------- */

.group + .group {
  border-top: 1px solid var(--border);
}

.group-head {
  display: flex;
  align-items: baseline;
  gap: var(--space-2);
  flex-wrap: wrap;
  padding: var(--space-4) var(--space-4) var(--space-3);
}

.group-name {
  margin: 0;
  font-size: var(--text-sm);
  font-weight: var(--weight-semibold);
  letter-spacing: var(--tracking-tight);
}

.group-blurb {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--muted);
  font-size: var(--text-xs);
}

.group-count {
  margin-left: auto;
  color: var(--faint);
  font-size: var(--text-2xs);
}

.group-alert {
  padding: 0 var(--space-4) var(--space-3);
}

/* --- Rows --------------------------------------------------------------- */

.model-id {
  display: block;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text);
}

.row-action {
  display: flex;
  justify-content: flex-end;
}

/* --- First paint -------------------------------------------------------- */

/* Shaped like a table row — an id line beside a name — so the layout does not
   jump when the catalog lands. Static, not pulsing: a 240ms loop is a strobe. */
.skeletons {
  display: grid;
  gap: var(--space-4);
  padding: var(--space-5) var(--space-4);
}

.skeleton-row {
  display: grid;
  grid-template-columns: minmax(0, 1.6fr) minmax(0, 1fr);
  gap: var(--space-4);
}

.skeleton {
  display: block;
  height: var(--text-sm);
  border-radius: var(--radius-full);
  background: var(--hover);
}

.skeleton-id {
  width: 80%;
}

.skeleton-name {
  width: 55%;
}

@media (max-width: 1080px) {
  /* The mobile bar above the content region takes its height out too. */
  .page {
    height: calc(100dvh - var(--header-height) - var(--space-6) - var(--space-12));
  }
}

@media (max-width: 640px) {
  .page {
    height: calc(100dvh - var(--header-height) - var(--space-4) - var(--space-10));
  }

  .search {
    max-width: none;
  }

  /* The row is a card here, and its action reads as the last field rather than
     something pinned to a column edge. */
  .row-action {
    justify-content: flex-start;
  }
}
</style>
