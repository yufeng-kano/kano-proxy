<script setup lang="ts">
/**
 * Inline message: an error that did not block the page, a success
 * confirmation, a caution.
 *
 * Errors and warnings announce themselves (`role="alert"`) since they usually
 * appear in response to something the user just did; neutral and success tones
 * are polite, so a background refresh succeeding does not interrupt a screen
 * reader mid-sentence.
 */
import { computed } from "vue"

const props = withDefaults(
  defineProps<{ tone?: "neutral" | "ok" | "warn" | "error" }>(),
  { tone: "neutral" },
)

const assertive = computed(() => props.tone === "error" || props.tone === "warn")
</script>

<template>
  <div
    class="banner"
    :class="tone"
    :role="assertive ? 'alert' : 'status'"
    :aria-live="assertive ? 'assertive' : 'polite'"
  >
    <div class="banner-body">
      <slot />
    </div>
    <div v-if="$slots.actions" class="banner-actions">
      <slot name="actions" />
    </div>
  </div>
</template>

<style scoped>
.banner {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-3);
  flex-wrap: wrap;
  padding: var(--space-3);
  border-radius: var(--radius-sm);
  border: 1px solid var(--border);
  background: var(--surface-2);
  color: var(--text-secondary);
  font-size: var(--text-sm);
}

.banner-body {
  min-width: 0;
  /* Upstream error text can be one long unbroken token (a URL, a JSON blob);
     without this it would push the card wider than its column. */
  overflow-wrap: anywhere;
}

.banner-actions {
  display: flex;
  gap: var(--space-2);
  flex-shrink: 0;
}

.error {
  background: var(--danger-bg);
  border-color: var(--danger-border);
  color: var(--danger);
}

.warn {
  background: var(--warn-bg);
  border-color: var(--warn-border);
  color: var(--warn);
}

.ok {
  background: var(--ok-bg);
  border-color: var(--ok-border);
  color: var(--ok);
}
</style>
