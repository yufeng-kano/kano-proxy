<script setup lang="ts">
/**
 * A button showing the current choice that opens a list of rich options —
 * name, subtitle, figures, an optional color dot — for a choice a native
 * `<select>` cannot show (docs/admin-ui.md § Components; Logs' provider and
 * model pickers).
 *
 * Not modal, like the account menu (docs/admin-ui.md § Keyboard): Escape
 * closes it, a press outside closes it, focus returns to the button, and the
 * arrow keys walk the options. The panel is teleported to `body` and placed
 * from the button's rect, because the sticky header it opens from clips and
 * its backdrop filter would trap a fixed child.
 */
import { nextTick, onBeforeUnmount, ref, watch } from "vue"

/* Two roots (button and teleported panel): attributes go to the button by hand. */
defineOptions({ inheritAttrs: false })

export type ListPickerOption = {
  key: string
  label: string
  sub?: string | null
  meta?: string | null
  /** A CSS color for the dot; no dot when absent. */
  color?: string | null
}

const props = withDefaults(
  defineProps<{
    options: ListPickerOption[]
    modelValue: string
    /** Accessible name of the list (and of the button, before its value). */
    label: string
    /** Which edge of the button the panel hangs from. */
    align?: "start" | "end"
  }>(),
  { align: "start" },
)

const emit = defineEmits<{ "update:modelValue": [string] }>()

defineSlots<{
  /** Above the options, inside the panel (Logs' By model / By alias switch). */
  header?: () => unknown
  /** In place of the options when there are none. */
  empty?: () => unknown
}>()

const open = ref(false)
const trigger = ref<HTMLButtonElement | null>(null)
const panel = ref<HTMLElement | null>(null)
const position = ref<{ top: number; left: number }>({ top: 0, left: 0 })

/** Space kept between the panel and the window's edges. */
const GUTTER = 16

const OPTION = 'button[role="option"]'

function current(): ListPickerOption | undefined {
  return props.options.find((o) => o.key === props.modelValue)
}

/**
 * Hangs the panel from the button's edge, then pulls it back inside the
 * window: on a narrow screen the header wraps, and a button far from the
 * edge would otherwise push a wide panel off-screen.
 */
function place() {
  const rect = trigger.value?.getBoundingClientRect()
  const width = panel.value?.offsetWidth
  if (!rect || width === undefined) return
  const wanted = props.align === "end" ? rect.right - width : rect.left
  const left = Math.max(GUTTER, Math.min(wanted, window.innerWidth - GUTTER - width))
  position.value = { top: rect.bottom + 6, left }
}

function close(refocus: boolean) {
  if (!open.value) return
  open.value = false
  if (refocus) trigger.value?.focus()
}

function choose(key: string) {
  close(true)
  if (key !== props.modelValue) emit("update:modelValue", key)
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === "Escape") {
    close(true)
    return
  }
  if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return
  event.preventDefault()
  if (!open.value) {
    open.value = true
    return
  }
  const items = [...(panel.value?.querySelectorAll<HTMLElement>(OPTION) ?? [])]
  if (!items.length) return
  const step = event.key === "ArrowDown" ? 1 : -1
  const at = items.indexOf(document.activeElement as HTMLElement)
  const next = at < 0 ? (step === 1 ? 0 : items.length - 1) : (at + step + items.length) % items.length
  items[next]!.focus()
}

/* Pointer down rather than click: a panel that survives until mouseup reads as
   lagging behind the press that dismissed it. */
function onPointerDown(event: PointerEvent) {
  const target = event.target as Node
  if (trigger.value?.contains(target) || panel.value?.contains(target)) return
  close(false)
}

/** The panel is placed from a rect, so anything that moves the button ends it — but not the list's own scroll. */
function onScroll(event: Event) {
  if (panel.value?.contains(event.target as Node)) return
  close(false)
}

function onResize() {
  close(false)
}

function listen(on: boolean) {
  const method = on ? "addEventListener" : "removeEventListener"
  document[method]("pointerdown", onPointerDown as EventListener)
  window[method]("scroll", onScroll, true)
  window[method]("resize", onResize)
}

watch(open, async (isOpen) => {
  listen(isOpen)
  if (!isOpen) return
  await nextTick()
  place()
  const items = [...(panel.value?.querySelectorAll<HTMLElement>(OPTION) ?? [])]
  ;(items.find((el) => el.getAttribute("aria-selected") === "true") ?? items[0])?.focus()
})

onBeforeUnmount(() => listen(false))
</script>

<template>
  <button
    ref="trigger"
    v-bind="$attrs"
    type="button"
    class="trigger"
    aria-haspopup="listbox"
    :aria-expanded="open ? 'true' : 'false'"
    :aria-label="`${label}: ${current()?.label ?? ''}`"
    @click="open = !open"
    @keydown="onKeydown"
  >
    <span v-if="current()?.color" class="dot" :style="{ background: current()!.color! }" aria-hidden="true" />
    <span class="trigger-label">{{ current()?.label }}</span>
    <svg class="chevron" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M6 9l6 6 6-6" />
    </svg>
  </button>

  <Teleport to="body">
    <div
      v-if="open"
      ref="panel"
      class="panel"
      :style="{ top: `${position.top}px`, left: `${position.left}px` }"
      @keydown="onKeydown"
    >
      <div v-if="$slots.header" class="panel-head">
        <slot name="header" />
      </div>
      <div class="list" role="listbox" :aria-label="label">
        <button
          v-for="option in options"
          :key="option.key"
          type="button"
          role="option"
          class="option"
          :aria-selected="option.key === modelValue ? 'true' : 'false'"
          @click="choose(option.key)"
        >
          <span class="dot" :style="{ background: option.color ?? 'transparent' }" aria-hidden="true" />
          <span class="text">
            <span class="name">{{ option.label }}</span>
            <span v-if="option.sub" class="sub">{{ option.sub }}</span>
          </span>
          <span v-if="option.meta" class="meta">{{ option.meta }}</span>
        </button>
        <p v-if="!options.length && $slots.empty" class="empty"><slot name="empty" /></p>
      </div>
    </div>
  </Teleport>
</template>

<style scoped>
/* Set like the native `.select` beside it, so the header's controls read as one row. */
.trigger {
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
  min-width: 0;
  max-width: 260px;
  height: 34px;
  padding: 0 10px 0 var(--space-3);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-control);
  background: var(--surface);
  color: var(--text);
  font: inherit;
  font-size: var(--text-sm);
  cursor: pointer;
  transition: border-color var(--duration-fast) var(--ease);
}

.trigger:hover {
  border-color: var(--ring-border);
}

.trigger-label {
  min-width: 0;
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
}

.chevron {
  flex-shrink: 0;
  width: 14px;
  height: 14px;
  margin-left: auto;
  fill: none;
  stroke: var(--muted);
  stroke-width: 2;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.dot {
  flex-shrink: 0;
  width: 8px;
  height: 8px;
  border-radius: var(--radius-full);
}

.panel {
  position: fixed;
  z-index: 60;
  display: flex;
  flex-direction: column;
  width: min(380px, calc(100vw - 32px));
  max-height: min(440px, calc(100dvh - 120px));
  padding: var(--space-2);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--surface);
  box-shadow: var(--shadow-lg);
  animation: panel-in var(--duration) var(--ease-enter);
}

.panel-head {
  flex-shrink: 0;
  padding: var(--space-1) var(--space-1) var(--space-2);
}

.list {
  min-height: 0;
  overflow-y: auto;
}

.option {
  display: grid;
  grid-template-columns: 8px minmax(0, 1fr) auto;
  align-items: center;
  column-gap: var(--space-3);
  width: 100%;
  padding: var(--space-2);
  border: none;
  border-radius: var(--radius-sm);
  background: transparent;
  color: var(--text);
  font: inherit;
  text-align: left;
  cursor: pointer;
  transition: background var(--duration-fast) var(--ease);
}

.option:hover,
.option:focus-visible {
  background: var(--hover);
  outline: none;
}

.option[aria-selected="true"] {
  background: var(--hover);
}

.text {
  display: flex;
  flex-direction: column;
  min-width: 0;
}

.name,
.sub {
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
}

.name {
  font-size: var(--text-sm);
  font-weight: var(--weight-medium);
}

.sub {
  font-size: var(--text-xs);
  color: var(--muted);
}

.meta {
  font-size: var(--text-xs);
  color: var(--text-secondary);
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
}

.empty {
  margin: 0;
  padding: var(--space-3) var(--space-2);
  font-size: var(--text-xs);
  color: var(--muted);
}

@keyframes panel-in {
  from {
    opacity: 0;
    transform: translateY(-4px);
  }
  to {
    opacity: 1;
    transform: none;
  }
}
</style>
