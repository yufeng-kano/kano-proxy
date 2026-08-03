<script setup lang="ts">
/**
 * API keys: the two jobs this page does are split into tabs rather than
 * stacked, so neither pushes the other off-screen (docs/admin-ui.md
 * § Anti-scroll rules).
 *
 * - **Keys** — create and revoke. A freshly created key is the only time the
 *   plaintext exists in the UI, so it gets an emphasized copy field at the top
 *   of the list rather than a row buried in it.
 * - **Connect** — the base URLs this deployment actually serves, derived from
 *   the current host, never hardcoded.
 */
import { computed, onMounted, ref } from "vue"
import AppButton from "@/components/ui/AppButton.vue"
import AppCard from "@/components/ui/AppCard.vue"
import Banner from "@/components/ui/Banner.vue"
import CopyField from "@/components/ui/CopyField.vue"
import DataTable from "@/components/ui/DataTable.vue"
import type { Column } from "@/components/ui/DataTable.vue"
import EmptyState from "@/components/ui/EmptyState.vue"
import FormField from "@/components/ui/FormField.vue"
import PageHeader from "@/components/ui/PageHeader.vue"
import SectionNav from "@/components/ui/SectionNav.vue"
import type { SectionItem } from "@/components/ui/SectionNav.vue"
import TextInput from "@/components/ui/TextInput.vue"
import { useI18n } from "@/i18n"
import { clientBaseUrls, createKey, listKeys, revokeKey } from "@/services/api"
import type { ApiKey, CreatedKey } from "@/types"

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
const creating = ref(false)
const error = ref<string | null>(null)
const name = ref("")
const created = ref<CreatedKey | null>(null)
const revokingId = ref<string | null>(null)

const baseUrls = clientBaseUrls()

const tabs = computed<SectionItem[]>(() => [
  { id: "keys", label: t("keys.tab.keys"), count: keys.value.length },
  { id: "connect", label: t("keys.tab.connect") },
])

const columns = computed<Column<ApiKey>[]>(() => [
  { key: "name", header: t("keys.column.name"), value: (k) => k.name },
  { key: "prefix", header: t("keys.column.key") },
  { key: "created", header: t("keys.column.created") },
  { key: "lastUsed", header: t("keys.column.lastUsed") },
  { key: "revoke", header: t("keys.revoke") },
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

async function onCreate() {
  if (creating.value) return
  creating.value = true
  error.value = null
  try {
    created.value = await createKey(name.value)
    name.value = ""
    await load()
  } catch {
    error.value = t("keys.error.create")
  } finally {
    creating.value = false
  }
}

async function onRevoke(key: ApiKey) {
  if (!confirm(t("keys.revokeConfirm"))) return
  revokingId.value = key.id
  error.value = null
  try {
    await revokeKey(key.id)
    // The plaintext banner outlives its key otherwise — a copy field for a
    // credential that no longer works.
    if (created.value?.id === key.id) created.value = null
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
      <div class="create">
        <FormField v-slot="field" :label="t('keys.nameLabel')" class="create-field">
          <TextInput
            :id="field.id"
            v-model="name"
            :placeholder="t('keys.namePlaceholder')"
            :described-by="field.describedBy"
            :disabled="creating"
            @enter="onCreate"
          />
        </FormField>
        <AppButton variant="primary" :loading="creating" @click="onCreate">
          {{ t("keys.create") }}
        </AppButton>
      </div>

      <!-- The one moment the plaintext exists in the UI. -->
      <Banner v-if="created" tone="ok" class="fresh">
        <div class="fresh-body">
          <strong class="fresh-title">{{ t("keys.created.title") }}</strong>
          <span class="fresh-note">{{ t("keys.created.body") }}</span>
          <CopyField emphasis :value="created.key" @error="error = $event" />
        </div>
        <template #actions>
          <AppButton size="sm" variant="ghost" @click="created = null">
            {{ t("action.done") }}
          </AppButton>
        </template>
      </Banner>

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
        />

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
          <template #cell-revoke="{ row }">
            <span class="row-action">
              <AppButton
                size="sm"
                variant="danger"
                :loading="revokingId === row.id"
                @click="onRevoke(row)"
              >
                {{ t("keys.revoke") }}
              </AppButton>
            </span>
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
  </div>
</template>

<style scoped>
/*
 * The page fills the content region exactly, so the key list can bound itself
 * and scroll inside its own card instead of growing the page. The subtracted
 * values are the region's own padding tokens, mirrored per breakpoint below.
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

/* --- Create ------------------------------------------------------------- */

.create {
  display: flex;
  align-items: flex-end;
  gap: var(--space-3);
  flex-shrink: 0;
}

.create-field {
  flex: 1 1 auto;
  max-width: 320px;
}

.fresh {
  flex-shrink: 0;
}

.fresh-body {
  display: grid;
  gap: var(--space-2);
  min-width: 0;
}

.fresh-title {
  font-weight: var(--weight-semibold);
}

.fresh-note {
  color: var(--text-secondary);
  font-size: var(--text-xs);
}

/* Banner's tone color must not bleed into the copy field's own surface. */
.fresh-body :deep(.copy) {
  color: var(--text);
}

/* --- List --------------------------------------------------------------- */

.list {
  flex: 1;
  min-height: 0;
}

.never {
  color: var(--faint);
}

.row-action {
  display: flex;
  justify-content: flex-end;
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

  .create {
    flex-wrap: wrap;
  }

  .create-field {
    flex-basis: 100%;
    max-width: none;
  }

  .detail {
    grid-template-columns: minmax(0, 1fr);
    gap: var(--space-1);
  }

  /* The row is a card here, and its action reads as the last field rather than
     something pinned to a column edge. */
  .row-action {
    justify-content: flex-start;
  }
}
</style>
