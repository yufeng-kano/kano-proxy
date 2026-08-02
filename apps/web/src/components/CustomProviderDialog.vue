<script setup lang="ts">
import { computed, reactive, ref } from "vue"
import { createCustomProvider, testCustomProvider, updateCustomProvider } from "@/services/api"
import type {
  CustomProvider,
  CustomProviderFormat,
  CustomProviderModelsMode,
  CustomProviderTestResult,
} from "@/types"

const props = defineProps<{
  /** null/omitted = create mode. A provider = edit mode, prefilled from it. */
  provider?: CustomProvider | null
}>()

const emit = defineEmits<{ close: []; saved: [] }>()

const SLUG_RE = /^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$/

const isEdit = computed(() => !!props.provider)

const form = reactive({
  name: props.provider?.name ?? "",
  slug: props.provider?.slug ?? "",
  format: (props.provider?.format ?? "openai") as CustomProviderFormat,
  base_url: props.provider?.base_url ?? "",
  api_key: "",
  models_mode: (props.provider?.models_mode ?? "auto") as CustomProviderModelsMode,
})
const manualModelsText = ref(props.provider?.manual_models?.join("\n") ?? "")

const slugTouched = ref(false)
const saving = ref(false)
const testing = ref(false)
const error = ref<string | null>(null)
const testResult = ref<CustomProviderTestResult | null>(null)

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

function onNameInput() {
  if (isEdit.value || slugTouched.value) return
  form.slug = slugify(form.name)
}

function onSlugInput() {
  slugTouched.value = true
  form.slug = form.slug.toLowerCase()
}

const baseUrlPlaceholder = computed(() =>
  form.format === "anthropic" ? "https://api.example.com" : "https://api.example.com/v1",
)

const resolvedEndpointPreview = computed(() => {
  const raw = form.base_url.trim().replace(/\/+$/, "")
  const base = raw || baseUrlPlaceholder.value
  return form.format === "anthropic" ? `${base}/v1/messages` : `${base}/chat/completions`
})

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

function setFormat(f: CustomProviderFormat) {
  if (isEdit.value) return
  form.format = f
}

function validate(): string | null {
  if (!form.name.trim()) return "Name is required."
  if (!isEdit.value) {
    const slug = form.slug.trim()
    if (!slug) return "Slug is required."
    if (!SLUG_RE.test(slug)) {
      return "Slug must be lowercase letters, numbers, and hyphens, starting and ending with a letter or digit."
    }
  }
  const baseUrl = form.base_url.trim()
  if (!baseUrl) return "Base URL is required."
  try {
    if (new URL(baseUrl).protocol !== "https:") return "Base URL must use https."
  } catch {
    return "Base URL must be a valid URL."
  }
  if (!isEdit.value && !form.api_key.trim()) return "API key is required."
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
  } catch (e) {
    error.value = e instanceof Error ? e.message : "Test failed"
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
      if (form.api_key.trim()) body.api_key = form.api_key
      await updateCustomProvider(props.provider.id, body)
    } else {
      await createCustomProvider({
        name: form.name.trim(),
        slug: form.slug.trim(),
        format: form.format,
        base_url: form.base_url.trim(),
        api_key: form.api_key,
        models_mode: form.models_mode,
        manual_models: form.models_mode === "manual" ? manualModelsList.value : undefined,
      })
    }
    emit("saved")
    emit("close")
  } catch (e) {
    error.value = e instanceof Error ? e.message : "Save failed"
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <div class="overlay" @click.self="emit('close')">
    <div class="dialog cp-dialog" role="dialog" aria-modal="true">
      <h2>{{ isEdit ? "Edit endpoint" : "Add endpoint" }}</h2>

      <div class="stack">
        <div class="field">
          <label id="cp-format-label">Format</label>
          <div class="cp-segmented" role="radiogroup" aria-labelledby="cp-format-label">
            <button
              type="button"
              class="cp-segmented-option"
              :class="{ active: form.format === 'openai' }"
              :disabled="isEdit"
              role="radio"
              :aria-checked="form.format === 'openai'"
              @click="setFormat('openai')"
            >
              OpenAI Chat Completions
            </button>
            <button
              type="button"
              class="cp-segmented-option"
              :class="{ active: form.format === 'anthropic' }"
              :disabled="isEdit"
              role="radio"
              :aria-checked="form.format === 'anthropic'"
              @click="setFormat('anthropic')"
            >
              Anthropic Messages
            </button>
          </div>
          <p v-if="isEdit" class="faint cp-hint">Locked after creation.</p>
        </div>

        <div class="field">
          <label for="cp-name">Name</label>
          <input
            id="cp-name"
            v-model="form.name"
            class="input"
            autocomplete="off"
            placeholder="My endpoint"
            @input="onNameInput"
          />
        </div>

        <div class="field">
          <label for="cp-slug">Slug</label>
          <input
            id="cp-slug"
            v-model="form.slug"
            class="input mono"
            autocomplete="off"
            placeholder="my-endpoint"
            :disabled="isEdit"
            @input="onSlugInput"
          />
          <p class="faint cp-hint">
            Model id preview: <code class="mono">{{ form.slug || "slug" }}/&lt;model&gt;</code>
          </p>
          <p v-if="isEdit" class="faint cp-hint">Locked after creation.</p>
        </div>

        <div class="field">
          <label for="cp-base-url">Base URL</label>
          <input
            id="cp-base-url"
            v-model="form.base_url"
            class="input mono"
            autocomplete="off"
            inputmode="url"
            :placeholder="baseUrlPlaceholder"
          />
          <p class="faint cp-hint">
            Resolves to <code class="mono">{{ resolvedEndpointPreview }}</code>
          </p>
        </div>

        <div class="field">
          <label for="cp-api-key">API key</label>
          <input
            id="cp-api-key"
            v-model="form.api_key"
            type="password"
            class="input"
            autocomplete="off"
            :placeholder="isEdit ? 'Leave blank to keep current key' : 'sk-…'"
          />
          <p v-if="isEdit" class="faint cp-hint">
            Stored keys are never shown. Leave blank to keep the current key.
          </p>
        </div>

        <div class="field">
          <label id="cp-models-mode-label">Models</label>
          <div class="cp-radio-group" role="radiogroup" aria-labelledby="cp-models-mode-label">
            <label class="cp-radio-option">
              <input v-model="form.models_mode" type="radio" name="cp-models-mode" value="auto" />
              <span>Auto — fetch from the endpoint's models list</span>
            </label>
            <label class="cp-radio-option">
              <input v-model="form.models_mode" type="radio" name="cp-models-mode" value="manual" />
              <span>Manual — list model ids yourself</span>
            </label>
          </div>
        </div>

        <div v-if="form.models_mode === 'manual'" class="field">
          <label for="cp-manual-models">Model ids (one per line)</label>
          <textarea
            id="cp-manual-models"
            v-model="manualModelsText"
            class="input textarea mono"
            :placeholder="'gpt-4o\nclaude-3-7-sonnet'"
          />
        </div>

        <div class="stack" style="gap: 8px">
          <button
            type="button"
            class="btn btn-secondary"
            :disabled="testing || saving || !canTest"
            @click="runTest"
          >
            {{ testing ? "Testing…" : "Test connection" }}
          </button>

          <div v-if="testResult" class="banner" :class="testResult.ok ? 'ok' : 'error'">
            <template v-if="testResult.ok">
              Connection OK<span v-if="testResult.models_count != null">
                — {{ testResult.models_count }} model{{ testResult.models_count === 1 ? "" : "s" }}</span
              >.
            </template>
            <template v-else>{{ testResult.error || "Connection failed." }}</template>
            <div v-if="testResult.sample?.length" class="faint" style="margin-top: 4px">
              Sample: <code class="mono">{{ testResult.sample.join(", ") }}</code>
            </div>
            <div v-if="testResult.note" class="faint" style="margin-top: 4px">{{ testResult.note }}</div>
          </div>
        </div>
      </div>

      <div v-if="error" class="banner error">{{ error }}</div>

      <div class="dialog-actions">
        <button type="button" class="btn btn-ghost" @click="emit('close')">Cancel</button>
        <button type="button" class="btn" :disabled="saving || testing" @click="submit">
          {{ saving ? "Saving…" : isEdit ? "Save changes" : "Add endpoint" }}
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.cp-dialog {
  max-height: min(640px, calc(100vh - 40px));
  overflow-y: auto;
}

.cp-hint {
  margin: 0;
  font-size: 11.5px;
}

.cp-segmented {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 6px;
}

.cp-segmented-option {
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-sm);
  background: var(--surface);
  color: var(--text-secondary);
  padding: 8px 10px;
  font-size: 12.5px;
  font-weight: 500;
  cursor: pointer;
  text-align: center;
}

.cp-segmented-option:hover:not(:disabled) {
  background: var(--surface-2);
}

.cp-segmented-option.active {
  background: var(--accent);
  border-color: var(--accent);
  color: var(--accent-fg);
}

.cp-segmented-option:disabled {
  cursor: not-allowed;
  opacity: 0.7;
}

.cp-radio-group {
  display: grid;
  gap: 8px;
}

.cp-radio-option {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 13px;
  cursor: pointer;
}

.cp-radio-option input {
  flex-shrink: 0;
}
</style>
