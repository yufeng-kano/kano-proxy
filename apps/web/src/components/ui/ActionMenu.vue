<script setup lang="ts">
/**
 * A small anchored menu of actions behind one trigger (docs/admin-ui.md §
 * Components; Logs' title menu). It owes what the account menu owes
 * (docs/admin-ui.md § Keyboard): not modal, so no scrim and no focus trap;
 * Escape closes it, a press outside closes it, focus returns to the trigger,
 * and the arrow keys walk its items.
 *
 * The panel is teleported to `body` and placed from the trigger's rect: every
 * container it opens from clips (the card body scrolls, the sticky header's
 * backdrop filter would trap a fixed child), so it cannot live in the flow.
 */
import { nextTick, onBeforeUnmount, ref, watch } from "vue"
import Spinner from "@/components/ui/Spinner.vue"

export interface MenuItem {
  key: string
  label: string
  /** Destructive items keep their word in the danger tone. */
  tone?: "danger"
}

/* Two roots (trigger and teleported panel), so the class a parent passes is
   bound to the trigger by hand rather than left to fall through. */
defineOptions({ inheritAttrs: false })

const props = withDefaults(
  defineProps<{
    items: MenuItem[]
    /** Which edge of the trigger the panel hangs from. */
    align?: "start" | "end"
    /** The trigger reports an action in flight; the menu stays closed meanwhile. */
    loading?: boolean
  }>(),
  { align: "start", loading: false },
)

const emit = defineEmits<{ select: [string] }>()

defineSlots<{
  /** The trigger's content; the button itself is this component's. */
  default: () => unknown
}>()

const open = ref(false)
const trigger = ref<HTMLButtonElement | null>(null)
const panel = ref<HTMLElement | null>(null)
const position = ref<{ top: number; left?: number; right?: number }>({ top: 0 })

const ITEM = 'button[role="menuitem"]'

function place() {
  const rect = trigger.value?.getBoundingClientRect()
  if (!rect) return
  position.value =
    props.align === "end"
      ? { top: rect.bottom + 4, right: window.innerWidth - rect.right }
      : { top: rect.bottom + 4, left: rect.left }
}

function toggle() {
  if (props.loading) return
  open.value = !open.value
}

function close(refocus: boolean) {
  if (!open.value) return
  open.value = false
  if (refocus) trigger.value?.focus()
}

function choose(key: string) {
  close(true)
  emit("select", key)
}

/** Bound to the trigger and the panel: it only runs with focus in one of them. */
function onKeydown(event: KeyboardEvent) {
  if (event.key === "Escape") {
    close(true)
    return
  }
  if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return
  event.preventDefault()
  if (!open.value) {
    toggle()
    return
  }
  const items = [...(panel.value?.querySelectorAll<HTMLElement>(ITEM) ?? [])]
  if (!items.length) return
  const step = event.key === "ArrowDown" ? 1 : -1
  const current = items.indexOf(document.activeElement as HTMLElement)
  const next = current < 0 ? (step === 1 ? 0 : items.length - 1) : (current + step + items.length) % items.length
  items[next]!.focus()
}

/* Pointer down rather than click, as the account menu: a panel that survives
   until mouseup reads as lagging behind the press that dismissed it. */
function onPointerDown(event: PointerEvent) {
  const target = event.target as Node
  if (trigger.value?.contains(target) || panel.value?.contains(target)) return
  close(false)
}

/** The panel is placed from a rect, so anything that moves the trigger ends it. */
function onMoved() {
  close(false)
}

watch(open, async isOpen => {
  if (isOpen) {
    place()
    document.addEventListener("pointerdown", onPointerDown)
    window.addEventListener("scroll", onMoved, true)
    window.addEventListener("resize", onMoved)
    await nextTick()
    panel.value?.querySelector<HTMLElement>(ITEM)?.focus()
  } else {
    document.removeEventListener("pointerdown", onPointerDown)
    window.removeEventListener("scroll", onMoved, true)
    window.removeEventListener("resize", onMoved)
  }
})

onBeforeUnmount(() => {
  document.removeEventListener("pointerdown", onPointerDown)
  window.removeEventListener("scroll", onMoved, true)
  window.removeEventListener("resize", onMoved)
})
</script>

<template>
  <button
    ref="trigger"
    v-bind="$attrs"
    type="button"
    class="trigger"
    aria-haspopup="menu"
    :aria-expanded="open ? 'true' : 'false'"
    :aria-busy="loading ? 'true' : undefined"
    :disabled="loading"
    @click="toggle"
    @keydown="onKeydown"
  >
    <slot />
    <Spinner v-if="loading" :size="12" />
  </button>

  <Teleport to="body">
    <div
      v-if="open"
      ref="panel"
      class="menu"
      role="menu"
      :style="{ top: `${position.top}px`, left: position.left !== undefined ? `${position.left}px` : undefined, right: position.right !== undefined ? `${position.right}px` : undefined }"
      @keydown="onKeydown"
    >
      <button
        v-for="item in items"
        :key="item.key"
        type="button"
        role="menuitem"
        class="item"
        :class="{ danger: item.tone === 'danger' }"
        @click="choose(item.key)"
      >{{ item.label }}</button>
    </div>
  </Teleport>
</template>

<style scoped>
/* The trigger is unstyled: its content decides whether it reads as a title or
   a button, and the parent styles it through the class it passes. */
.trigger {
  display: inline-flex;
  align-items: center;
  gap: var(--space-1);
  padding: 0;
  border: none;
  background: transparent;
  color: inherit;
  font: inherit;
  letter-spacing: inherit;
  cursor: pointer;
}

.trigger:disabled {
  cursor: default;
}

.menu {
  position: fixed;
  z-index: 60;
  min-width: 160px;
  padding: var(--space-1);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--surface);
  box-shadow: var(--shadow-lg);
  animation: menu-in var(--duration) var(--ease-enter);
}

.item {
  display: flex;
  align-items: center;
  width: 100%;
  height: 34px;
  padding: 0 var(--space-2);
  border: none;
  border-radius: var(--radius-sm);
  background: transparent;
  color: var(--text-secondary);
  font-size: var(--text-sm);
  text-align: left;
  white-space: nowrap;
  cursor: pointer;
  transition:
    background var(--duration-fast) var(--ease),
    color var(--duration-fast) var(--ease);
}

.item:hover,
.item:focus-visible {
  background: var(--hover);
  color: var(--text);
}

.item.danger {
  color: var(--danger);
}

.item.danger:hover,
.item.danger:focus-visible {
  background: var(--danger-bg, var(--hover));
  color: var(--danger);
}

@keyframes menu-in {
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
