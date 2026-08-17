<script setup lang="ts">
/**
 * Add or edit a user-defined OpenAI-/Anthropic-compatible endpoint.
 *
 * Two things here are contracts, not styling: `format` and `slug` are
 * immutable once saved (server-side too), and a blank API key on edit means
 * "keep the stored one" — the key is never pre-filled or echoed back
 * (docs/admin-ui.md § Providers page).
 *
 * The base-URL hint is a live preview of the endpoint the request will
 * actually reach, matching providers.md's literal-concatenation rule, so a
 * missing or doubled `/v1` is visible before saving rather than after a failed
 * call. The token-count URL deliberately has no such preview: it is a complete
 * URL posted to verbatim, not a base (docs/providers.md § Custom endpoints).
 */
import { computed, reactive, ref } from "vue"
import { useI18n } from "@/i18n"
import { createCustomProvider, testCustomProvider, updateCustomProvider } from "@/services/api"
import type {
  CustomProvider,
  CustomProviderFormat,
  CustomProviderModelsMode,
  CustomProviderTestResult,
} from "@/types"
import AppButton from "./ui/AppButton.vue"
import Badge from "./ui/Badge.vue"
import Banner from "./ui/Banner.vue"
import FormField from "./ui/FormField.vue"
import Modal from "./ui/Modal.vue"
import Segmented from "./ui/Segmented.vue"
import TextInput from "./ui/TextInput.vue"

const props = defineProps<{
  /** null/omitted = create mode. A provider = edit mode, prefilled from it. */
  provider?: CustomProvider | null
}>()

const emit = defineEmits<{ close: []; saved: [] }>()

const { t } = useI18n()

const SLUG_RE = /^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$/

const isEdit = computed(() => !!props.provider)

const form = reactive({
  name: props.provider?.name ?? "",
  slug: props.provider?.slug ?? "",
  format: (props.provider?.format ?? "openai") as CustomProviderFormat,
  base_url: props.provider?.base_url ?? "",
  api_key: "",
  // Not a secret, so unlike the key it is pre-filled on edit and emptying it
  // is how the user turns the feature back off (docs/auth.md).
  count_tokens_url: props.provider?.count_tokens_url ?? "",
  models_mode: (props.provider?.models_mode ?? "auto") as CustomProviderModelsMode,
})
const manualModelsText = ref(props.provider?.manual_models?.join("\n") ?? "")

const slugTouched = ref(false)
const saving = ref(false)
const testing = ref(false)
const error = ref<string | null>(null)
const testResult = ref<CustomProviderTestResult | null>(null)

const formatOptions = computed(() => [
  { value: "openai", label: t("custom.dialog.formatOpenAI") },
  { value: "anthropic", label: t("custom.dialog.formatAnthropic") },
])

const formatLabel = computed(() =>
  form.format === "anthropic"
    ? t("custom.dialog.formatAnthropic")
    : t("custom.dialog.formatOpenAI"),
)

function slugify(input: string): string {
  // A single pass already collapses any run of invalid chars (including "-"
  // itself, which [^a-z0-9] also matches) into one hyphen, so no second
  // collapse pass is needed. Re-trim after slice(): truncation can expose a
  // trailing hyphen that was safely interior before the cut.
  return input
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 32)
    .replace(/-+$/, "")
}

function onNameInput(value: string) {
  form.name = value
  if (isEdit.value || slugTouched.value) return
  form.slug = slugify(value)
}

function onSlugInput(value: string) {
  slugTouched.value = true
  form.slug = value.toLowerCase()
}

function onFormatChange(value: string | number) {
  if (isEdit.value) return
  form.format = value === "anthropic" ? "anthropic" : "openai"
}

/**
 * A URL, not copy — `example.com` is the reserved documentation domain and the
 * `/v1` suffix is the OpenAI wire path, so neither is translated. It doubles as
 * the stand-in the endpoint preview shows before the user types anything.
 */
const baseUrlPlaceholder = computed(() =>
  form.format === "anthropic" ? "https://api.example.com" : "https://api.example.com/v1",
)

const resolvedEndpointPreview = computed(() => {
  const raw = form.base_url.trim().replace(/\/+$/, "")
  const base = raw || baseUrlPlaceholder.value
  return form.format === "anthropic" ? `${base}/v1/messages` : `${base}/chat/completions`
})

/**
 * The `slug/*` model-prefix form the card and docs use. The wildcard is a
 * symbol rather than a translatable word, so the preview needs no second key.
 */
const slugPreview = computed(
  () => `${form.slug || t("custom.dialog.slugPlaceholder")}/*`,
)

/**
 * Only the OpenAI format has a token-count gap to fill — an anthropic-format
 * endpoint derives that path from its own base, and the server rejects the
 * field there. `format` is immutable once saved, so on edit this is simply the
 * saved format.
 */
const showCountTokensUrl = computed(() => form.format === "openai")

const manualModelsList = computed(() =>
  manualModelsText.value
    .split(/\r?\n/)
    .map((s) => s.trim())
    .filter(Boolean),
)

const canTest = computed(() => {
  if (!form.base_url.trim()) return false
  if (!isEdit.value && !form.api_key.trim()) return false
  return true
})

/**
 * Headline of a finished test: the count when the endpoint reported one. A
 * null `models_count` means the endpoint answered without a list, which is not
 * the same as having none, so it falls back to the uncounted form.
 */
const testHeadline = computed(() => {
  const result = testResult.value
  if (!result) return null
  if (!result.ok) return result.error || t("custom.test.failed")
  return result.models_count != null
    ? t("custom.test.okModels", { count: result.models_count })
    : t("custom.test.ok")
})

function validate(): string | null {
  if (!form.name.trim()) return t("custom.error.name")
  if (!isEdit.value) {
    const slug = form.slug.trim()
    if (!slug) return t("custom.error.slug")
    if (!SLUG_RE.test(slug)) return t("custom.error.slugFormat")
  }
  const baseUrl = form.base_url.trim()
  if (!baseUrl) return t("custom.error.baseUrl")
  try {
    if (new URL(baseUrl).protocol !== "https:") return t("custom.error.baseUrlHttps")
  } catch {
    return t("custom.error.baseUrlInvalid")
  }
  if (!isEdit.value && !form.api_key.trim()) return t("custom.error.apiKey")
  // Optional, and only ever sent for the OpenAI format — but when it is filled
  // in it gets the same shape check as the base URL. The server is still the
  // authority (host and scheme rules live there).
  const countTokensUrl = form.count_tokens_url.trim()
  if (showCountTokensUrl.value && countTokensUrl) {
    try {
      if (new URL(countTokensUrl).protocol !== "https:") {
        return t("custom.error.countTokensUrlHttps")
      }
    } catch {
      return t("custom.error.countTokensUrlInvalid")
    }
  }
  return null
}

async function runTest() {
  testing.value = true
  testResult.value = null
  error.value = null
  try {
    if (form.api_key.trim()) {
      testResult.value = await testCustomProvider({
        format: form.format,
        base_url: form.base_url.trim(),
        api_key: form.api_key,
      })
    } else if (isEdit.value && props.provider) {
      testResult.value = await testCustomProvider({
        id: props.provider.id,
        base_url: form.base_url.trim() || undefined,
      })
    }
  } catch {
    error.value = t("custom.test.failed")
  } finally {
    testing.value = false
  }
}

async function submit() {
  error.value = null
  const validationError = validate()
  if (validationError) {
    error.value = validationError
    return
  }

  saving.value = true
  try {
    if (isEdit.value && props.provider) {
      const body: Parameters<typeof updateCustomProvider>[1] = {
        name: form.name.trim(),
        base_url: form.base_url.trim(),
        models_mode: form.models_mode,
        manual_models: form.models_mode === "manual" ? manualModelsList.value : undefined,
      }
      // Blank means keep: only a typed key is ever sent.
      if (form.api_key.trim()) body.api_key = form.api_key
      // The opposite convention, on purpose: this one is readable data, so the
      // field is always sent and an empty string is what clears a stored value.
      if (showCountTokensUrl.value) body.count_tokens_url = form.count_tokens_url.trim()
      await updateCustomProvider(props.provider.id, body)
    } else {
      await createCustomProvider({
        name: form.name.trim(),
        slug: form.slug.trim(),
        format: form.format,
        base_url: form.base_url.trim(),
        api_key: form.api_key,
        // Nothing to clear on create, and a value typed before the format was
        // switched to anthropic must not travel with it.
        count_tokens_url: showCountTokensUrl.value
          ? form.count_tokens_url.trim() || undefined
          : undefined,
        models_mode: form.models_mode,
        manual_models: form.models_mode === "manual" ? manualModelsList.value : undefined,
      })
    }
    emit("saved")
    emit("close")
  } catch {
    error.value = t("custom.error.save")
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <Modal
    size="md"
    :title="isEdit ? t('custom.dialog.editTitle') : t('custom.dialog.addTitle')"
    @close="emit('close')"
  >
    <div class="body">
      <div class="field">
        <span class="field-label">{{ t("custom.dialog.format") }}</span>
        <!-- Immutable once saved, so on edit there is nothing to toggle: the
             value is shown as the fact it is rather than as a control that
             looks live and refuses to move. -->
        <Segmented
          v-if="!isEdit"
          :model-value="form.format"
          :options="formatOptions"
          :label="t('custom.dialog.format')"
          @update:model-value="onFormatChange"
        />
        <template v-else>
          <Badge>{{ formatLabel }}</Badge>
          <p class="field-hint">{{ t("custom.dialog.formatLocked") }}</p>
        </template>
      </div>

      <FormField v-slot="field" :label="t('custom.dialog.name')">
        <TextInput
          :id="field.id"
          :model-value="form.name"
          :placeholder="t('custom.dialog.namePlaceholder')"
          :described-by="field.describedBy"
          @update:model-value="onNameInput"
        />
      </FormField>

      <FormField
        v-slot="field"
        :label="t('custom.dialog.slug')"
        :hint="
          isEdit
            ? t('custom.dialog.slugLocked')
            : t('custom.dialog.slugHint', { example: slugPreview })
        "
      >
        <TextInput
          :id="field.id"
          :model-value="form.slug"
          mono
          :disabled="isEdit"
          :placeholder="t('custom.dialog.slugPlaceholder')"
          :described-by="field.describedBy"
          @update:model-value="onSlugInput"
        />
      </FormField>

      <FormField
        v-slot="field"
        :label="t('custom.dialog.baseUrl')"
        :hint="t('custom.dialog.baseUrlHint', { url: resolvedEndpointPreview })"
      >
        <TextInput
          :id="field.id"
          v-model="form.base_url"
          type="url"
          mono
          inputmode="url"
          :placeholder="baseUrlPlaceholder"
          :described-by="field.describedBy"
        />
      </FormField>

      <FormField v-slot="field" :label="t('custom.dialog.apiKey')">
        <TextInput
          :id="field.id"
          v-model="form.api_key"
          type="password"
          autocomplete="new-password"
          :placeholder="
            isEdit
              ? t('custom.dialog.apiKeyPlaceholderEdit')
              : t('custom.dialog.apiKeyPlaceholderNew')
          "
          :described-by="field.describedBy"
        />
      </FormField>

      <!-- A real fieldset/legend so the group's name reaches assistive tech
           natively. The rows live in an inner grid: a legend is pulled out of
           its fieldset's flow, so it would ignore a gap set on the fieldset. -->
      <fieldset class="fieldset">
        <legend class="field-label">{{ t("custom.dialog.models") }}</legend>
        <div class="modes">
          <label class="mode">
            <input v-model="form.models_mode" type="radio" name="cp-models-mode" value="auto" />
            <span class="mode-text">
              <span class="mode-title">{{ t("custom.dialog.modelsAuto") }}</span>
              <span class="mode-hint">{{ t("custom.dialog.modelsAutoHint") }}</span>
            </span>
          </label>
          <label class="mode">
            <input v-model="form.models_mode" type="radio" name="cp-models-mode" value="manual" />
            <span class="mode-text">
              <span class="mode-title">{{ t("custom.dialog.modelsManual") }}</span>
              <span class="mode-hint">{{ t("custom.dialog.modelsManualHint") }}</span>
            </span>
          </label>
        </div>
      </fieldset>

      <FormField
        v-if="form.models_mode === 'manual'"
        v-slot="field"
        :label="t('custom.dialog.manualModels')"
        :hint="t('custom.dialog.manualModelsHint')"
      >
        <TextInput
          :id="field.id"
          v-model="manualModelsText"
          multiline
          mono
          :rows="4"
          :described-by="field.describedBy"
        />
      </FormField>

      <!-- OpenAI format only: an anthropic-format endpoint already derives this
           path from its base URL, and the server rejects the field there. -->
      <FormField
        v-if="showCountTokensUrl"
        v-slot="field"
        :label="t('custom.dialog.countTokensUrl')"
        :optional-text="t('custom.dialog.countTokensUrlOptional')"
        :hint="t('custom.dialog.countTokensUrlHint')"
      >
        <TextInput
          :id="field.id"
          v-model="form.count_tokens_url"
          type="url"
          mono
          inputmode="url"
          :placeholder="t('custom.dialog.countTokensUrlPlaceholder')"
          :described-by="field.describedBy"
        />
      </FormField>

      <div class="test">
        <AppButton :loading="testing" :disabled="saving || !canTest" @click="runTest">
          {{ t("custom.dialog.testConnection") }}
        </AppButton>

        <Banner v-if="testResult" :tone="testResult.ok ? 'ok' : 'error'">
          <div class="result">
            <span>{{ testHeadline }}</span>
            <span v-if="testResult.sample?.length" class="result-note">
              {{ t("custom.test.sample", { models: testResult.sample.join(", ") }) }}
            </span>
            <span v-if="testResult.note" class="result-note">{{ testResult.note }}</span>
          </div>
        </Banner>
      </div>

      <Banner v-if="error" tone="error">{{ error }}</Banner>
    </div>

    <template #footer>
      <AppButton variant="ghost" :disabled="saving" @click="emit('close')">
        {{ t("action.cancel") }}
      </AppButton>
      <AppButton variant="primary" :loading="saving" :disabled="testing" @click="submit">
        {{ isEdit ? t("custom.dialog.submitEdit") : t("custom.dialog.submitAdd") }}
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

/* Matches FormField's own grid so a hand-built group (the format toggle, the
   models radios) sits on the same rhythm as the fields around it. */
.field {
  display: grid;
  grid-template-columns: minmax(0, 1fr);
  gap: var(--space-2);
  justify-items: start;
}

/* A fieldset's default margin, padding, and border are all browser chrome this
   design does not use — the legend alone carries the grouping. */
.fieldset {
  margin: 0;
  padding: 0;
  border: none;
}

.field-label {
  /* A legend carries its own inline padding in every engine. */
  padding: 0;
  color: var(--text-secondary);
  font-size: var(--text-xs);
  font-weight: var(--weight-medium);
}

.field-hint {
  margin: 0;
  color: var(--muted);
  font-size: var(--text-2xs);
  line-height: 1.5;
}

.modes {
  display: grid;
  gap: var(--space-2);
  margin-top: var(--space-2);
}

.mode {
  display: flex;
  align-items: flex-start;
  gap: var(--space-3);
  padding: var(--space-3);
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  cursor: pointer;
  transition: border-color var(--duration-fast) var(--ease);
}

.mode:hover {
  border-color: var(--border-strong);
}

/* The whole row reads as selected, not just the dot — a 13px radio is a small
   target for "which mode is this endpoint in". */
.mode:has(input:checked) {
  border-color: var(--ring-border);
  background: var(--surface-2);
}

/* Nudged down to sit on the title's baseline rather than its line box top. */
.mode input {
  flex-shrink: 0;
  margin: var(--space-1) 0 0;
  accent-color: var(--accent);
}

.mode-text {
  display: grid;
  gap: var(--space-1);
  min-width: 0;
}

.mode-title {
  font-size: var(--text-sm);
  font-weight: var(--weight-medium);
}

.mode-hint {
  color: var(--muted);
  font-size: var(--text-xs);
}

.test {
  display: grid;
  gap: var(--space-3);
  justify-items: start;
  padding-top: var(--space-1);
  border-top: 1px solid var(--border);
}

.test > :deep(.banner) {
  justify-self: stretch;
}

.result {
  display: grid;
  gap: var(--space-1);
  min-width: 0;
}

.result-note {
  color: var(--muted);
  font-size: var(--text-2xs);
  overflow-wrap: anywhere;
}
</style>
