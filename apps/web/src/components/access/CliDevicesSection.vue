<script setup lang="ts">
/**
 * The Access page's Devices tab (docs/admin-ui.md § Access page): the CLI
 * devices card. A device is a credential — a rotating token family that can
 * mint API keys — so it lives beside the keys it mints, where both are
 * revoked. No create flow: signing in is the CLI's job, so the empty state
 * teaches the install + `kano-proxy init` path instead. Row actions sit
 * behind the card's edit gate, the convention every section shares.
 *
 * Data comes from the shared useCli singleton; the page owns loading, this
 * component only renders and mutates.
 */
import { computed, ref } from "vue"
import ActionIcon from "@/components/ui/ActionIcon.vue"
import AppButton from "@/components/ui/AppButton.vue"
import AppCard from "@/components/ui/AppCard.vue"
import Banner from "@/components/ui/Banner.vue"
import CopyField from "@/components/ui/CopyField.vue"
import DataTable from "@/components/ui/DataTable.vue"
import type { Column } from "@/components/ui/DataTable.vue"
import EmptyState from "@/components/ui/EmptyState.vue"
import FormField from "@/components/ui/FormField.vue"
import Modal from "@/components/ui/Modal.vue"
import TextInput from "@/components/ui/TextInput.vue"
import { useCli } from "@/composables/useCli"
import { useI18n } from "@/i18n"
import { renameCliDevice, revokeCliDevice } from "@/services/api"
import type { CliDevice } from "@/types"

/**
 * Wire identifiers, not copy (docs/i18n.md): the install commands and the
 * Releases URL are the same for every instance — instances never host
 * binaries themselves (docs/cli.md § Distribution). The init command carries
 * this instance's own origin: the CLI's compiled-in default is the hosted
 * edition, so a self-hosted instance has to teach its address here.
 */
const INSTALL_BREW = "brew install yufeng-kano/tap/kano-proxy"
const INSTALL_SCRIPT =
  "curl -fsSL https://raw.githubusercontent.com/yufeng-kano/kano-proxy/main/scripts/install-cli.sh | sh"
const INIT_COMMAND = `kano-proxy init --base-url ${window.location.origin}`
const RELEASES_URL = "https://github.com/yufeng-kano/kano-proxy/releases"

const emit = defineEmits<{ (e: "changed"): void }>()

const { t, format } = useI18n()
const { state, load } = useCli()

const error = ref<string | null>(null)
const editing = ref(false)
const busyId = ref<string | null>(null)

const renaming = ref<CliDevice | null>(null)
const renameValue = ref("")
const renameSaving = ref(false)

const devices = computed<CliDevice[]>(() => state.devices ?? [])
const isEmpty = computed(() => !state.loading && devices.value.length === 0)

const columns = computed<Column<CliDevice>[]>(() => [
  { key: "name", header: t("cli.devices.column.name") },
  { key: "lastSeen", header: t("cli.devices.column.lastSeen") },
  { key: "created", header: t("cli.devices.column.created"), hideOnMobile: true },
  { key: "keys", header: t("cli.devices.column.keys"), numeric: true },
  ...(editing.value
    ? [{ key: "actions", header: "", srHeader: t("action.edit"), align: "end" as const, width: "148px" }]
    : []),
])

async function onRevoke(device: CliDevice) {
  // Revoking takes the keys the device minted along (docs/cli.md § Keys and
  // groups) — the one fact the user must have before pressing it.
  const message =
    device.key_count > 0
      ? t("cli.devices.revokeConfirmKeys", { name: device.name, count: device.key_count })
      : t("cli.devices.revokeConfirm", { name: device.name })
  if (!confirm(message)) return
  busyId.value = device.id
  error.value = null
  try {
    await revokeCliDevice(device.id)
    await load({ refresh: true })
    emit("changed")
  } catch {
    error.value = t("cli.error.revoke")
  } finally {
    busyId.value = null
  }
}

function openRename(row: CliDevice) {
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
    await renameCliDevice(target.id, name)
    renaming.value = null
    await load({ refresh: true })
    emit("changed")
  } catch {
    error.value = t("cli.error.renameDevice")
  } finally {
    renameSaving.value = false
  }
}
</script>

<template>
  <Banner v-if="error || state.error" tone="error">
    {{ error || t("cli.error.load") }}
    <template #actions>
      <AppButton size="sm" variant="ghost" @click="((error = null), (state.error = null))">
        {{ t("action.dismiss") }}
      </AppButton>
    </template>
  </Banner>

  <AppCard fill :flush="!isEmpty" class="card" :title="t('cli.devices.title')">
    <template #actions>
      <!-- One gate for the whole section. aria-pressed: the same control
           opens and closes it, so its state must be audible as one. -->
      <AppButton
        v-if="devices.length"
        icon-only
        size="sm"
        variant="ghost"
        :label="
          editing
            ? t('providers.section.doneEditing', { section: t('cli.devices.title') })
            : t('providers.section.edit', { section: t('cli.devices.title') })
        "
        :aria-pressed="editing"
        @click="editing = !editing"
      >
        <template #icon><ActionIcon :name="editing ? 'check' : 'edit'" /></template>
      </AppButton>
    </template>

    <!-- Empty state: the install path is the content. -->
    <EmptyState v-if="isEmpty" :title="t('cli.empty.title')" :body="t('cli.empty.body')">
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

    <DataTable
      v-else-if="devices.length"
      :columns="columns"
      :rows="devices"
      :row-key="(d: CliDevice) => d.id"
      :caption="t('cli.devices.title')"
    >
      <template #cell-name="{ row }">
        <span class="name">{{ row.name }}</span>
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
      <template #cell-keys="{ row }">
        <span class="tabular">{{ row.key_count }}</span>
      </template>
      <template #cell-actions="{ row }">
        <!-- Revoke keeps its word: it destroys a sign-in the operator would
             have to redo on the machine itself, and the row goes with it. -->
        <div class="row-actions">
          <AppButton
            icon-only
            size="sm"
            variant="ghost"
            :label="t('cli.devices.renameName', { name: row.name })"
            @click="openRename(row)"
          >
            <template #icon><ActionIcon name="edit" /></template>
          </AppButton>
          <AppButton
            size="sm"
            variant="danger"
            :label="t('cli.devices.revokeName', { name: row.name })"
            :loading="busyId === row.id"
            @click="onRevoke(row)"
          >
            {{ t("cli.devices.revoke") }}
          </AppButton>
        </div>
      </template>
    </DataTable>
  </AppCard>

  <Modal v-if="renaming" :title="t('cli.rename.deviceTitle')" @close="renaming = null">
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
.card {
  flex: 1;
  min-height: 0;
}

.name {
  font-weight: var(--weight-medium);
  margin-right: var(--space-2);
}

.never {
  color: var(--faint);
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
