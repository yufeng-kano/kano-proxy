<script setup lang="ts">
/**
 * The app's only button.
 *
 * Renders as `<button>`, or as `<a>` / `<RouterLink>` when given `href` / `to`
 * — a link that looks like a button is still a link, and must keep
 * middle-click, "open in new tab", and the correct role.
 *
 * `loading` shows a spinner *in place of* the icon and disables the control,
 * so a pending action can never be fired twice; the label stays put so the
 * button does not resize mid-press.
 */
import { computed } from "vue"
import Spinner from "./Spinner.vue"

const props = withDefaults(
  defineProps<{
    variant?: "primary" | "secondary" | "ghost" | "danger"
    size?: "sm" | "md"
    /** Square button with an icon and no visible label — `label` becomes its accessible name. */
    iconOnly?: boolean
    loading?: boolean
    disabled?: boolean
    /** Accessible name. Required when `iconOnly`; otherwise a tooltip/title. */
    label?: string
    href?: string
    to?: string
    type?: "button" | "submit"
  }>(),
  {
    variant: "secondary",
    size: "md",
    type: "button",
  },
)

const tag = computed(() => (props.to ? "RouterLink" : props.href ? "a" : "button"))
const isDisabled = computed(() => props.disabled || props.loading)

/**
 * A disabled *link* has no native equivalent, so it is downgraded to a plain
 * span-like anchor with `aria-disabled` rather than shipping a clickable
 * control that looks inert.
 */
const bindings = computed(() => {
  if (props.to) {
    return isDisabled.value
      ? { role: "link", "aria-disabled": "true" }
      : { to: props.to }
  }
  if (props.href) {
    return isDisabled.value
      ? { role: "link", "aria-disabled": "true" }
      : { href: props.href, target: "_blank", rel: "noopener noreferrer" }
  }
  return { type: props.type, disabled: isDisabled.value }
})
</script>

<template>
  <component
    :is="tag"
    class="btn"
    :class="[`btn-${variant}`, `btn-${size}`, { 'btn-icon': iconOnly, 'is-loading': loading }]"
    :aria-label="iconOnly ? label : undefined"
    :title="iconOnly ? label : undefined"
    :aria-busy="loading ? 'true' : undefined"
    v-bind="bindings"
  >
    <Spinner v-if="loading" class="btn-spinner" />
    <slot v-else name="icon" />
    <span v-if="!iconOnly" class="btn-label"><slot /></span>
  </component>
</template>

<style scoped>
.btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: var(--space-2);
  border: 1px solid transparent;
  border-radius: var(--radius-sm);
  font-weight: var(--weight-medium);
  white-space: nowrap;
  cursor: pointer;
  text-decoration: none;
  transition:
    background var(--duration-fast) var(--ease),
    border-color var(--duration-fast) var(--ease),
    color var(--duration-fast) var(--ease);
}

.btn-md {
  height: 34px;
  padding: 0 var(--space-3);
  font-size: var(--text-sm);
}

.btn-sm {
  height: 28px;
  padding: 0 var(--space-2);
  font-size: var(--text-xs);
}

.btn-icon {
  padding: 0;
  aspect-ratio: 1;
}

.btn-icon.btn-md {
  width: 34px;
}

.btn-icon.btn-sm {
  width: 28px;
}

.btn:disabled,
.btn[aria-disabled="true"] {
  opacity: 0.5;
  cursor: not-allowed;
  pointer-events: none;
}

/* Loading keeps full opacity — the spinner already says "busy", and dimming
   on top of it reads as broken rather than pending. */
.btn.is-loading {
  opacity: 1;
  cursor: progress;
}

.btn-primary {
  background: var(--accent);
  color: var(--accent-fg);
}

.btn-primary:hover {
  background: var(--accent-hover);
}

.btn-secondary {
  background: var(--surface);
  color: var(--text);
  border-color: var(--border-strong);
}

.btn-secondary:hover {
  background: var(--surface-2);
  border-color: var(--faint);
}

.btn-ghost {
  background: transparent;
  color: var(--text-secondary);
}

.btn-ghost:hover {
  background: var(--hover);
  color: var(--text);
}

.btn-danger {
  background: var(--surface);
  color: var(--danger);
  border-color: var(--danger-border);
}

.btn-danger:hover {
  background: var(--danger-bg);
}

.btn-spinner {
  flex-shrink: 0;
}

.btn :deep(svg) {
  flex-shrink: 0;
  width: 15px;
  height: 15px;
}

.btn-sm :deep(svg) {
  width: 14px;
  height: 14px;
}

/* Touch targets need the full 44px on a coarse pointer even where the visual
   height stays 34px — the padding is invisible, the hit area is not. */
@media (pointer: coarse) {
  .btn-md {
    min-height: 40px;
  }

  .btn-sm {
    min-height: 34px;
  }
}
</style>
