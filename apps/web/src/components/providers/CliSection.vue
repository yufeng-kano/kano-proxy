<script setup lang="ts">
/**
 * The Providers page's CLI section (docs/admin-ui.md § CLI section): the
 * CLI providers card. Devices are not here — a device is a credential and
 * lives on the Access page beside the keys it mints. No create flow: creation
 * is `kano-proxy tunnel add`'s job, and the empty state says so. Row actions
 * sit behind the card's edit gate, same convention as the page's other
 * sections, and so do each provider's request-edit switches, in one block
 * under the table.
 *
 * Data comes from the shared useCli singleton; the page owns loading and
 * polling, this component only renders and mutates. Its handle is scoped to
 * the signed-in user like the page's, so what it writes back lands in that
 * user's cache.
 */
import { computed, ref } from "vue"
import RequestEditSwitches from "@/components/providers/RequestEditSwitches.vue"
import ActionIcon from "@/components/ui/ActionIcon.vue"
import AppButton from "@/components/ui/AppButton.vue"
import AppCard from "@/components/ui/AppCard.vue"
import Badge from "@/components/ui/Badge.vue"
import Banner from "@/components/ui/Banner.vue"
import DataTable from "@/components/ui/DataTable.vue"
import type { Column } from "@/components/ui/DataTable.vue"
import EmptyState from "@/components/ui/EmptyState.vue"
import FormField from "@/components/ui/FormField.vue"
import Modal from "@/components/ui/Modal.vue"
import TextInput from "@/components/ui/TextInput.vue"
import { useAuth } from "@/composables/useAuth"
import { useCli } from "@/composables/useCli"
import { useConfirm } from "@/composables/useConfirm"
import { useI18n } from "@/i18n"
import { deleteCliProvider, renameCliProvider, updateAccountEdits } from "@/services/api"
import type { CliProvider, RequestEdits } from "@/types"

const { t, format } = useI18n()
const confirm = useConfirm()
const { user } = useAuth()
const { state, load, setUserId, setEdits } = useCli()
setUserId(user.value?.id ?? null)

const error = ref<string | null>(null)
const editing = ref(false)
const busyId = ref<string | null>(null)
/** The provider with a switch write in flight. Not `busyId`: that one spins the row's Remove. */
const settingsBusyId = ref<string | null>(null)

const renaming = ref<CliProvider | null>(null)
const renameValue = ref("")
const renameSaving = ref(false)

const providers = computed<CliProvider[]>(() => state.providers ?? [])

const columns = computed<Column<CliProvider>[]>(() => [
  { key: "provider", header: t("cli.providers.column.provider") },
  { key: "state", header: t("cli.providers.column.state") },
  { key: "models", header: t("cli.providers.column.models") },
  { key: "device", header: t("cli.providers.column.device"), hideOnMobile: true },
  ...(editing.value
    ? [{ key: "actions", header: "", srHeader: t("action.edit"), align: "end" as const, width: "148px" }]
    : []),
])

function openRename(row: CliProvider) {
  renaming.value = row
  renameValue.value = row.name
}

async function saveRename() {
  const target = renaming.value
  const name = renameValue.value.trim()
  if (!target || !name) return
  renameSaving.value = true
  error.value = null
  try {
    await renameCliProvider(target.id, name)
    renaming.value = null
    await load({ refresh: true })
  } catch {
    error.value = t("cli.error.rename")
  } finally {
    renameSaving.value = false
  }
}

async function onRemove(provider: CliProvider) {
  const confirmed = await confirm({
    title: t("cli.providers.removeTitle", { name: provider.name }),
    message: t("cli.providers.removeConfirm", { slug: provider.slug }),
    confirmLabel: t("action.remove"),
  })
  if (!confirmed) return
  busyId.value = provider.id
  error.value = null
  try {
    await deleteCliProvider(provider.id)
    await load({ refresh: true })
  } catch {
    error.value = t("cli.error.remove")
  } finally {
    busyId.value = null
  }
}

/**
 * A provider's own switches, on its one internal account row, written by the
 * account route under its slug (docs/auth.md).
 */
async function onEdits(provider: CliProvider, accountId: string, patch: Partial<RequestEdits>) {
  settingsBusyId.value = provider.id
  error.value = null
  try {
    setEdits(provider.id, await updateAccountEdits(provider.slug, accountId, patch))
  } catch {
    error.value = t("cli.error.settings")
  } finally {
    settingsBusyId.value = null
  }
}

function modelsCell(provider: CliProvider): string {
  if (!provider.models_updated_at && provider.models.length === 0) return t("cli.providers.noModels")
  return t("cli.providers.model.count", { count: provider.models.length })
}
</script>

<template>
  <Banner v-if="error || state.error" tone="error">
    {{ error || t("cli.error.loadProviders") }}
    <template #actions>
      <AppButton size="sm" variant="ghost" @click="((error = null), (state.error = null))">
        {{ t("action.dismiss") }}
      </AppButton>
    </template>
  </Banner>

  <AppCard flush :title="t('cli.providers.title')">
    <template #actions>
      <!-- One gate for the whole section. aria-pressed: the same control
           opens and closes it, so its state must be audible as one. -->
      <AppButton
        v-if="providers.length"
        icon-only
        :variant="editing ? 'primary' : 'secondary'"
        :label="
          editing
            ? t('providers.section.doneEditing', { section: t('cli.providers.title') })
            : t('providers.section.edit', { section: t('cli.providers.title') })
        "
        :aria-pressed="editing"
        @click="editing = !editing"
      >
        <template #icon><ActionIcon :name="editing ? 'check' : 'edit'" /></template>
      </AppButton>
    </template>

    <EmptyState
      v-if="!state.loading && !providers.length"
      compact
      :title="t('cli.providers.empty.title')"
      :body="t('cli.providers.empty.body')"
    />
    <DataTable
      v-else-if="providers.length"
      :columns="columns"
      :rows="providers"
      :row-key="(p: CliProvider) => p.id"
      :caption="t('cli.providers.title')"
    >
      <template #cell-provider="{ row }">
        <div class="provider-cell">
          <span class="name">{{ row.name }}</span>
          <span class="meta">
            <code class="mono slug">{{ row.slug }}/*</code>
            <Badge tone="neutral">
              {{ row.format === "anthropic" ? t("custom.dialog.formatAnthropic") : t("custom.dialog.formatOpenAI") }}
            </Badge>
            <Badge v-if="row.strip_fast_mode">{{ t("providers.edits.fastModeOn") }}</Badge>
            <Badge v-if="row.strip_claude_code_line">{{ t("providers.edits.lineOn") }}</Badge>
          </span>
        </div>
      </template>
      <template #cell-state="{ row }">
        <!-- Dot + label, never color alone (docs/admin-ui.md § A11y floor). -->
        <span class="conn" :class="{ on: row.connected }">
          <span class="conn-dot" aria-hidden="true" />
          {{ row.connected ? t("cli.providers.connected") : t("cli.providers.offline") }}
        </span>
      </template>
      <template #cell-models="{ row }">
        <span>{{ modelsCell(row) }}</span>
        <span
          v-if="row.models_updated_at"
          class="report-time"
          :title="format.dateTime(row.models_updated_at)"
        >
          {{ format.relative(row.models_updated_at) }}
        </span>
      </template>
      <template #cell-device="{ row }">
        <span v-if="row.device_name">{{ row.device_name }}</span>
        <span v-else class="never">—</span>
      </template>
      <template #cell-actions="{ row }">
        <div class="row-actions">
          <AppButton
            icon-only
            variant="ghost"
            :label="t('cli.providers.renameName', { name: row.name })"
            @click="openRename(row)"
          >
            <template #icon><ActionIcon name="edit" /></template>
          </AppButton>
          <AppButton
            icon-only
            variant="danger"
            :label="t('cli.providers.removeName', { name: row.name })"
            :loading="busyId === row.id"
            @click="onRemove(row)"
          >
            <template #icon><ActionIcon name="trash" /></template>
          </AppButton>
        </div>
      </template>
    </DataTable>

    <!-- Under the table rather than in it: two switches with a line each do
         not fit a cell. One group per provider, named after it, since every
         group offers the same two switches. -->
    <div v-if="editing && providers.some((p) => p.account_id)" class="edits">
      <template v-for="row in providers" :key="row.id">
        <!-- A provider whose account row is missing has nowhere to store
             its switches, so it gets no group. -->
        <div v-if="row.account_id" class="edit" role="group" :aria-label="row.name">
          <span class="edit-name">
            {{ row.name }}
            <code class="mono slug">{{ row.slug }}/*</code>
          </span>
          <RequestEditSwitches
            :strip-fast-mode="!!row.strip_fast_mode"
            :strip-claude-code-line="!!row.strip_claude_code_line"
            :fast-mode-hint="t('providers.edits.fastModeHintEndpoint')"
            :line-hint="t('providers.edits.lineHintEndpoint')"
            :busy="settingsBusyId === row.id"
            @change="onEdits(row, row.account_id, $event)"
          />
        </div>
      </template>
    </div>
  </AppCard>

  <Modal v-if="renaming" :title="t('cli.rename.title')" @close="renaming = null">
    <form class="rename-form" @submit.prevent="saveRename">
      <FormField v-slot="field" :label="t('cli.rename.label')">
        <TextInput
          :id="field.id"
          v-model="renameValue"
          :described-by="field.describedBy"
          :disabled="renameSaving"
        />
      </FormField>
      <div class="rename-actions">
        <AppButton variant="ghost" @click="renaming = null">{{ t("action.cancel") }}</AppButton>
        <AppButton variant="primary" type="submit" :loading="renameSaving">
          {{ t("action.done") }}
        </AppButton>
      </div>
    </form>
  </Modal>
</template>

<style scoped>
.name {
  font-weight: var(--weight-medium);
  margin-right: var(--space-2);
}

.never {
  color: var(--faint);
}

.provider-cell {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.meta {
  display: inline-flex;
  align-items: center;
  flex-wrap: wrap;
  gap: var(--space-2);
}

.slug {
  color: var(--muted);
  font-size: var(--text-xs);
}

.conn {
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
  color: var(--muted);
  font-size: var(--text-xs);
}

.conn-dot {
  width: 7px;
  height: 7px;
  border-radius: var(--radius-full);
  background: var(--faint);
}

.conn.on {
  color: var(--ok);
}

.conn.on .conn-dot {
  background: var(--ok);
}

.report-time {
  display: block;
  color: var(--muted);
  font-size: var(--text-xs);
}

.row-actions {
  display: inline-flex;
  align-items: center;
  justify-content: flex-end;
  gap: var(--space-1);
}

.edits {
  display: grid;
  border-top: 1px solid var(--border);
  background: var(--surface-2);
}

.edit {
  display: grid;
  gap: var(--space-3);
  padding: var(--space-4) var(--space-5);
}

.edit + .edit {
  border-top: 1px solid var(--border);
}

.edit-name {
  display: inline-flex;
  align-items: baseline;
  gap: var(--space-2);
  font-size: var(--text-sm);
  font-weight: var(--weight-medium);
}

@media (max-width: 640px) {
  .edit {
    padding: var(--space-4);
  }
}

.rename-form {
  display: grid;
  gap: var(--space-4);
}

.rename-actions {
  display: flex;
  justify-content: flex-end;
  gap: var(--space-2);
}
</style>
