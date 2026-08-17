<script setup lang="ts">
/**
 * Models: every `provider/model` id the user can call right now.
 *
 * Two filters, both client-side over already-loaded data (docs/admin-ui.md
 * § Models page): the tabs narrow to one provider group, the search box
 * narrows across ids and display names. Neither costs a request — a keystroke
 * must never hit the network.
 *
 * One card per provider group, stacked — the Providers page's layout, for the
 * same reason: each group is a separate dataset with its own identity and its
 * own upstream error. The card head is the group's label, so the table below
 * it drops its column-header row (`hideHeader`), and each card bounds its own
 * rows at `--group-max` so one auto-mode endpoint cannot set the page's length
 * (docs/admin-ui.md § Models page, § Anti-scroll rules).
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
  /**
   * The `slug/*` prefix hint of a custom endpoint — the one piece of identity
   * the group's name does not already carry, since it is what a client puts in
   * front of every model id here. A builtin has none: its name *is* its prefix,
   * and a line restating what the provider is would be the subtitle the card
   * head refuses to grow (docs/admin-ui.md § Design restraint).
   */
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
      blurb: null,
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
  <div>
    <PageHeader :title="t('models.title')">
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
         `panel-<id>`, and only the selected tab's cards are ever in the DOM. -->
    <div
      :id="`panel-${activeTab}`"
      class="sections"
      role="tabpanel"
      :aria-label="activeLabel"
    >
      <!-- The page's own states — nothing loaded, nothing anywhere, nothing
           matched — are facts about the page, so they get one card rather than
           being attributed to a provider. -->
      <AppCard v-if="showSkeleton">
        <!-- Skeletons are decoration; the status beside them is what a screen
             reader gets. -->
        <div class="skeletons">
          <span class="sr-only" role="status">{{ t("app.loading") }}</span>
          <div v-for="i in 6" :key="i" class="skeleton-row" aria-hidden="true">
            <span class="skeleton skeleton-id" />
            <span class="skeleton skeleton-name" />
          </div>
        </div>
      </AppCard>

      <AppCard v-else-if="noResults">
        <EmptyState
          :title="t('models.noResults.title')"
          :body="t('models.noResults.body', { query: trimmedQuery })"
        />
      </AppCard>

      <AppCard v-else-if="noModels">
        <EmptyState :title="t('models.empty.title')" :body="t('models.empty.body')">
          <template #action>
            <AppButton variant="primary" to="/providers">
              {{ t("models.empty.action") }}
            </AppButton>
          </template>
        </EmptyState>
      </AppCard>

      <!-- `v-else` on the wrapper, not on the card: `v-for` outranks `v-if` on
           one element, so the branch would be evaluated once per group. -->
      <template v-else>
        <AppCard v-for="group in visibleGroups" :key="group.key" flush :title="group.name">
          <!-- Identity, not explanation: the format the endpoint speaks and the
               prefix its ids carry. A builtin adds neither. -->
          <template v-if="group.formatBadge || group.blurb" #heading>
            <Badge v-if="group.formatBadge">{{ group.formatBadge }}</Badge>
            <span v-if="group.blurb" class="group-prefix mono">{{ group.blurb }}</span>
          </template>

          <template #actions>
            <span class="group-count tabular">
              {{ t("models.count", { count: group.models.length }) }}
            </span>
          </template>

          <!-- Outside the scroll region below: an upstream failure is a fact
               about the group, not about the rows it happens to sit above. -->
          <div v-if="group.error" class="group-alert">
            <Banner tone="warn">{{ group.error }}</Banner>
          </div>

          <!-- Click-to-copy on the row: the id *is* what this page is for, and
               a 28px square is a small target for the only action a row has.
               The button stays a real control, so keyboard and screen-reader
               users are unaffected — this only widens the pointer target. -->
          <div v-if="group.models.length" class="group-rows">
            <DataTable
              hide-header
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
          </div>

          <EmptyState
            v-else
            compact
            :title="t('models.empty.title')"
            :body="t(GROUP_EMPTY_KEY[group.emptyKind])"
          />
        </AppCard>
      </template>
    </div>

    <span class="sr-only" role="status" aria-live="polite">
      {{ copiedId ? t("action.copied") : "" }}
    </span>
  </div>
</template>

<style scoped>
/* PageHeader carries its own bottom margin, so the stack needs no gap above it. */
.page-alert {
  margin-bottom: var(--space-4);
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
 * One card per provider, the Providers page's stack. `--group-max` is the
 * height past which a group's rows scroll inside their own card, so the page
 * length is set by the number of providers rather than by the largest of them
 * (docs/admin-ui.md § Anti-scroll rules).
 *
 * Layout arithmetic, so a raw px rather than a spacing token: a row is 53px (a
 * 28px ghost control between two --space-3 gutters, plus its rule), so this is
 * nine rows. Every builtin catalog is smaller than that and hugs its content
 * with no scrollbar at all; a custom endpoint in auto mode listing hundreds
 * stops at roughly half a laptop viewport, which still leaves the next
 * provider's card head on screen.
 */
.sections {
  --group-max: 480px;
  display: grid;
  gap: var(--space-5);
}

/* The body is flush, so the banner brings the gutter the rows do not need. */
.group-alert {
  padding: var(--space-3) var(--space-4);
}

.group-rows {
  max-height: var(--group-max);
  overflow: auto;
}

/* Below the table breakpoint a row is a stacked card (~109px), not a 53px line,
   so the same pixel cap would bound five providers at four models each. Raised
   to hold the same handful of rows the desktop cap does. */
@media (max-width: 768px) {
  .sections {
    --group-max: 640px;
  }
}

/* The prefix a client puts in front of every id in this card — long enough on a
   narrow endpoint name to need the ellipsis rather than the head's second row. */
.group-prefix {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--muted);
  font-size: var(--text-xs);
}

/* A fact about the card's dataset, read alongside the title — so one tone down
   from it, not shrunk into 11px chrome (§ Design restraint). */
.group-count {
  color: var(--muted);
  font-size: var(--text-xs);
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
.group-rows :deep(tbody tr .btn) {
  color: var(--faint);
}

.group-rows :deep(tbody tr:hover .btn) {
  color: var(--text);
}

/* --- First paint -------------------------------------------------------- */

/* Shaped like a table row — an id line beside a name — so the layout does not
   jump when the catalog lands. Static, not pulsing: a 240ms loop is a strobe. */
.skeletons {
  display: grid;
  gap: var(--space-4);
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
