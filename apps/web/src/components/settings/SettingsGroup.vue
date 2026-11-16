<script setup lang="ts">
/**
 * One subject's rows in one rounded box, with an optional heading above and a
 * muted note below (docs/admin-ui.md § Settings dialog). Exported for an
 * edition's own sections, so every section draws the same way.
 */
defineProps<{ heading?: string }>()
</script>

<template>
  <section class="group">
    <h4 v-if="heading" class="group-heading">{{ heading }}</h4>
    <div class="group-box">
      <slot />
    </div>
    <p v-if="$slots.note" class="group-note"><slot name="note" /></p>
  </section>
</template>

<style scoped>
.group {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
}

.group-heading {
  margin: 0;
  font-size: var(--text-xs);
  font-weight: var(--weight-medium);
  color: var(--faint);
}

.group-box {
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--surface-2);
}

/* Hairlines between rows, drawn by the box so a row never needs to know its position. */
.group-box > :deep(* + *) {
  border-top: 1px solid var(--border);
}

.group-note {
  margin: 0;
  font-size: var(--text-xs);
  line-height: 1.5;
  color: var(--faint);
}
</style>
