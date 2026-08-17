<script setup lang="ts">
/**
 * The sticky top of every page: the title, the primary actions, and — where a
 * page has sections — the section nav.
 *
 * Sticky is the point. The controls a page is operated by (range picker,
 * search, Create) must be reachable at any scroll depth, so they live here
 * rather than in the content that scrolls away.
 *
 * The visible `h1` is the one thing here that repeats the sidebar's active
 * item, and the named exception to it (docs/admin-ui.md § Design restraint): it
 * anchors the content column and gives the header something to be the top of.
 * What went is the line *beneath* it explaining the page — there is no
 * `subtitle` prop.
 *
 * Chrome, not a surface: one compact title row over a heavy frosted wash of the
 * page bg, so a stuck header reads as the page frosting out under the controls,
 * never as a white slab sitting on it.
 */
defineProps<{
  /** The page's name — the visible `h1`, and the only one on the page. */
  title: string
}>()

defineSlots<{
  /** Primary + secondary actions, right-aligned on the title row. */
  actions?: () => unknown
  /** Section nav or tabs, on their own row below the title. */
  nav?: () => unknown
}>()
</script>

<template>
  <header class="page-header">
    <div class="row">
      <h1 class="title">{{ title }}</h1>
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
 * as padding so title, actions, and the section nav stay on the card column —
 * the gutter is the shell's `--space-2`, shared with the cards, not a
 * tighter header-only inset. A header whose controls run wider than the cards
 * reads as two page widths stacked; a wash that stops at the cards reads as a
 * strip that does not cover the page.
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

/*
 * Title and actions on one row: the header is a strip of chrome, and a second
 * stacked text row is what made it read as a block.
 *
 * The 34px floor is the height of the control that normally sits beside the
 * title — taller than the title itself, so without it the header would lose
 * that much height wherever a page has no action of its own (Keys' Connect
 * tab), which reads as the layout jumping rather than as the page changing.
 */
.row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-4);
  flex-wrap: wrap;
  min-height: 34px;
  padding-bottom: var(--space-3);
}

.title {
  margin: 0;
  min-width: 0;
  font-size: var(--text-md);
  font-weight: var(--weight-semibold);
  letter-spacing: var(--tracking-tight);
  line-height: 1.3;
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
}
</style>
