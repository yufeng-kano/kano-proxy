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
 * with the subtitle inline beside it, and a heavy frosted wash of the page
 * bg — a stuck header reads as the page frosting out under the controls,
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
 * The frost spans the content region, not just the card column. Cancelling
 * the region's top padding lets the blur reach the top edge once the header
 * is stuck; cancelling `--page-gutter` lets the wash and bottom rule run
 * from the sidebar edge to the region's far edge. The same gutter comes back
 * as padding so title, actions, and section nav stay on the card column —
 * the gutter is the shell's `--space-2`, shared with the cards, not a
 * tighter header-only inset. A header whose words run wider than the
 * cards reads as two page widths stacked; a wash that stops at the cards
 * reads as a strip that does not cover the page.
 *
 * Both values are AppShell's own, inherited rather than restated — two
 * copies would disagree at whichever breakpoint someone updated only one
 * of them. The fallbacks keep the component usable outside the shell
 * (and honest if a property is ever renamed).
 */
.page-header {
  --top: var(--page-top, var(--space-6));
  --gutter: var(--page-gutter, var(--space-2));

  position: sticky;
  top: 0;
  z-index: 10;
  margin: calc(var(--top) * -1) calc(var(--gutter) * -1) var(--space-5);
  padding: var(--top) var(--gutter) 0;
  background: var(--topbar-bg);
  /* Heavy frost: smear scrolling cards into milk, not a 12px whisper. */
  backdrop-filter: blur(var(--topbar-blur));
  -webkit-backdrop-filter: blur(var(--topbar-blur));
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

/*
 * Sized to its controls at every width, never widened to fill the line
 * (docs/admin-ui.md § Layout). The auto margin is what keeps it right-aligned
 * on the line it wraps onto: `space-between` puts a lone item at the start, and
 * a wrapped line is a line of one.
 *
 * Left shrinkable rather than pinned, because a line only ever holds this box
 * alone by the time it is too narrow — that is the case Models' search
 * `max-width: 100%` is written for, and it needs a box that can go under 260px
 * to resolve against.
 */
.actions {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  margin-left: auto;
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
}
</style>
