<script setup lang="ts">
/**
 * The sticky top of every page: title, primary actions, and — where a page has
 * sections — the section nav.
 *
 * Sticky is the point. The controls a page is operated by (range picker,
 * search, Create) must be reachable at any scroll depth, so they live here
 * rather than in the content that scrolls away.
 *
 * Chrome, not a surface (docs/admin-ui.md § Layout): one compact title row
 * with the subtitle inline beside it, and a background that is the page bg
 * blurred — a stuck header reads as the page fading out under the controls,
 * never as a white slab sitting on it.
 */
defineProps<{
  title: string
  subtitle?: string
}>()

defineSlots<{
  /** Primary + secondary actions, right-aligned. */
  actions?: () => unknown
  /** Section nav or tabs, on their own row below the title. */
  nav?: () => unknown
}>()
</script>

<template>
  <header class="page-header">
    <div class="row">
      <div class="heading">
        <h1 class="title">{{ title }}</h1>
        <p v-if="subtitle" class="subtitle">{{ subtitle }}</p>
      </div>
      <div v-if="$slots.actions" class="actions">
        <slot name="actions" />
      </div>
    </div>

    <div v-if="$slots.nav" class="nav-row">
      <slot name="nav" />
    </div>
  </header>
</template>

<style scoped>
/**
 * Spans exactly the content column — the same left and right edges as the
 * cards below it. It bleeds *upward* only: cancelling the region's top padding
 * is what lets the blur reach the top edge once the header is stuck, with no
 * sliver of scrolling content showing above it.
 *
 * It deliberately does not cancel the horizontal gutter. A header running
 * wider than the content beneath it reads as two different page widths stacked
 * on top of each other, and its title stops lining up with the first column of
 * the table it heads.
 *
 * The top value is AppShell's own, inherited rather than restated — two copies
 * would disagree at whichever breakpoint someone updated only one of them.
 * The fallback keeps the component usable outside the shell (and honest if the
 * property is ever renamed).
 */
.page-header {
  --top: var(--page-top, var(--space-6));

  position: sticky;
  top: 0;
  z-index: 10;
  margin: calc(var(--top) * -1) 0 var(--space-5);
  padding: var(--top) 0 0;
  background: var(--topbar-bg);
  backdrop-filter: blur(12px);
  border-bottom: 1px solid var(--border);
}

/* Title and subtitle share one baseline row: the header is a strip of
   chrome, and two stacked text rows are what made it read as a block. */
.row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-4);
  flex-wrap: wrap;
  padding-bottom: var(--space-3);
}

.heading {
  display: flex;
  align-items: baseline;
  gap: var(--space-3);
  min-width: 0;
  flex-wrap: wrap;
}

.title {
  margin: 0;
  font-size: var(--text-md);
  font-weight: var(--weight-semibold);
  letter-spacing: var(--tracking-tight);
  line-height: 1.3;
}

.subtitle {
  margin: 0;
  color: var(--muted);
  font-size: var(--text-xs);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.actions {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  flex-shrink: 0;
  flex-wrap: wrap;
}

.nav-row {
  /* The nav's own bottom edge is the header's border, so no gap between. */
  margin-bottom: -1px;
}

@media (max-width: 640px) {
  .page-header {
    margin-bottom: var(--space-4);
  }

  /* The subtitle is context, not instruction — on a phone the vertical space
     is worth more than the restatement. */
  .subtitle {
    display: none;
  }

  .actions {
    width: 100%;
  }
}
</style>
