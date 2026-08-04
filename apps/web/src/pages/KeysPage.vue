<script setup lang="ts">
/**
 * API keys: the two jobs this page does are split into tabs rather than
 * stacked, so neither pushes the other off-screen (docs/admin-ui.md
 * § Anti-scroll rules).
 *
 * - **Keys** — create, edit, revoke. Creation and editing run in a dialog
 *   (CreateKeyDialog); a fresh key's plaintext is shown once inside that
 *   dialog's done step and nowhere else, while the list refreshes behind it.
 * - **Connect** — the base URLs this deployment actually serves, derived from
 *   the current host, never hardcoded.
 */
import { computed, onMounted, ref } from "vue"
import CreateKeyDialog from "@/components/CreateKeyDialog.vue"
import ActionIcon from "@/components/ui/ActionIcon.vue"
import AppButton from "@/components/ui/AppButton.vue"
import AppCard from "@/components/ui/AppCard.vue"
import Banner from "@/components/ui/Banner.vue"
import CopyField from "@/components/ui/CopyField.vue"
import DataTable from "@/components/ui/DataTable.vue"
import type { Column } from "@/components/ui/DataTable.vue"
import EmptyState from "@/components/ui/EmptyState.vue"
import PageHeader from "@/components/ui/PageHeader.vue"
import SectionNav from "@/components/ui/SectionNav.vue"
import type { SectionItem } from "@/components/ui/SectionNav.vue"
import { useI18n } from "@/i18n"
import { clientBaseUrls, listKeys, revokeKey } from "@/services/api"
import type { ApiKey } from "@/types"

type Tab = "keys" | "connect"

/**
 * Wire identifiers, not copy: the header name a client sends and the shape of
 * a model id on both bases (docs/api.md). They are protocol, so they read the
 * same in every locale — same call as the `slug/*` preview in the custom
 * endpoint dialog.
 */
const API_KEY_HEADER = "x-api-key"
const MODEL_ID_FORM = "provider/model"

const { t, format } = useI18n()

const tab = ref<Tab>("keys")
const keys = ref<ApiKey[]>([])
const loading = ref(true)
const error = ref<string | null>(null)
const revokingId = ref<string | null>(null)

const showDialog = ref(false)
const editingKey = ref<ApiKey | null>(null)

const baseUrls = clientBaseUrls()

const tabs = computed<SectionItem[]>(() => [
  { id: "keys", label: t("keys.tab.keys"), count: keys.value.length },
  { id: "connect", label: t("keys.tab.connect") },
])

/**
 * Action columns (edit, revoke) size to their controls rather than taking the
 * table's leftover width, which would strand their headers at the far edge of
 * the track from the buttons they label.
 */
const columns = computed<Column<ApiKey>[]>(() => [
  { key: "name", header: t("keys.column.name"), value: (k) => k.name },
  { key: "prefix", header: t("keys.column.key") },
  { key: "limit", header: t("keys.column.limit"), numeric: true },
  { key: "created", header: t("keys.column.created"), hideOnMobile: true },
  { key: "lastUsed", header: t("keys.column.lastUsed") },
  { key: "edit", header: t("action.edit"), align: "end", width: "64px" },
  { key: "revoke", header: t("keys.revoke"), align: "end", width: "112px" },
])

onMounted(() => void load())

function onSelectTab(id: string) {
  tab.value = id === "connect" ? "connect" : "keys"
}

async function load() {
  error.value = null
  try {
    keys.value = await listKeys()
  } catch {
    error.value = t("keys.error.load")
  } finally {
    loading.value = false
  }
}

function openCreate() {
  editingKey.value = null
  showDialog.value = true
}

function openEdit(key: ApiKey) {
  editingKey.value = key
  showDialog.value = true
}

function closeDialog() {
  showDialog.value = false
  editingKey.value = null
}

/**
 * The key's spend against its ceiling: "$3.20 / $50.00", spend alone when
 * unlimited, an em dash when the window sum was unreadable.
 */
function spendCell(key: ApiKey): string {
  const spend = format.currency(key.window_spend ?? null)
  if (key.spend_limit == null) return spend
  return `${spend} / ${format.currency(key.spend_limit)}`
}

async function onRevoke(key: ApiKey) {
  if (!confirm(t("keys.revokeConfirm"))) return
  revokingId.value = key.id
  error.value = null
  try {
    await revokeKey(key.id)
    await load()
  } catch {
    error.value = t("keys.error.revoke")
  } finally {
    revokingId.value = null
  }
}
</script>

<template>
  <div class="page">
    <PageHeader :title="t('keys.title')" :subtitle="t('keys.subtitle')">
      <template #actions>
        <AppButton v-if="tab === 'keys'" variant="primary" @click="openCreate">
          <template #icon><ActionIcon name="plus" /></template>
          {{ t("keys.create") }}
        </AppButton>
      </template>
      <template #nav>
        <SectionNav
          :items="tabs"
          :active="tab"
          :label="t('keys.title')"
          @select="onSelectTab"
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

    <!-- --- Keys ---------------------------------------------------------- -->
    <section
      v-if="tab === 'keys'"
      id="panel-keys"
      class="panel"
      role="tabpanel"
      :aria-label="t('keys.tab.keys')"
    >
      <AppCard fill flush class="list">
        <div v-if="loading" class="skeletons">
          <span class="sr-only" role="status">{{ t("app.loading") }}</span>
          <div v-for="i in 3" :key="i" class="skeleton-row" aria-hidden="true">
            <span class="skeleton skeleton-name" />
            <span class="skeleton skeleton-meta" />
          </div>
        </div>

        <EmptyState
          v-else-if="!keys.length"
          :title="t('keys.empty.title')"
          :body="t('keys.empty.body')"
        >
          <template #action>
            <AppButton variant="primary" @click="openCreate">
              {{ t("keys.create") }}
            </AppButton>
          </template>
        </EmptyState>

        <DataTable
          v-else
          :columns="columns"
          :rows="keys"
          :row-key="(k) => k.id"
          :caption="t('keys.title')"
        >
          <template #cell-prefix="{ row }">
            <code class="mono">{{ row.key_prefix }}</code>
          </template>
          <template #cell-limit="{ row }">
            <span class="tabular">{{ spendCell(row) }}</span>
            <span v-if="row.spend_limit == null" class="unlimited">
              {{ t("keys.limit.none") }}
            </span>
          </template>
          <template #cell-created="{ row }">
            <span :title="format.dateTime(row.created_at)">
              {{ format.date(row.created_at) }}
            </span>
          </template>
          <template #cell-lastUsed="{ row }">
            <span v-if="row.last_used_at" :title="format.dateTime(row.last_used_at)">
              {{ format.relative(row.last_used_at) }}
            </span>
            <span v-else class="never">{{ t("state.never") }}</span>
          </template>
          <template #cell-edit="{ row }">
            <AppButton
              size="sm"
              variant="ghost"
              icon-only
              :label="t('keys.editKey', { name: row.name })"
              @click="openEdit(row)"
            >
              <template #icon><ActionIcon name="edit" /></template>
            </AppButton>
          </template>
          <template #cell-revoke="{ row }">
            <AppButton
              size="sm"
              variant="danger"
              :loading="revokingId === row.id"
              @click="onRevoke(row)"
            >
              {{ t("keys.revoke") }}
            </AppButton>
          </template>
        </DataTable>
      </AppCard>
    </section>

    <!-- --- Connect ------------------------------------------------------- -->
    <section
      v-else
      id="panel-connect"
      class="panel panel-scroll"
      role="tabpanel"
      :aria-label="t('keys.tab.connect')"
    >
      <AppCard :title="t('keys.connect.title')" :subtitle="t('keys.connect.body')">
        <div class="connect">
          <CopyField
            :label="t('keys.connect.openai')"
            :value="baseUrls.openai"
            @error="error = $event"
          />
          <CopyField
            :label="t('keys.connect.anthropic')"
            :value="baseUrls.anthropic"
            @error="error = $event"
          />

          <dl class="details">
            <div class="detail">
              <dt>{{ t("keys.connect.auth") }}</dt>
              <dd>{{ t("keys.connect.authValue", { header: API_KEY_HEADER }) }}</dd>
            </div>
          </dl>

          <p class="hint">{{ t("keys.connect.modelHint", { example: MODEL_ID_FORM }) }}</p>
        </div>
      </AppCard>
    </section>

    <CreateKeyDialog
      v-if="showDialog"
      :editing="editingKey"
      @close="closeDialog"
      @saved="load"
    />
  </div>
</template>

<style scoped>
/*
 * The page fills the content region exactly, which is what gives
 * `AppCard fill` a bounded box to scroll the key list inside of instead of
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

.panel {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
  flex: 1;
  min-height: 0;
}

/* Connect is short and fixed; it only ever scrolls on a very short viewport. */
.panel-scroll {
  overflow-y: auto;
}

/* --- List --------------------------------------------------------------- */

.list {
  flex: 1;
  min-height: 0;
}

.never {
  color: var(--faint);
}

.unlimited {
  display: block;
  margin-top: 2px;
  color: var(--faint);
  font-size: var(--text-2xs);
}

/* --- Connect ------------------------------------------------------------ */

.connect {
  display: grid;
  gap: var(--space-3);
}

.details {
  display: grid;
  gap: var(--space-2);
  margin: 0;
  padding-top: var(--space-1);
}

.detail {
  display: grid;
  grid-template-columns: 128px minmax(0, 1fr);
  align-items: baseline;
  gap: var(--space-3);
}

.detail dt {
  color: var(--muted);
  font-size: var(--text-xs);
}

.detail dd {
  margin: 0;
  min-width: 0;
  color: var(--text-secondary);
  font-size: var(--text-sm);
  overflow-wrap: anywhere;
}

.hint {
  margin: 0;
  color: var(--muted);
  font-size: var(--text-xs);
  overflow-wrap: anywhere;
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

@media (max-width: 640px) {
  .detail {
    grid-template-columns: minmax(0, 1fr);
    gap: var(--space-1);
  }
}
</style>
