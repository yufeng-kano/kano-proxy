<script setup lang="ts">
/**
 * Modal dialog.
 *
 * Does the four things a dialog must do and that a bare overlay `<div>` does
 * not: traps Tab inside itself, closes on Escape, returns focus to whatever
 * opened it, and hides the rest of the app from assistive tech (`aria-modal`).
 *
 * Below 640px it becomes a bottom sheet — a centered box on a phone leaves the
 * action buttons under the thumb-unfriendly middle of the screen and often
 * behind the keyboard.
 */
import { nextTick, onBeforeUnmount, onMounted, ref } from "vue"
import { useI18n } from "@/i18n"

withDefaults(defineProps<{ title: string; size?: "sm" | "md" | "lg" }>(), { size: "sm" })

const emit = defineEmits<{ close: [] }>()

const { t } = useI18n()
const panel = ref<HTMLElement | null>(null)
/** Restored on unmount, so closing a dialog does not dump focus on <body>. */
let previouslyFocused: HTMLElement | null = null

const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), textarea:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])'

function focusables(): HTMLElement[] {
  if (!panel.value) return []
  return [...panel.value.querySelectorAll<HTMLElement>(FOCUSABLE)].filter(
    (el) => el.offsetParent !== null || el === document.activeElement,
  )
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === "Escape") {
    event.stopPropagation()
    emit("close")
    return
  }
  if (event.key !== "Tab") return

  const items = focusables()
  if (!items.length) {
    // Nothing focusable inside: keep focus on the panel rather than letting
    // Tab escape to the page behind the overlay.
    event.preventDefault()
    panel.value?.focus()
    return
  }
  const first = items[0]!
  const last = items[items.length - 1]!
  const active = document.activeElement

  if (event.shiftKey && (active === first || active === panel.value)) {
    event.preventDefault()
    last.focus()
  } else if (!event.shiftKey && active === last) {
    event.preventDefault()
    first.focus()
  }
}

onMounted(async () => {
  previouslyFocused = document.activeElement as HTMLElement | null
  await nextTick()
  // First field if there is one, else the panel — never the close button,
  // which would make Escape and Enter do the same thing on open.
  const target = focusables().find((el) => el.tagName !== "BUTTON") ?? panel.value
  target?.focus()
})

onBeforeUnmount(() => {
  previouslyFocused?.focus?.()
})
</script>

<template>
  <Teleport to="body">
    <div class="overlay" @click.self="emit('close')" @keydown="onKeydown">
      <div
        ref="panel"
        class="panel"
        :class="size"
        role="dialog"
        aria-modal="true"
        :aria-label="title"
        tabindex="-1"
      >
        <header class="panel-head">
          <h2 class="panel-title">{{ title }}</h2>
          <button type="button" class="panel-close" :aria-label="t('action.close')" @click="emit('close')">
            <svg viewBox="0 0 16 16" fill="none" aria-hidden="true">
              <path
                d="M4 4l8 8M12 4l-8 8"
                stroke="currentColor"
                stroke-width="1.5"
                stroke-linecap="round"
              />
            </svg>
          </button>
        </header>

        <div class="panel-body">
          <slot />
        </div>

        <footer v-if="$slots.footer" class="panel-foot">
          <slot name="footer" />
        </footer>
      </div>
    </div>
  </Teleport>
</template>

<style scoped>
.overlay {
  position: fixed;
  inset: 0;
  z-index: 60;
  display: grid;
  place-items: center;
  padding: var(--space-5);
  background: var(--overlay);
  backdrop-filter: blur(2px);
  animation: fade var(--duration) var(--ease-enter);
}

.panel {
  display: flex;
  flex-direction: column;
  width: 100%;
  /* Never *wider* than the overlay either. A grid child defaults to
     `min-width: auto`, so a wide descendant (the detail modal's chart declares
     a 420px floor) would stretch the panel past the screen and carry the close
     button off the right edge with it — leaving the dialog undismissable by
     tap. Wide content scrolls inside the body instead. */
  min-width: 0;
  /* Never taller than the viewport: the body scrolls, the header and footer
     stay, so Save is always reachable without scrolling to find it. */
  max-height: min(760px, calc(100dvh - var(--space-10)));
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius-lg);
  box-shadow: var(--shadow-lg);
  animation: rise var(--duration-slow) var(--ease-enter);
}

.panel.sm {
  max-width: 460px;
}

.panel.md {
  max-width: 620px;
}

/* Chart + table detail views — wide enough for a 31-bucket plot. */
.panel.lg {
  max-width: 880px;
}

.panel-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-3);
  padding: var(--space-4) var(--space-5);
  border-bottom: 1px solid var(--border);
}

/* Ellipsizes rather than shoving the close button out of the header — the one
   control that must survive every title. */
.panel-title {
  margin: 0;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: var(--text-md);
  font-weight: var(--weight-semibold);
  letter-spacing: var(--tracking-tight);
}

.panel-close {
  display: grid;
  place-items: center;
  width: 28px;
  height: 28px;
  flex-shrink: 0;
  border: none;
  border-radius: var(--radius-sm);
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  transition: background var(--duration-fast) var(--ease);
}

.panel-close:hover {
  background: var(--hover);
  color: var(--text);
}

.panel-close svg {
  width: 16px;
  height: 16px;
}

.panel-body {
  flex: 1;
  min-height: 0;
  /* Same reason as the panel's own: without it a wide child sizes this box
     rather than scrolling within it. */
  min-width: 0;
  overflow-y: auto;
  padding: var(--space-5);
}

.panel-foot {
  display: flex;
  justify-content: flex-end;
  gap: var(--space-2);
  padding: var(--space-4) var(--space-5);
  border-top: 1px solid var(--border);
}

@keyframes fade {
  from {
    opacity: 0;
  }
}

@keyframes rise {
  from {
    opacity: 0;
    transform: translateY(8px) scale(0.99);
  }
}

/* Bottom sheet on phones. */
@media (max-width: 640px) {
  .overlay {
    place-items: end stretch;
    padding: 0;
  }

  .panel,
  .panel.sm,
  .panel.md,
  .panel.lg {
    max-width: none;
    /* `dvh`, not `vh`: the dynamic viewport shrinks when the on-screen
       keyboard opens, so a focused field stays above it instead of being
       covered by it. */
    max-height: 92dvh;
    border-radius: var(--radius-lg) var(--radius-lg) 0 0;
    border-bottom: none;
    animation: sheet var(--duration-slow) var(--ease-enter);
  }

  .panel-foot {
    /* Clears the home indicator on gesture-nav phones. */
    padding-bottom: max(var(--space-4), env(safe-area-inset-bottom));
  }

  @keyframes sheet {
    from {
      transform: translateY(100%);
    }
  }
}
</style>
