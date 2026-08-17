<script setup lang="ts">
/**
 * One user-defined endpoint inside the custom section.
 *
 * Mirrors AccountCard's row shape so the two sections read as one page — the
 * section-owned `editing` gate included (docs/admin-ui.md § Providers page) —
 * but carries a static key instead of a usage window: the details are the
 * model prefix, the base URL, the token-count URL when one is configured, and
 * the key mask (never the key itself — see docs/admin-ui.md § Data freshness).
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
  /** The section's gate — the row's actions render only while this is on. */
  editing?: boolean
  /**
   * Reordering controls, owned by the section: it knows the list length, so it
   * decides whether this row can move and whether ordering is offered at all.
   */
  reorderable?: boolean
  canMoveUp?: boolean
  canMoveDown?: boolean
  /** True while this row is the one being dragged — paired with a text cue, never color alone. */
  dragging?: boolean
}>()

const emit = defineEmits<{
  resume: []
  edit: []
  remove: []
  moveUp: []
  moveDown: []
  dragStart: []
  dragEnter: []
  dragEnd: []
}>()

const { t } = useI18n()

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
 * The complete token-count URL, shown only when an OpenAI-format endpoint has
 * one — anthropic rows derive that path from their base and never carry the
 * field (docs/providers.md § Custom endpoints).
 */
const countTokensUrl = computed(() =>
  props.provider.format === "openai" ? props.provider.count_tokens_url || null : null,
)

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

/**
 * The row is only `draggable` while the pointer is on the handle. Marking the
 * whole row draggable makes selecting the base URL start a drag instead, and
 * the handle is the affordance we advertise.
 */
const dragArmed = ref(false)

function onDragStart(event: DragEvent) {
  if (!dragArmed.value) {
    event.preventDefault()
    return
  }
  // Required for Firefox to start a drag at all; the payload is unused — the
  // page tracks which row is moving.
  event.dataTransfer?.setData("text/plain", props.provider.id)
  if (event.dataTransfer) event.dataTransfer.effectAllowed = "move"
  emit("dragStart")
}

function onDragEnd() {
  dragArmed.value = false
  emit("dragEnd")
}

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
  <div
    class="endpoint"
    :class="{ 'is-dragging': dragging }"
    :draggable="reorderable && dragArmed ? true : undefined"
    @dragstart="onDragStart"
    @dragenter="reorderable && emit('dragEnter')"
    @dragover.prevent
    @dragend="onDragEnd"
  >
    <div class="endpoint-head">
      <!-- Decoration on top of the move buttons: the same capability lives in
           real controls below, so the handle itself is hidden from a11y. -->
      <span
        v-if="editing && reorderable"
        class="handle"
        aria-hidden="true"
        @pointerdown="dragArmed = true"
        @pointerup="dragArmed = false"
      >
        <ActionIcon name="grip" />
      </span>

      <div class="identity">
        <span class="name" :title="provider.name">{{ provider.name }}</span>
        <div class="tags">
          <StatusDot :status="provider.status" />
          <Badge>{{ formatLabel }}</Badge>
          <!-- A word, not a tint: the drag state has to survive a color-blind
               reader and a high-contrast mode. -->
          <span v-if="dragging" class="moving">{{ t("custom.reorder.moving") }}</span>
        </div>
      </div>

      <!-- The blank space at the row's right edge, filled only while the
           section's gate is open. Iconography like the account rows above, and
           `label` carries the endpoint name for the accessible name since
           several rows offer the same actions. Two keep their word: Remove,
           because deleting the endpoint deletes its stored key, and Test,
           because no glyph says "send a probe request" without a hover (a play
           triangle already means Resume on this very row). -->
      <div v-if="editing" class="actions">
        <template v-if="reorderable">
          <AppButton
            icon-only
            size="sm"
            variant="ghost"
            :label="t('custom.reorder.moveUp', { name: provider.name })"
            :disabled="busy || !canMoveUp"
            @click="emit('moveUp')"
          >
            <template #icon><ActionIcon name="arrow-up" /></template>
          </AppButton>
          <AppButton
            icon-only
            size="sm"
            variant="ghost"
            :label="t('custom.reorder.moveDown', { name: provider.name })"
            :disabled="busy || !canMoveDown"
            @click="emit('moveDown')"
          >
            <template #icon><ActionIcon name="arrow-down" /></template>
          </AppButton>
        </template>
        <AppButton
          v-if="provider.status === 'benched'"
          icon-only
          size="sm"
          variant="ghost"
          :label="t('custom.resumeEndpoint', { name: provider.name })"
          :disabled="busy || testing"
          @click="emit('resume')"
        >
          <template #icon><ActionIcon name="play" /></template>
        </AppButton>
        <AppButton
          size="sm"
          :label="t('custom.testEndpoint', { name: provider.name })"
          :loading="testing"
          :disabled="busy"
          @click="runTest"
        >
          {{ t("action.test") }}
        </AppButton>
        <AppButton
          icon-only
          size="sm"
          variant="ghost"
          :label="t('custom.editEndpoint', { name: provider.name })"
          :disabled="busy || testing"
          @click="emit('edit')"
        >
          <template #icon><ActionIcon name="edit" /></template>
        </AppButton>
        <AppButton
          size="sm"
          variant="danger"
          :label="t('custom.removeEndpoint', { name: provider.name })"
          :disabled="busy || testing"
          @click="emit('remove')"
        >
          {{ t("action.remove") }}
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
      <!-- Only when the operator configured one: an unset field is the common
           case and must not leave a permanent empty row behind
           (docs/admin-ui.md § Providers page). -->
      <div v-if="countTokensUrl" class="detail">
        <dt>{{ t("custom.field.countTokens") }}</dt>
        <dd>
          <code class="mono truncate" :title="countTokensUrl">{{ countTokensUrl }}</code>
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

/* Grabbable, but never the only way to reorder — the move buttons carry the
   same capability for keyboard and touch. */
.handle {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 20px;
  height: 24px;
  flex-shrink: 0;
  color: var(--faint);
  cursor: grab;
  touch-action: none;
}

.handle:active {
  cursor: grabbing;
}

/* Dimmed *and* labelled: the "Moving" tag beside the name is what carries the
   state; the opacity is only there to say which row the drop will land on. */
.endpoint.is-dragging {
  opacity: 0.6;
}

.moving {
  color: var(--muted);
  font-size: var(--text-2xs);
}

.identity {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
  min-width: 0;
  /* The basis is the wrap threshold, not spacing: it has to clear four icon
     buttons plus the gutters at 360px, or the actions drop below the name. */
  flex: 1 1 120px;
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

/* Stays on the name row at every width, right-aligned — same single column of
   controls as the account rows above (docs/admin-ui.md § Providers page). */
.actions {
  display: flex;
  align-items: center;
  gap: var(--space-1);
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
