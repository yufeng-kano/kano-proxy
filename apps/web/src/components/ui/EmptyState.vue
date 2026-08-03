<script setup lang="ts">
/**
 * What a surface shows when it has nothing — which is the *first* thing a new
 * user sees, so it says what would be here and how to get it, never just
 * "No data".
 *
 * `compact` is for empty states nested inside a card section, where the full
 * vertical treatment would push everything else off-screen.
 */
defineProps<{
  title: string
  body?: string
  compact?: boolean
}>()
</script>

<template>
  <div class="empty" :class="{ compact }">
    <div v-if="$slots.icon && !compact" class="empty-icon" aria-hidden="true">
      <slot name="icon" />
    </div>
    <p class="empty-title">{{ title }}</p>
    <p v-if="body" class="empty-body">{{ body }}</p>
    <div v-if="$slots.action" class="empty-action">
      <slot name="action" />
    </div>
  </div>
</template>

<style scoped>
.empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: var(--space-2);
  padding: var(--space-12) var(--space-5);
  text-align: center;
}

.empty.compact {
  padding: var(--space-6) var(--space-4);
  gap: var(--space-1);
}

.empty-icon {
  display: grid;
  place-items: center;
  width: 44px;
  height: 44px;
  margin-bottom: var(--space-1);
  border-radius: var(--radius);
  background: var(--surface-2);
  border: 1px solid var(--border);
  color: var(--faint);
}

.empty-icon :deep(svg) {
  width: 20px;
  height: 20px;
}

.empty-title {
  margin: 0;
  font-size: var(--text-sm);
  font-weight: var(--weight-semibold);
  color: var(--text);
}

.empty-body {
  margin: 0;
  max-width: 44ch;
  color: var(--muted);
  font-size: var(--text-sm);
  line-height: 1.6;
}

.empty.compact .empty-body {
  font-size: var(--text-xs);
}

.empty-action {
  margin-top: var(--space-3);
}
</style>
