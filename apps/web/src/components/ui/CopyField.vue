<script setup lang="ts">
/**
 * A read-only value with a copy button — base URLs, model ids, a new API key.
 *
 * The confirmation is announced, not just colored: a screen-reader user
 * pressing Copy otherwise gets no feedback at all.
 */
import { ref } from "vue"
import { useI18n } from "@/i18n"
import AppButton from "./AppButton.vue"

const props = defineProps<{
  value: string
  label?: string
  /** Emphasized treatment for a value the user must copy now (a fresh key). */
  emphasis?: boolean
}>()

const emit = defineEmits<{ error: [string] }>()

const { t } = useI18n()
const copied = ref(false)
let resetTimer: number | undefined

async function copy() {
  try {
    await navigator.clipboard.writeText(props.value)
    copied.value = true
    window.clearTimeout(resetTimer)
    resetTimer = window.setTimeout(() => {
      copied.value = false
    }, 1600)
  } catch {
    emit("error", t("state.copyFailed"))
  }
}
</script>

<template>
  <div class="copy" :class="{ emphasis }">
    <span v-if="label" class="copy-label">{{ label }}</span>
    <code class="copy-value mono">{{ value }}</code>
    <AppButton size="sm" :variant="emphasis ? 'primary' : 'secondary'" @click="copy">
      <template #icon>
        <svg v-if="copied" viewBox="0 0 16 16" fill="none" aria-hidden="true">
          <path
            d="M3.5 8.5l3 3 6-6.5"
            stroke="currentColor"
            stroke-width="1.6"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
        <svg v-else viewBox="0 0 16 16" fill="none" aria-hidden="true">
          <rect
            x="5.75"
            y="5.75"
            width="8.5"
            height="8.5"
            rx="1.75"
            stroke="currentColor"
            stroke-width="1.4"
          />
          <path
            d="M10.25 3.75A1.75 1.75 0 008.5 2h-4.75A1.75 1.75 0 002 3.75V8.5c0 .966.784 1.75 1.75 1.75"
            stroke="currentColor"
            stroke-width="1.4"
            stroke-linecap="round"
          />
        </svg>
      </template>
      {{ copied ? t("action.copied") : t("action.copy") }}
    </AppButton>
    <span class="sr-only" role="status" aria-live="polite">
      {{ copied ? t("action.copied") : "" }}
    </span>
  </div>
</template>

<style scoped>
.copy {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  padding: var(--space-2) var(--space-2) var(--space-2) var(--space-3);
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--surface-2);
}

.copy.emphasis {
  background: var(--surface);
  border-color: var(--border-strong);
}

.copy-label {
  flex-shrink: 0;
  min-width: 128px;
  color: var(--muted);
  font-size: var(--text-xs);
}

.copy-value {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text);
  font-size: var(--text-xs);
}

@media (max-width: 640px) {
  .copy {
    flex-wrap: wrap;
    gap: var(--space-2);
  }

  .copy-label {
    min-width: 0;
    width: 100%;
  }

  .copy-value {
    /* Wrap rather than ellipsize on a phone: there is no hover to reveal the
       rest, and a truncated base URL is useless. */
    flex: 1 1 100%;
    white-space: normal;
    overflow-wrap: anywhere;
  }
}
</style>
