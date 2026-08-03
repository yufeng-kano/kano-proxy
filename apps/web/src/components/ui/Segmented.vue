<script setup lang="ts">
/**
 * Exclusive choice among 2–4 short options: a time range, a chart view.
 *
 * A radio group, not a row of buttons — arrow keys move between options and
 * the selected one is announced as such. `roving` tabindex keeps the whole
 * group a single tab stop, which is what the pattern expects.
 */
import { computed, ref } from "vue"

type OptionValue = string | number

const props = defineProps<{
  modelValue: OptionValue
  options: { value: OptionValue; label: string; title?: string }[]
  /** Accessible name for the group as a whole. */
  label: string
  size?: "sm" | "md"
}>()

const emit = defineEmits<{ "update:modelValue": [OptionValue] }>()

const root = ref<HTMLElement | null>(null)

const selectedIndex = computed(() =>
  Math.max(
    0,
    props.options.findIndex((o) => o.value === props.modelValue),
  ),
)

function select(value: OptionValue) {
  if (value !== props.modelValue) emit("update:modelValue", value)
}

/**
 * Arrow keys move *and* select — a radio group's selection follows focus, so
 * the user never has to press an extra key to commit the choice.
 */
function onKeydown(event: KeyboardEvent) {
  const last = props.options.length - 1
  let next: number | null = null
  switch (event.key) {
    case "ArrowRight":
    case "ArrowDown":
      next = selectedIndex.value === last ? 0 : selectedIndex.value + 1
      break
    case "ArrowLeft":
    case "ArrowUp":
      next = selectedIndex.value === 0 ? last : selectedIndex.value - 1
      break
    case "Home":
      next = 0
      break
    case "End":
      next = last
      break
    default:
      return
  }
  event.preventDefault()
  const option = props.options[next]
  if (!option) return
  select(option.value)
  root.value?.querySelectorAll<HTMLElement>("[role='radio']")[next]?.focus()
}
</script>

<template>
  <div
    ref="root"
    class="segmented"
    :class="size === 'sm' ? 'sm' : 'md'"
    role="radiogroup"
    :aria-label="label"
    @keydown="onKeydown"
  >
    <button
      v-for="option in options"
      :key="option.value"
      type="button"
      role="radio"
      class="segment"
      :class="{ active: option.value === modelValue }"
      :aria-checked="option.value === modelValue"
      :tabindex="option.value === modelValue ? 0 : -1"
      :title="option.title"
      @click="select(option.value)"
    >
      {{ option.label }}
    </button>
  </div>
</template>

<style scoped>
.segmented {
  display: inline-flex;
  gap: 2px;
  padding: 2px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--surface-2);
}

.segment {
  border: none;
  background: transparent;
  color: var(--muted);
  font-weight: var(--weight-medium);
  border-radius: var(--radius-xs);
  cursor: pointer;
  white-space: nowrap;
  transition:
    background var(--duration-fast) var(--ease),
    color var(--duration-fast) var(--ease);
}

.md .segment {
  height: 28px;
  padding: 0 var(--space-3);
  font-size: var(--text-xs);
}

.sm .segment {
  height: 24px;
  padding: 0 var(--space-2);
  font-size: var(--text-2xs);
}

.segment:hover:not(.active) {
  background: var(--hover);
  color: var(--text);
}

.segment.active {
  background: var(--surface);
  color: var(--text);
  box-shadow: var(--shadow);
}

@media (pointer: coarse) {
  .md .segment {
    height: 34px;
  }

  .sm .segment {
    height: 30px;
  }
}
</style>
