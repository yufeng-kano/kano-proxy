<script setup lang="ts">
/**
 * A setting: its label and one-line description at the left, its control at
 * the right; on a phone the control drops under the text (docs/admin-ui.md
 * § Settings dialog). The `label` slot replaces the label text when a row
 * needs more than words there (the Account section's photo and name).
 */
defineProps<{ label?: string; description?: string }>()
</script>

<template>
  <div class="row">
    <div class="row-text">
      <slot name="label">
        <div class="row-label">{{ label }}</div>
      </slot>
      <div v-if="description" class="row-description">{{ description }}</div>
    </div>
    <div v-if="$slots.default" class="row-control">
      <slot />
    </div>
  </div>
</template>

<style scoped>
.row {
  display: flex;
  align-items: center;
  gap: var(--space-4);
  padding: var(--space-3) var(--space-4);
}

.row-text {
  flex: 1;
  min-width: 0;
}

.row-label {
  font-size: var(--text-base);
  font-weight: var(--weight-medium);
  color: var(--text);
}

.row-description {
  margin-top: 2px;
  font-size: var(--text-sm);
  line-height: 1.45;
  color: var(--muted);
}

.row-control {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  flex-shrink: 0;
}

@media (max-width: 640px) {
  .row {
    flex-direction: column;
    align-items: stretch;
    gap: var(--space-3);
  }

  .row-control {
    flex-wrap: wrap;
  }
}
</style>
