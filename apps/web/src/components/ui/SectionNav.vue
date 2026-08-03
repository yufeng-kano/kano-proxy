<!--
  In-page navigation: tabs that switch a view, or anchors that scroll a
  section into the content region.

  This is how a page with several sections stays navigable without the user
  scrolling to find anything (docs/admin-ui.md § Anti-scroll rules). An
  optional count sits on each item so "which of these has anything in it" is
  answerable without visiting them.

  `mode="tabs"` shows one panel at a time and gets full tab semantics;
  `mode="anchors"` keeps every section mounted and scrolls to the chosen one,
  which suits sections a user compares side by side.
-->

<!-- A plain block, not a second `<script setup>`: only one setup block is
     compiled, and a type exported from a second one is silently dropped along
     with every macro in it. -->
<script lang="ts">
export type SectionItem = {
  id: string
  label: string
  count?: number | null
}
</script>

<script setup lang="ts">
withDefaults(
  defineProps<{
    items: SectionItem[]
    active: string
    label: string
    mode?: "tabs" | "anchors"
  }>(),
  { mode: "tabs" },
)

defineEmits<{ select: [string] }>()
</script>

<template>
  <div
    class="section-nav"
    :role="mode === 'tabs' ? 'tablist' : undefined"
    :aria-label="label"
  >
    <button
      v-for="item in items"
      :key="item.id"
      type="button"
      class="item"
      :class="{ active: item.id === active }"
      :role="mode === 'tabs' ? 'tab' : undefined"
      :aria-selected="mode === 'tabs' ? item.id === active : undefined"
      :aria-current="mode === 'anchors' && item.id === active ? 'true' : undefined"
      :aria-controls="mode === 'tabs' ? `panel-${item.id}` : undefined"
      @click="$emit('select', item.id)"
    >
      <span class="item-label">{{ item.label }}</span>
      <span v-if="item.count != null" class="item-count tabular">{{ item.count }}</span>
    </button>
  </div>
</template>

<style scoped>
.section-nav {
  display: flex;
  gap: var(--space-1);
  /* Scrolls rather than wraps: a wrapped second row would move the content
     below it every time the set changes. */
  overflow-x: auto;
  scrollbar-width: none;
}

.section-nav::-webkit-scrollbar {
  display: none;
}

.item {
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
  flex-shrink: 0;
  height: 38px;
  padding: 0 var(--space-3);
  border: none;
  /* The underline is the selected indicator; a transparent one on every item
     keeps the row from shifting by a pixel when selection moves. */
  border-bottom: 2px solid transparent;
  background: transparent;
  color: var(--muted);
  font-size: var(--text-sm);
  font-weight: var(--weight-medium);
  white-space: nowrap;
  cursor: pointer;
  transition:
    color var(--duration-fast) var(--ease),
    border-color var(--duration-fast) var(--ease);
}

.item:hover {
  color: var(--text);
}

.item.active {
  color: var(--text);
  border-bottom-color: var(--accent);
}

.item-count {
  padding: 1px var(--space-2);
  border-radius: var(--radius-full);
  background: var(--hover);
  color: var(--muted);
  font-size: var(--text-2xs);
  font-weight: var(--weight-medium);
}

.item.active .item-count {
  background: var(--accent);
  color: var(--accent-fg);
}

/* Inset so the ring is not clipped by the scroll container's edge. */
.item:focus-visible {
  outline-offset: -3px;
}

@media (pointer: coarse) {
  .item {
    height: 44px;
  }
}
</style>
