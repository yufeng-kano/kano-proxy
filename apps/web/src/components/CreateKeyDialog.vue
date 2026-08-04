<script setup lang="ts">
/**
 * Create / edit an API key (docs/admin-ui.md § Keys page).
 *
 * Create runs in two steps inside one dialog: the form (name + optional
 * spend limit), then — after the POST — a done step showing the plaintext in
 * an emphasized copy field. That step is the only place the plaintext ever
 * exists in the UI; the copy says so ("it won't be shown again"), and the
 * key row is already in the list behind the dialog, so closing loses only
 * the one chance to copy.
 *
 * Edit is the same form without key material: rename + limit fields via
 * PATCH. Editing never re-shows a key.
 */
import { computed, nextTick, reactive, ref } from "vue"
import AppButton from "@/components/ui/AppButton.vue"
import Banner from "@/components/ui/Banner.vue"
import CopyField from "@/components/ui/CopyField.vue"
import FormField from "@/components/ui/FormField.vue"
import Modal from "@/components/ui/Modal.vue"
import TextInput from "@/components/ui/TextInput.vue"
import { useI18n } from "@/i18n"
import type { MessageKey } from "@/i18n"
import { createKey, updateKey, type KeyLimitFields } from "@/services/api"
import {
  SPEND_LIMIT_INTERVALS,
  type ApiKey,
  type CreatedKey,
  type SpendLimitInterval,
} from "@/types"

const props = defineProps<{
  /** Present = edit mode; absent = create. */
  editing?: ApiKey | null
}>()

const emit = defineEmits<{
  close: []
  /** Fired on successful create/save, before the done step — the list refreshes behind the dialog. */
  saved: []
}>()

const { t, format } = useI18n()

const isEdit = computed(() => !!props.editing)

const form = reactive({
  name: props.editing?.name ?? "",
  /** Text, not number: an empty field is "unlimited", which a number input can't say. */
  limit: props.editing?.spend_limit != null ? String(props.editing.spend_limit) : "",
  interval: (props.editing?.spend_limit_interval ?? "monthly") as SpendLimitInterval,
  includeOauth: props.editing?.spend_limit_include_oauth ?? true,
})

const saving = ref(false)
const error = ref<string | null>(null)
const fieldError = ref<string | null>(null)
/** Create's done step — set only when the POST returned the plaintext. */
const created = ref<CreatedKey | null>(null)
/**
 * The done step replaces the footer the focused Create button lived in, which
 * would otherwise dump focus on <body> mid-dialog — so it moves to Done.
 */
const doneButton = ref<InstanceType<typeof AppButton> | null>(null)

/**
 * An explicit map rather than a template literal: `` `keys.dialog.interval.${v}` ``
 * widens to `string` and would not typecheck against `MessageKey`, so a
 * renamed key has to fail the build here.
 */
const INTERVAL_KEY: Record<SpendLimitInterval, MessageKey> = {
  daily: "keys.dialog.interval.daily",
  weekly: "keys.dialog.interval.weekly",
  monthly: "keys.dialog.interval.monthly",
  total: "keys.dialog.interval.total",
}

const intervalOptions = computed(() =>
  SPEND_LIMIT_INTERVALS.map((value) => ({ value, label: t(INTERVAL_KEY[value]) })),
)

/** null = unlimited; NaN/negative = a field error, not a silent clamp. */
function parsedLimit(): number | null | "invalid" {
  const raw = form.limit.trim()
  if (!raw) return null
  const n = Number(raw)
  if (!Number.isFinite(n) || n <= 0) return "invalid"
  return n
}

function limitFields(): KeyLimitFields | "invalid" {
  const limit = parsedLimit()
  if (limit === "invalid") return "invalid"
  return {
    spend_limit: limit,
    spend_limit_interval: form.interval,
    spend_limit_include_oauth: form.includeOauth,
  }
}

async function submit() {
  if (saving.value) return
  fieldError.value = null
  error.value = null
  const limits = limitFields()
  if (limits === "invalid") {
    fieldError.value = t("keys.dialog.limitInvalid")
    return
  }
  saving.value = true
  try {
    if (props.editing) {
      await updateKey(props.editing.id, { name: form.name.trim() || props.editing.name, ...limits })
      emit("saved")
      emit("close")
    } else {
      created.value = await createKey(form.name, limits)
      emit("saved")
      await nextTick()
      doneButton.value?.$el?.focus?.()
    }
  } catch {
    error.value = isEdit.value ? t("keys.dialog.saveFailed") : t("keys.error.create")
  } finally {
    saving.value = false
  }
}

const title = computed(() => {
  if (created.value) return t("keys.created.title")
  return isEdit.value ? t("keys.dialog.editTitle") : t("keys.create")
})
</script>

<template>
  <Modal :title="title" size="sm" @close="emit('close')">
    <!-- --- Done step: the one moment the plaintext exists in the UI -------- -->
    <div v-if="created" class="done">
      <p class="done-note">{{ t("keys.created.body") }}</p>
      <CopyField emphasis :value="created.key" @error="error = $event" />
      <Banner v-if="error" tone="error">{{ error }}</Banner>
    </div>

    <!-- --- Form step ------------------------------------------------------- -->
    <div v-else class="body">
      <FormField v-slot="field" :label="t('keys.nameLabel')">
        <TextInput
          :id="field.id"
          v-model="form.name"
          :placeholder="t('keys.namePlaceholder')"
          :described-by="field.describedBy"
          :disabled="saving"
          @enter="submit"
        />
      </FormField>

      <FormField
        v-slot="field"
        :label="t('keys.dialog.limitLabel')"
        :optional-text="t('keys.dialog.limitOptional')"
        :hint="t('keys.dialog.limitHint')"
        :error="fieldError ?? undefined"
      >
        <div class="limit-row">
          <span class="currency" aria-hidden="true">$</span>
          <TextInput
            :id="field.id"
            v-model="form.limit"
            inputmode="numeric"
            :placeholder="t('keys.dialog.limitPlaceholder')"
            :described-by="field.describedBy"
            :invalid="field.invalid"
            :disabled="saving"
            @enter="submit"
          />
        </div>
      </FormField>

      <FormField v-slot="field" :label="t('keys.dialog.intervalLabel')">
        <select
          :id="field.id"
          v-model="form.interval"
          class="select"
          :aria-describedby="field.describedBy"
          :disabled="saving"
        >
          <option v-for="opt in intervalOptions" :key="opt.value" :value="opt.value">
            {{ opt.label }}
          </option>
        </select>
      </FormField>

      <label class="check">
        <input v-model="form.includeOauth" type="checkbox" :disabled="saving" />
        <span class="check-text">
          <span class="check-title">{{ t("keys.dialog.includeOauth") }}</span>
          <span class="check-hint">{{ t("keys.dialog.includeOauthHint") }}</span>
        </span>
      </label>

      <!-- Edit shows the window's spend so "how close is this key" is
           answerable while adjusting the ceiling. -->
      <p v-if="editing && editing.window_spend != null" class="spend-note">
        {{ t("keys.dialog.currentSpend", { amount: format.currency(editing.window_spend) }) }}
      </p>

      <Banner v-if="error" tone="error">{{ error }}</Banner>
    </div>

    <template #footer>
      <template v-if="created">
        <AppButton ref="doneButton" variant="primary" @click="emit('close')">
          {{ t("action.done") }}
        </AppButton>
      </template>
      <template v-else>
        <AppButton variant="ghost" :disabled="saving" @click="emit('close')">
          {{ t("action.cancel") }}
        </AppButton>
        <AppButton variant="primary" :loading="saving" @click="submit">
          {{ isEdit ? t("keys.dialog.save") : t("keys.create") }}
        </AppButton>
      </template>
    </template>
  </Modal>
</template>

<style scoped>
.body,
.done {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
}

.done-note {
  margin: 0;
  color: var(--text-secondary);
  font-size: var(--text-sm);
}

.limit-row {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  width: 100%;
}

.currency {
  color: var(--muted);
  font-size: var(--text-sm);
}

/* Styled to TextInput's control spec — the one native select in the app. */
.select {
  width: 100%;
  min-width: 0;
  height: 34px;
  padding: 0 var(--space-3);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-sm);
  background: var(--surface);
  color: var(--text);
  font-size: var(--text-sm);
}

.select:focus {
  border-color: var(--ring-border);
  box-shadow: var(--ring);
  outline: none;
}

.select:disabled {
  background: var(--surface-2);
  color: var(--muted);
  cursor: not-allowed;
}

/* Same row shape as CustomProviderDialog's mode radios, minus the border —
   one checkbox does not need a card. */
.check {
  display: flex;
  align-items: flex-start;
  gap: var(--space-3);
  cursor: pointer;
}

.check input {
  flex-shrink: 0;
  margin: 3px 0 0;
  accent-color: var(--accent);
}

.check-text {
  display: grid;
  gap: var(--space-1);
  min-width: 0;
}

.check-title {
  font-size: var(--text-sm);
  font-weight: var(--weight-medium);
}

.check-hint {
  color: var(--muted);
  font-size: var(--text-xs);
}

.spend-note {
  margin: 0;
  color: var(--muted);
  font-size: var(--text-xs);
}

@media (pointer: coarse) {
  .select {
    height: 40px;
    font-size: var(--text-md);
  }
}
</style>
