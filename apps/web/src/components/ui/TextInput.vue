<script setup lang="ts">
/**
 * Text input / textarea. Pairs with FormField, which supplies `id` and
 * `describedBy` through its slot props.
 */
withDefaults(
  defineProps<{
    modelValue: string
    id?: string
    type?: "text" | "password" | "url" | "search"
    placeholder?: string
    describedBy?: string
    invalid?: boolean
    disabled?: boolean
    mono?: boolean
    multiline?: boolean
    rows?: number
    autocomplete?: string
    inputmode?: "text" | "url" | "email" | "numeric"
  }>(),
  { type: "text", rows: 4, autocomplete: "off" },
)

defineEmits<{ "update:modelValue": [string]; enter: [] }>()
</script>

<template>
  <textarea
    v-if="multiline"
    :id="id"
    class="control textarea"
    :class="{ mono, invalid }"
    :value="modelValue"
    :rows="rows"
    :placeholder="placeholder"
    :disabled="disabled"
    :aria-describedby="describedBy"
    :aria-invalid="invalid || undefined"
    :autocomplete="autocomplete"
    @input="$emit('update:modelValue', ($event.target as HTMLTextAreaElement).value)"
  />
  <input
    v-else
    :id="id"
    class="control"
    :class="{ mono, invalid }"
    :type="type"
    :value="modelValue"
    :placeholder="placeholder"
    :disabled="disabled"
    :aria-describedby="describedBy"
    :aria-invalid="invalid || undefined"
    :autocomplete="autocomplete"
    :inputmode="inputmode"
    @input="$emit('update:modelValue', ($event.target as HTMLInputElement).value)"
    @keydown.enter="$emit('enter')"
  />
</template>

<style scoped>
.control {
  width: 100%;
  min-width: 0;
  padding: 0 var(--space-3);
  height: 34px;
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-sm);
  background: var(--surface);
  color: var(--text);
  font-size: var(--text-sm);
  outline: none;
  transition:
    border-color var(--duration-fast) var(--ease),
    box-shadow var(--duration-fast) var(--ease);
}

.control::placeholder {
  color: var(--faint);
}

.control:focus {
  border-color: var(--ring-border);
  box-shadow: var(--ring);
  /* The border + ring above already meet the focus-visibility bar, and the
     global outline would sit awkwardly outside a field's own ring. */
  outline: none;
}

.control:disabled {
  background: var(--surface-2);
  color: var(--muted);
  cursor: not-allowed;
}

.control.invalid {
  border-color: var(--danger-border);
}

.control.invalid:focus {
  border-color: var(--danger);
}

.textarea {
  height: auto;
  min-height: 76px;
  padding: var(--space-2) var(--space-3);
  line-height: 1.6;
  resize: vertical;
}

.mono {
  font-family: var(--mono);
  font-size: var(--text-xs);
}

/* 16px prevents iOS Safari from zooming the viewport on focus. */
@media (pointer: coarse) {
  .control {
    height: 40px;
    font-size: var(--text-md);
  }

  .mono {
    font-size: var(--text-base);
  }
}
</style>
