<script setup lang="ts">
/**
 * CLI devices and CLI providers (docs/cli.md, docs/admin-ui.md § CLI page).
 *
 * Two genuinely separate datasets, two cards. No create flows here — creation
 * is the CLI's job, so the page-level empty state teaches the install +
 * `kano-proxy init` path instead. Row actions sit behind each card's edit
 * gate, same convention as the Providers page.
 */
import { computed, ref, watch } from "vue"
import ActionIcon from "@/components/ui/ActionIcon.vue"
import AppButton from "@/components/ui/AppButton.vue"
import AppCard from "@/components/ui/AppCard.vue"
import Badge from "@/components/ui/Badge.vue"
import Banner from "@/components/ui/Banner.vue"
import CopyField from "@/components/ui/CopyField.vue"
import DataTable from "@/components/ui/DataTable.vue"
import type { Column } from "@/components/ui/DataTable.vue"
import EmptyState from "@/components/ui/EmptyState.vue"
import FormField from "@/components/ui/FormField.vue"
import Modal from "@/components/ui/Modal.vue"
import PageHeader from "@/components/ui/PageHeader.vue"
import TextInput from "@/components/ui/TextInput.vue"
import { useAuth } from "@/composables/useAuth"
import { useCli } from "@/composables/useCli"
import { useI18n } from "@/i18n"
import { deleteCliProvider, renameCliProvider, revokeCliDevice } from "@/services/api"
import type { CliDevice, CliProvider } from "@/types"

/**
 * Wire identifiers, not copy (docs/i18n.md): the install commands and the
 * Releases URL are the same for every instance — instances never host
 * binaries themselves (docs/cli.md § Distribution).
 */
const INSTALL_BREW = "brew install yufeng-kano/tap/kano-proxy"
const INSTALL_SCRIPT =
  "curl -fsSL https://raw.githubusercontent.com/yufeng-kano/kano-proxy/main/scripts/install-cli.sh | sh"
const INIT_COMMAND = "kano-proxy init"
const RELEASES_URL = "https://github.com/yufeng-kano/kano-proxy/releases"

const { t, format } = useI18n()
const { user } = useAuth()
const { state, setUserId, load } = useCli()

const error = ref<string | null>(null)
const editingDevices = ref(false)
const editingProviders = ref(false)
const busyId = ref<string | null>(null)

const renaming = ref<CliProvider | null>(null)
const renameValue = ref("")
const renameSaving = ref(false)

const devices = computed<CliDevice[]>(() => state.devices ?? [])
const providers = computed<CliProvider[]>(() => state.providers ?? [])
const isEmpty = computed(
  () => !state.loading && devices.value.length === 0 && providers.value.length === 0,
)

const deviceColumns = computed<Column<CliDevice>[]>(() => [
  { key: "name", header: t("cli.devices.column.name") },
  { key: "lastSeen", header: t("cli.devices.column.lastSeen") },
  { key: "created", header: t("cli.devices.column.created"), hideOnMobile: true },
  ...(editingDevices.value
    ? [{ key: "actions", header: "", srHeader: t("action.edit"), align: "end" as const, width: "96px" }]
    : []),
])

const providerColumns = computed<Column<CliProvider>[]>(() => [
  { key: "provider", header: t("cli.providers.column.provider") },
  { key: "state", header: t("cli.providers.column.state") },
  { key: "models", header: t("cli.providers.column.models") },
  { key: "device", header: t("cli.providers.column.device"), hideOnMobile: true },
  ...(editingProviders.value
    ? [{ key: "actions", header: "", srHeader: t("action.edit"), align: "end" as const, width: "148px" }]
    : []),
])

// The immediate watch covers mount too — a separate onMounted load would
// double-fetch the first paint.
watch(
  () => user.value?.id ?? null,
  (id) => {
    setUserId(id)
    if (id) void load()
  },
  { immediate: true },
)

async function onRevoke(device: CliDevice) {
  if (!confirm(t("cli.devices.revokeConfirm", { name: device.name }))) return
  busyId.value = device.id
  error.value = null
  try {
    await revokeCliDevice(device.id)
    await load({ refresh: true })
  } catch {
    error.value = t("cli.error.revoke")
  } finally {
    busyId.value = null
  }
}

function openRename(provider: CliProvider) {
  renaming.value = provider
  renameValue.value = provider.name
}

async function saveRename() {
  const provider = renaming.value
  const name = renameValue.value.trim()
  if (!provider || !name) return
  renameSaving.value = true
  error.value = null
  try {
    await renameCliProvider(provider.id, name)
    renaming.value = null
    await load({ refresh: true })
  } catch {
    error.value = t("cli.error.rename")
  } finally {
    renameSaving.value = false
  }
}

async function onRemove(provider: CliProvider) {
  if (!confirm(t("cli.providers.removeConfirm", { name: provider.name, slug: provider.slug }))) return
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

function modelsCell(provider: CliProvider): string {
  if (!provider.models_updated_at && provider.models.length === 0) return t("cli.providers.noModels")
  return t("cli.providers.model.count", { count: provider.models.length })
}
</script>

<template>
  <div class="page">
    <PageHeader :title="t('cli.title')">
      <template #actions>
        <AppButton
          icon-only
          variant="ghost"
          :label="t('action.refresh')"
          :loading="state.refreshing"
          @click="load({ refresh: true })"
        >
          <template #icon><ActionIcon name="refresh" /></template>
        </AppButton>
      </template>
    </PageHeader>

    <Banner v-if="error || state.error" tone="error" class="page-alert">
      {{ error || t("cli.error.load") }}
      <template #actions>
        <AppButton size="sm" variant="ghost" @click="((error = null), (state.error = null))">
          {{ t("action.dismiss") }}
        </AppButton>
      </template>
    </Banner>

    <!-- Page-level empty state: the install path is the content. -->
    <AppCard v-if="isEmpty">
      <EmptyState :title="t('cli.empty.title')" :body="t('cli.empty.body')">
        <template #action>
          <div class="install">
            <span class="install-label">{{ t("cli.empty.install") }}</span>
            <CopyField :value="INSTALL_BREW" @error="error = $event" />
            <CopyField :value="INSTALL_SCRIPT" @error="error = $event" />
            <span class="install-label">{{ t("cli.empty.thenRun") }}</span>
            <CopyField :value="INIT_COMMAND" @error="error = $event" />
            <AppButton variant="ghost" size="sm" :href="RELEASES_URL">
              {{ t("cli.empty.releases") }}
            </AppButton>
          </div>
        </template>
      </EmptyState>
    </AppCard>

    <template v-else>
      <!-- --- Devices ------------------------------------------------------ -->
      <AppCard flush :title="t('cli.devices.title')" class="section">
        <template #actions>
          <!-- One gate for the whole section. aria-pressed: the same control
               opens and closes it, so its state must be audible as one. -->
          <AppButton
            v-if="devices.length"
            icon-only
            size="sm"
            variant="ghost"
            :label="
              editingDevices
                ? t('providers.section.doneEditing', { section: t('cli.devices.title') })
                : t('providers.section.edit', { section: t('cli.devices.title') })
            "
            :aria-pressed="editingDevices"
            @click="editingDevices = !editingDevices"
          >
            <template #icon><ActionIcon :name="editingDevices ? 'check' : 'edit'" /></template>
          </AppButton>
        </template>

        <EmptyState
          v-if="!state.loading && !devices.length"
          compact
          :title="t('cli.empty.title')"
          :body="t('cli.empty.body')"
        />
        <DataTable
          v-else-if="devices.length"
          :columns="deviceColumns"
          :rows="devices"
          :row-key="(d: CliDevice) => d.id"
          :caption="t('cli.devices.title')"
        >
          <template #cell-name="{ row }">
            <span class="name">{{ row.name }}</span>
            <Badge v-if="row.revoked_at" tone="danger">{{ t("cli.devices.revoked") }}</Badge>
          </template>
          <template #cell-lastSeen="{ row }">
            <span v-if="row.last_seen_at" :title="format.dateTime(row.last_seen_at)">
              {{ format.relative(row.last_seen_at) }}
            </span>
            <span v-else class="never">{{ t("state.never") }}</span>
          </template>
          <template #cell-created="{ row }">
            <span :title="format.dateTime(row.created_at)">{{ format.date(row.created_at) }}</span>
          </template>
          <template #cell-actions="{ row }">
            <!-- Revoke keeps its word: it destroys a sign-in the operator
                 would have to redo on the machine itself. A revoked row has
                 nothing left to act on. -->
            <AppButton
              v-if="!row.revoked_at"
              size="sm"
              variant="danger"
              :label="t('cli.devices.revokeName', { name: row.name })"
              :loading="busyId === row.id"
              @click="onRevoke(row)"
            >
              {{ t("cli.devices.revoke") }}
            </AppButton>
          </template>
        </DataTable>
      </AppCard>

      <!-- --- CLI providers ------------------------------------------------ -->
      <AppCard flush :title="t('cli.providers.title')" class="section">
        <template #actions>
          <AppButton
            v-if="providers.length"
            icon-only
            size="sm"
            variant="ghost"
            :label="
              editingProviders
                ? t('providers.section.doneEditing', { section: t('cli.providers.title') })
                : t('providers.section.edit', { section: t('cli.providers.title') })
            "
            :aria-pressed="editingProviders"
            @click="editingProviders = !editingProviders"
          >
            <template #icon><ActionIcon :name="editingProviders ? 'check' : 'edit'" /></template>
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
          :columns="providerColumns"
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
                size="sm"
                variant="ghost"
                :label="t('cli.providers.renameName', { name: row.name })"
                @click="openRename(row)"
              >
                <template #icon><ActionIcon name="edit" /></template>
              </AppButton>
              <AppButton
                size="sm"
                variant="danger"
                :label="t('cli.providers.removeName', { name: row.name })"
                :loading="busyId === row.id"
                @click="onRemove(row)"
              >
                {{ t("action.remove") }}
              </AppButton>
            </div>
          </template>
        </DataTable>
      </AppCard>
    </template>

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
  </div>
</template>

<style scoped>
.page {
  display: flex;
  flex-direction: column;
  gap: var(--space-5);
}

.page-alert {
  flex-shrink: 0;
}

.section :deep(.card-title) {
  font-size: var(--text-sm);
}

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

.install {
  display: grid;
  gap: var(--space-2);
  justify-items: stretch;
  text-align: left;
  max-width: 560px;
  width: 100%;
}

.install-label {
  color: var(--muted);
  font-size: var(--text-xs);
  margin-top: var(--space-2);
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
