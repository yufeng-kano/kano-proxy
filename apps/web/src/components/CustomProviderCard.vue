<script setup lang="ts">
import { computed, ref } from "vue"
import { testCustomProvider } from "@/services/api"
import type { CustomProvider, CustomProviderTestResult } from "@/types"

const props = defineProps<{
  provider: CustomProvider
  busy?: boolean
}>()

const emit = defineEmits<{
  edit: []
  remove: []
}>()

const testing = ref(false)
const testResult = ref<CustomProviderTestResult | null>(null)
const testError = ref<string | null>(null)

const formatLabel = computed(() => (props.provider.format === "anthropic" ? "Anthropic" : "OpenAI"))

async function runTest() {
  testing.value = true
  testResult.value = null
  testError.value = null
  try {
    testResult.value = await testCustomProvider({ id: props.provider.id })
  } catch (e) {
    testError.value = e instanceof Error ? e.message : "Test failed"
  } finally {
    testing.value = false
  }
}
</script>

<template>
  <div class="account-row">
    <div class="account-top">
      <div class="account-meta">
        <span class="status-dot" :class="provider.status" :title="provider.status" />
        <span class="account-name">{{ provider.name }}</span>
        <span class="status-pill">{{ formatLabel }}</span>
      </div>
      <div class="account-actions">
        <button
          type="button"
          class="btn btn-secondary btn-sm"
          :disabled="busy || testing"
          @click="runTest"
        >
          {{ testing ? "Testing…" : "Test" }}
        </button>
        <button
          type="button"
          class="btn btn-secondary btn-sm"
          :disabled="busy"
          @click="emit('edit')"
        >
          Edit
        </button>
        <button
          type="button"
          class="btn btn-danger btn-sm"
          :disabled="busy"
          @click="emit('remove')"
        >
          Remove
        </button>
      </div>
    </div>

    <div class="cp-details">
      <div class="cp-detail-row">
        <span class="cp-detail-label">Model id</span>
        <code class="mono">{{ provider.slug }}/*</code>
      </div>
      <div class="cp-detail-row">
        <span class="cp-detail-label">Base URL</span>
        <code class="mono cp-truncate" :title="provider.base_url">{{ provider.base_url }}</code>
      </div>
      <div class="cp-detail-row">
        <span class="cp-detail-label">Key</span>
        <code class="mono">{{ provider.key_mask ?? "—" }}</code>
      </div>
    </div>

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
    <div v-if="testError" class="banner error">{{ testError }}</div>
  </div>
</template>

<style scoped>
.cp-details {
  display: grid;
  gap: 6px;
}

.cp-detail-row {
  display: grid;
  grid-template-columns: 76px minmax(0, 1fr);
  gap: 10px;
  align-items: baseline;
  font-size: 12.5px;
}

.cp-detail-label {
  color: var(--muted);
  font-size: 11.5px;
}

.cp-truncate {
  display: inline-block;
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  vertical-align: bottom;
}

@media (max-width: 720px) {
  .cp-detail-row {
    grid-template-columns: 1fr;
    gap: 2px;
  }
}
</style>
