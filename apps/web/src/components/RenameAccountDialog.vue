<script setup lang="ts">
/**
 * Give a connected account your own name (docs/admin-ui.md § Providers page).
 *
 * Display-only: it writes `custom_label` and touches nothing about routing or
 * which account is primary. Prefilled from `custom_label` rather than from the
 * resolved label — seeding the field with the upstream email would leave the
 * user no way to *say* "clear it", since the blank they need to submit would
 * look like they had deleted a real name.
 *
 * The upstream identity is shown as the hint, so what a blank submit falls
 * back to is visible before saving rather than after.
 */
import { computed, ref } from "vue"
import AppButton from "@/components/ui/AppButton.vue"
import Banner from "@/components/ui/Banner.vue"
import FormField from "@/components/ui/FormField.vue"
import Modal from "@/components/ui/Modal.vue"
import TextInput from "@/components/ui/TextInput.vue"
import { useI18n } from "@/i18n"
import { renameAccount } from "@/services/api"
import type { ProviderAccount, ProviderId } from "@/types"

const props = defineProps<{
  provider: ProviderId
  account: ProviderAccount
  /** Upstream email/username — what a blank name falls back to. */
  identity: string
}>()

const emit = defineEmits<{ close: []; saved: [] }>()

const { t } = useI18n()

/** Matches the server's ceiling, so a too-long name fails here, not on save. */
const MAX_LENGTH = 64

const name = ref(props.account.custom_label ?? "")
const saving = ref(false)
const error = ref<string | null>(null)
const fieldError = ref<string | null>(null)

const trimmed = computed(() => name.value.trim())

async function submit() {
  if (saving.value) return
  fieldError.value = null
  error.value = null
  if (trimmed.value.length > MAX_LENGTH) {
    fieldError.value = t("providers.rename.tooLong")
    return
  }
  saving.value = true
  try {
    // Blank clears it: the row goes back to the upstream identity.
    await renameAccount(props.provider, props.account.id, trimmed.value || null)
    emit("saved")
    emit("close")
  } catch {
    error.value = t("providers.error.rename")
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <Modal :title="t('providers.rename.title')" size="sm" @close="emit('close')">
    <div class="body">
      <FormField
        v-slot="field"
        :label="t('providers.rename.label')"
        :hint="t('providers.rename.hint', { identity })"
        :error="fieldError ?? undefined"
      >
        <TextInput
          :id="field.id"
          v-model="name"
          :placeholder="t('providers.rename.placeholder')"
          :described-by="field.describedBy"
          :invalid="field.invalid"
          :disabled="saving"
          @enter="submit"
        />
      </FormField>

      <Banner v-if="error" tone="error">{{ error }}</Banner>
    </div>

    <template #footer>
      <AppButton variant="ghost" :disabled="saving" @click="emit('close')">
        {{ t("action.cancel") }}
      </AppButton>
      <AppButton variant="primary" :loading="saving" @click="submit">
        {{ t("providers.rename.save") }}
      </AppButton>
    </template>
  </Modal>
</template>

<style scoped>
.body {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
}
</style>
