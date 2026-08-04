<script setup lang="ts">
/**
 * One user-defined endpoint inside the custom section.
 *
 * Mirrors AccountCard's row shape so the two sections read as one page — the
 * same pencil-gated actions included (docs/admin-ui.md § Providers page) —
 * but carries a static key instead of a usage window: the details are the
 * model prefix, the base URL, and the key mask (never the key itself — see
 * docs/admin-ui.md § Data freshness).
 */
import { computed, ref } from "vue"
import ActionIcon from "@/components/ui/ActionIcon.vue"
import AppButton from "@/components/ui/AppButton.vue"
import Badge from "@/components/ui/Badge.vue"
import Banner from "@/components/ui/Banner.vue"
import StatusDot from "@/components/ui/StatusDot.vue"
import { useI18n } from "@/i18n"
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

const { t } = useI18n()

/** Pencil-gated: the row's actions render only while this is on. */
const editing = ref(false)
const testing = ref(false)
const testResult = ref<CustomProviderTestResult | null>(null)
const testError = ref<string | null>(null)

const formatLabel = computed(() =>
  props.provider.format === "anthropic"
    ? t("custom.dialog.formatAnthropic")
    : t("custom.dialog.formatOpenAI"),
)

const modelPrefix = computed(() => `${props.provider.slug}/*`)

/**
 * Headline of a finished test: the count when the endpoint reported one.
 * `models_count` is nullable rather than 0-when-absent — an endpoint that
 * answered without a list is not an endpoint with zero models — so a null
 * falls back to the uncounted form instead of claiming "0 models".
 */
const testHeadline = computed(() => {
  const result = testResult.value
  if (!result) return null
  if (!result.ok) return result.error || t("custom.test.failed")
  return result.models_count != null
    ? t("custom.test.okModels", { count: result.models_count })
    : t("custom.test.ok")
})

async function runTest() {
  testing.value = true
  testResult.value = null
  testError.value = null
  try {
    testResult.value = await testCustomProvider({ id: props.provider.id })
  } catch (e) {
    testError.value = e instanceof Error ? e.message : t("custom.test.failed")
  } finally {
    testing.value = false
  }
}
</script>

<template>
  <div class="endpoint">
    <div class="endpoint-head">
      <div class="identity">
        <span class="name" :title="provider.name">{{ provider.name }}</span>
        <div class="tags">
          <StatusDot :status="provider.status" />
          <Badge>{{ formatLabel }}</Badge>
        </div>
      </div>

      <div class="actions">
        <template v-if="editing">
          <AppButton size="sm" :loading="testing" :disabled="busy" @click="runTest">
            {{ t("action.test") }}
          </AppButton>
          <AppButton size="sm" :disabled="busy || testing" @click="emit('edit')">
            {{ t("action.edit") }}
          </AppButton>
          <AppButton
            size="sm"
            variant="danger"
            :disabled="busy || testing"
            @click="emit('remove')"
          >
            {{ t("action.remove") }}
          </AppButton>
        </template>
        <!-- aria-pressed: the same control opens and closes the action set,
             so it is a toggle, and its state must be audible as one. -->
        <AppButton
          icon-only
          size="sm"
          variant="ghost"
          :label="
            editing
              ? t('providers.account.doneEditing', { name: provider.name })
              : t('providers.account.edit', { name: provider.name })
          "
          :aria-pressed="editing"
          @click="editing = !editing"
        >
          <template #icon><ActionIcon :name="editing ? 'check' : 'edit'" /></template>
        </AppButton>
      </div>
    </div>

    <dl class="details">
      <div class="detail">
        <dt>{{ t("custom.field.modelId") }}</dt>
        <dd><code class="mono">{{ modelPrefix }}</code></dd>
      </div>
      <div class="detail">
        <dt>{{ t("custom.field.endpoint") }}</dt>
        <dd>
          <code class="mono truncate" :title="provider.base_url">{{ provider.base_url }}</code>
        </dd>
      </div>
      <div class="detail">
        <dt>{{ t("custom.field.key") }}</dt>
        <dd><code class="mono">{{ provider.key_mask ?? "—" }}</code></dd>
      </div>
    </dl>

    <Banner v-if="testResult" :tone="testResult.ok ? 'ok' : 'error'">
      <div class="result">
        <span>{{ testHeadline }}</span>
        <span v-if="testResult.sample?.length" class="result-note">
          {{ t("custom.test.sample", { models: testResult.sample.join(", ") }) }}
        </span>
        <span v-if="testResult.note" class="result-note">{{ testResult.note }}</span>
      </div>
    </Banner>
    <Banner v-if="testError" tone="error">{{ testError }}</Banner>
  </div>
</template>

<style scoped>
.endpoint {
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
  padding: var(--space-4) 0;
}

.endpoint + .endpoint {
  border-top: 1px solid var(--border);
}

.endpoint-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--space-3);
  flex-wrap: wrap;
}

.identity {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
  min-width: 0;
  flex: 1 1 180px;
}

.name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: var(--text-sm);
  font-weight: var(--weight-medium);
}

.tags {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  flex-wrap: wrap;
}

.actions {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  flex-shrink: 0;
}

.details {
  display: grid;
  gap: var(--space-2);
  margin: 0;
}

.detail {
  display: grid;
  grid-template-columns: 96px minmax(0, 1fr);
  align-items: baseline;
  gap: var(--space-3);
}

.detail dt {
  color: var(--muted);
  font-size: var(--text-2xs);
}

.detail dd {
  margin: 0;
  min-width: 0;
  font-size: var(--text-xs);
}

.truncate {
  display: block;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
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

@media (max-width: 640px) {
  .actions {
    width: 100%;
  }

  /* The pencil stays square — only the labelled actions share the row. */
  .actions :deep(.btn:not(.btn-icon)) {
    flex: 1;
  }

  /* Stacked at 360px: a fixed label column plus a base URL leaves the URL
     unreadable, and there is no hover to reveal the rest. */
  .detail {
    grid-template-columns: minmax(0, 1fr);
    gap: var(--space-1);
  }

  .truncate {
    white-space: normal;
    overflow-wrap: anywhere;
  }
}
</style>
