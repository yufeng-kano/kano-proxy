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
 * then — defensively — any section belonging to neither, so a stale cache right
 * after an endpoint was deleted still renders its models instead of dropping
 * them silently. The catalog's fixed `group` section (the user's own model
 * groups) arrives through that last branch and needs nothing but a label.
 */
import { computed, onMounted, ref } from "vue"
import ActionIcon from "@/components/ui/ActionIcon.vue"
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

/**
 * The catalog's fixed section for model groups (docs/providers.md § Model
 * groups). Its rows are bare names, not `provider/model` ids, so it is the one
 * section whose label cannot come from a provider — hence this lookup, and
 * nothing else, special-cases it.
 */
const GROUP_SECTION = "group"

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

/** A section's visible name — the raw key unless the catalog named it for us. */
function sectionLabel(key: string): string {
  return key === GROUP_SECTION ? t("models.section.groups") : key
}

const groups = computed<ModelGroup[]>(() => {
  // Keyed on each row's own `provider`, not on the text before its first "/":
  // a group's id is a bare name, so a prefix split would file every group under
  // a section of its own.
  const byPrefix = new Map<string, CatalogModel[]>()
  for (const m of models.value) {
    const key = m.provider
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

  // 3. Whatever else the response listed: the fixed `group` section, and
  // defensively a prefix matching neither a builtin nor a known endpoint (a
  // stale cache right after a deletion) so its models still render.
  for (const [key, list] of byPrefix) {
    if (seen.has(key)) continue
    out.push({
      key,
      name: sectionLabel(key),
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

/** Names the panel after the tab that opened it — the tabs carry no ids to point at. */
const activeLabel = computed(
  () => tabs.value.find((tab) => tab.id === activeTab.value)?.label ?? t("models.all"),
)

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

/**
 * The copy column is sized to its control, not to the table's leftover width:
 * an auto-width action column stretches to a few hundred pixels and strands
 * its header at the opposite edge from the button it labels. Id and name then
 * split what is left, so a long `provider/model` gets the room it needs.
 */
const columns = computed<Column<CatalogModel>[]>(() => [
  { key: "id", header: t("models.column.id"), width: "52%" },
  { key: "name", header: t("models.column.name"), value: (m) => m.display_name },
  { key: "copy", header: t("action.copy"), align: "end", width: "72px" },
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
        <!-- Icon-only: the label is a tooltip and the accessible name, so the
             control keeps its meaning without spending header width on a word
             that repeats on every page. -->
        <AppButton
          icon-only
          :label="t('action.refresh')"
          :loading="refreshing"
          @click="onRefresh"
        >
          <template #icon><ActionIcon name="refresh" /></template>
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

    <!-- The panel the tabs point at: SectionNav's `aria-controls` is
         `panel-<id>`, and only the selected one is ever in the DOM. -->
    <AppCard
      fill
      flush
      class="catalog"
      :id="`panel-${activeTab}`"
      role="tabpanel"
      :aria-label="activeLabel"
    >
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

          <!-- Click-to-copy on the row: the id *is* what this page is for, and
               a 28px square is a small target for the only action a row has.
               The button stays a real control, so keyboard and screen-reader
               users are unaffected — this only widens the pointer target. -->
          <DataTable
            v-if="group.models.length"
            row-clickable
            :columns="columns"
            :rows="group.models"
            :row-key="(m) => m.id"
            :caption="group.name"
            @row-click="copyId($event.id)"
          >
            <template #cell-id="{ row }">
              <code class="mono model-id" :title="row.id">{{ row.id }}</code>
            </template>
            <template #cell-copy="{ row }">
              <AppButton
                size="sm"
                variant="ghost"
                icon-only
                :label="copiedId === row.id ? t('action.copied') : t('models.copyId')"
                @click.stop="copyId(row.id)"
              >
                <template #icon>
                  <ActionIcon :name="copiedId === row.id ? 'check' : 'copy'" />
                </template>
              </AppButton>
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
 * The page is exactly the content region minus its padding, which is what
 * gives `AppCard fill` a bounded box to scroll the catalog inside of instead
 * of growing the page (docs/admin-ui.md § Anti-scroll rules).
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

.catalog {
  flex: 1;
  min-height: 0;
}

/*
 * A real `width`, not a flex basis. The actions box is sized to its content,
 * and an `<input>`'s content contribution is its *default* ~170px regardless
 * of any basis — so a 260px basis exceeds the box the browser just built and
 * flex wraps Refresh onto a line of its own before shrinking can rescue it.
 * A width feeds the intrinsic sizing instead, so the box is the field plus the
 * button and the row stays one line.
 */
.search {
  display: block;
  width: 260px;
  max-width: 100%;
  min-width: 0;
}

/*
 * Narrow enough to sit beside the title on a phone rather than carrying the
 * whole actions box onto a second row. The clamp above cannot do this on its
 * own: in a wrapping flex row, **wrapping happens before shrinking**, so a box
 * whose preferred width overflows moves to the next line and never gets the
 * chance to shrink into the space it was offered.
 *
 * Layout arithmetic, so a raw px value rather than a spacing token: at the
 * narrowest phone (320px, minus this breakpoint's 16px gutters) the line holds
 * the title (~62px), two gaps, and the 40px coarse-pointer Refresh — leaving
 * ~160px. "Search models" reads fine at that width.
 */
@media (max-width: 640px) {
  .search {
    width: 150px;
  }
}

/* --- Groups ------------------------------------------------------------- */

/*
 * On the "All" tab the groups stack inside one scroll region, so each group's
 * head sticks: scrolled deep into a long catalog, "which provider is this id
 * from" stays answerable without scrolling back up. Each head is bounded by its
 * own section, so the next group's head pushes the previous one out.
 *
 * A declared height rather than a content-driven one, because the table header
 * below has to park under it exactly — a measured offset is the only way two
 * sticky bars stack without overlapping.
 */
.group {
  --group-head-height: 40px;
}

.group + .group {
  border-top: 1px solid var(--border);
}

.group-head {
  position: sticky;
  top: 0;
  /* Above DataTable's own sticky header (z-index 1), which parks beneath it. */
  z-index: 2;
  display: flex;
  align-items: center;
  gap: var(--space-2);
  height: var(--group-head-height);
  padding: 0 var(--space-4);
  /* Opaque, not translucent: rows scrolling under a semi-transparent bar smear
     into the label. */
  background: var(--surface);
}

.group :deep(.table th) {
  top: var(--group-head-height);
}

.group-name {
  margin: 0;
  flex-shrink: 0;
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
  flex-shrink: 0;
  padding-left: var(--space-2);
  color: var(--faint);
  font-size: var(--text-2xs);
}

.group-alert {
  padding: var(--space-2) var(--space-4) var(--space-3);
}

/* --- Rows --------------------------------------------------------------- */

.model-id {
  display: block;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text);
}

/* Quiet by default, full strength under the pointer: thirty outlined buttons
   down a catalog is a wall of chrome competing with the ids the page is
   actually for. Always *present* though — never revealed on hover, which would
   read as the control having disappeared. */
.group :deep(tbody tr .btn) {
  color: var(--faint);
}

.group :deep(tbody tr:hover .btn) {
  color: var(--text);
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
</style>
