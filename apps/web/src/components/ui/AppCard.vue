<script setup lang="ts">
/**
 * The surface every block of content sits on.
 *
 * `flush` drops the body padding for content that manages its own (a table, a
 * list of rows). `fill` makes the card a flex column that consumes its grid
 * cell, which is what lets a card's body scroll internally instead of growing
 * the page — the anti-scroll rule in docs/admin-ui.md.
 */
defineProps<{
  title?: string
  subtitle?: string
  flush?: boolean
  fill?: boolean
}>()

defineSlots<{
  default: () => unknown
  /** Right side of the header — filters, view switchers, a primary action. */
  actions?: () => unknown
  /** Below the header, outside the padded body: a search field, a legend. */
  toolbar?: () => unknown
}>()
</script>

<template>
  <section class="card" :class="{ fill }">
    <header v-if="title || $slots.actions" class="card-head">
      <div class="card-heading">
        <h2 v-if="title" class="card-title">{{ title }}</h2>
        <p v-if="subtitle" class="card-subtitle">{{ subtitle }}</p>
      </div>
      <div v-if="$slots.actions" class="card-actions">
        <slot name="actions" />
      </div>
    </header>

    <div v-if="$slots.toolbar" class="card-toolbar">
      <slot name="toolbar" />
    </div>

    <div class="card-body" :class="{ flush }">
      <slot />
    </div>
  </section>
</template>

<style scoped>
.card {
  display: flex;
  flex-direction: column;
  min-width: 0;
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  box-shadow: var(--shadow);
}

/**
 * Consumes its cell so .card-body has a bounded height to scroll within —
 * otherwise the body just grows and takes the page with it.
 *
 * This only bounds anything if the *row* is bounded: in an auto-height row,
 * `height: 100%` resolves against content and the card grows anyway. The
 * caller's grid therefore has to give the row a definite height (a fixed
 * track, or `min-height: 0` in a column that is itself bounded). `min-height:
 * 0` here is what lets the card shrink below its content once that holds.
 */
.card.fill {
  height: 100%;
  min-height: 0;
}

.card-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-3);
  flex-wrap: wrap;
  padding: var(--space-4) var(--space-5);
  border-bottom: 1px solid var(--border);
}

.card-heading {
  min-width: 0;
}

.card-title {
  margin: 0;
  font-size: var(--text-sm);
  font-weight: var(--weight-semibold);
  letter-spacing: var(--tracking-tight);
}

.card-subtitle {
  margin: 2px 0 0;
  color: var(--muted);
  font-size: var(--text-xs);
}

.card-actions {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  flex-shrink: 0;
}

.card-toolbar {
  padding: var(--space-3) var(--space-5);
  border-bottom: 1px solid var(--border);
}

.card-body {
  min-height: 0;
  padding: var(--space-5);
}

.card-body.flush {
  padding: 0;
}

/* In a filling card the body is the scroll region. */
.card.fill .card-body {
  flex: 1;
  overflow: auto;
}

@media (max-width: 640px) {
  .card-head {
    padding: var(--space-3) var(--space-4);
  }

  .card-toolbar,
  .card-body {
    padding: var(--space-4);
  }

  .card-body.flush {
    padding: 0;
  }
}
</style>
