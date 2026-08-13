<script setup lang="ts">
/**
 * Label + control + hint/error, wired together.
 *
 * The point is the wiring: `for`/`id`, `aria-describedby` to the hint, and
 * `aria-invalid` + a linked error message. Hand-rolled fields drift on exactly
 * those, and the drift is invisible until someone uses a screen reader.
 */
import { computed, useId } from "vue"

const props = defineProps<{
  label: string
  hint?: string
  error?: string
  /** Renders next to the label — for genuinely optional inputs. */
  optionalText?: string
  /**
   * Renders the hint between the label and the control instead of below it —
   * for guidance the user should read before typing, not after. The error
   * stays below the control either way.
   */
  hintAbove?: boolean
}>()

const id = useId()
const hintId = computed(() => (props.hint ? `${id}-hint` : undefined))
const errorId = computed(() => (props.error ? `${id}-error` : undefined))

const describedBy = computed(
  () => [errorId.value, hintId.value].filter(Boolean).join(" ") || undefined,
)
</script>

<template>
  <div class="field">
    <label class="field-label" :for="id">
      {{ label }}
      <span v-if="optionalText" class="field-optional">{{ optionalText }}</span>
    </label>

    <p v-if="hintAbove && hint" :id="hintId" class="field-hint"><slot name="hint">{{ hint }}</slot></p>

    <slot
      :id="id"
      :described-by="describedBy"
      :invalid="!!error"
    />

    <p v-if="error" :id="errorId" class="field-error">{{ error }}</p>
    <p v-else-if="hint && !hintAbove" :id="hintId" class="field-hint"><slot name="hint">{{ hint }}</slot></p>
  </div>
</template>

<style scoped>
.field {
  display: grid;
  /* Pins the track to the container: a long unbroken value (a URL, a key)
     must not inflate the grid and get clipped. */
  grid-template-columns: minmax(0, 1fr);
  gap: var(--space-2);
}

.field-label {
  display: flex;
  align-items: baseline;
  gap: var(--space-2);
  font-size: var(--text-xs);
  font-weight: var(--weight-medium);
  color: var(--text-secondary);
}

.field-optional {
  color: var(--faint);
  font-weight: var(--weight-normal);
}

.field-hint,
.field-error {
  margin: 0;
  font-size: var(--text-2xs);
  line-height: 1.5;
  overflow-wrap: anywhere;
}

.field-hint {
  color: var(--muted);
}

.field-error {
  color: var(--danger);
}
</style>
