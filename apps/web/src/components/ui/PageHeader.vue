<script setup lang="ts">
/**
 * The sticky top of every page: title, primary actions, and — where a page has
 * sections — the section nav.
 *
 * Sticky is the point. The controls a page is operated by (range picker,
 * search, Create) must be reachable at any scroll depth, so they live here
 * rather than in the content that scrolls away.
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
 * Bleeds to the content region's edges so the blur covers the full width,
 * while the inner rows keep the page's gutter. The gutter values are AppShell's
 * own, inherited rather than restated — two copies would disagree at whichever
 * breakpoint someone updated only one of them.
 *
 * Fallbacks make the component usable outside the shell (and keep it honest if
 * the properties are ever renamed).
 */
.page-header {
  --gutter: var(--page-gutter, var(--space-8));
  --top: var(--page-top, var(--space-6));

  position: sticky;
  top: 0;
  z-index: 10;
  margin: calc(var(--top) * -1) calc(var(--gutter) * -1) var(--space-6);
  padding: var(--top) var(--gutter) 0;
  background: var(--topbar-bg);
  backdrop-filter: blur(12px);
  border-bottom: 1px solid var(--border);
}

.row {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--space-4);
  flex-wrap: wrap;
  padding-bottom: var(--space-4);
}

.heading {
  min-width: 0;
}

.title {
  margin: 0;
  font-size: var(--text-lg);
  font-weight: var(--weight-semibold);
  letter-spacing: var(--tracking-tighter);
  line-height: 1.2;
}

.subtitle {
  margin: var(--space-1) 0 0;
  color: var(--muted);
  font-size: var(--text-sm);
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

  .row {
    padding-bottom: var(--space-3);
  }

  .title {
    font-size: var(--text-md);
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
