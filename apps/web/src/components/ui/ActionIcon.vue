<script setup lang="ts">
/**
 * Control glyphs, inlined.
 *
 * Same spec as NavIcon — a 16px stroked set on a 16px grid, 1.4 stroke, round
 * caps and joins — so a Refresh in the page header and a Copy in a table row
 * read as one family. Inline rather than an icon dependency, for the same
 * reason: a handful of glyphs does not justify a package, and a remote sprite
 * would be a render-blocking request for chrome that must paint immediately.
 *
 * Always decorative. An icon-only control carries its name in AppButton's
 * `label` (which becomes both the accessible name and the tooltip), never in
 * the glyph — so this is unconditionally `aria-hidden`.
 */
defineProps<{
  name: "refresh" | "copy" | "check" | "edit" | "expand" | "plus" | "arrow-up" | "arrow-down" | "grip"
}>()
</script>

<template>
  <svg
    viewBox="0 0 16 16"
    fill="none"
    stroke="currentColor"
    stroke-width="1.4"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
    focusable="false"
  >
    <!-- Refresh: an open circular arrow. Not a full ring — the gap and the
         arrowhead are what distinguish it from the Spinner it replaces while
         a refresh is in flight. -->
    <template v-if="name === 'refresh'">
      <path d="M13.5 8a5.5 5.5 0 11-1.61-3.89" />
      <path d="M13.5 2.5V5H11" />
    </template>

    <!-- Copy: two offset sheets. -->
    <template v-else-if="name === 'copy'">
      <rect x="5.75" y="5.75" width="8.5" height="8.5" rx="1.75" />
      <path d="M10.25 3.75A1.75 1.75 0 008.5 2H3.75A1.75 1.75 0 002 3.75V8.5c0 .966.784 1.75 1.75 1.75" />
    </template>

    <!-- Edit: a pencil over its edit line — the gate in front of row actions. -->
    <template v-else-if="name === 'edit'">
      <path d="M11.1 2.6a1.6 1.6 0 012.3 2.3L6 12.3l-3 .7.7-3z" />
      <path d="M9.5 4.2l2.3 2.3" />
    </template>

    <!-- Expand: two outward corner arrows — opens a card's detail view. -->
    <template v-else-if="name === 'expand'">
      <path d="M9.5 2.5H13.5V6.5" />
      <path d="M13.5 2.5L9 7" />
      <path d="M6.5 13.5H2.5V9.5" />
      <path d="M2.5 13.5L7 9" />
    </template>

    <!-- Plus: create. -->
    <template v-else-if="name === 'plus'">
      <path d="M8 3v10M3 8h10" />
    </template>

    <!-- Arrow up / down: move a row one position within its list. -->
    <template v-else-if="name === 'arrow-up'">
      <path d="M8 12.5V3.5" />
      <path d="M4.25 7.25L8 3.5l3.75 3.75" />
    </template>

    <template v-else-if="name === 'arrow-down'">
      <path d="M8 3.5v9" />
      <path d="M4.25 8.75L8 12.5l3.75-3.75" />
    </template>

    <!-- Grip: the drag handle's two rails. Decoration over the move buttons. -->
    <template v-else-if="name === 'grip'">
      <path d="M6 4.5h.01M6 8h.01M6 11.5h.01M10 4.5h.01M10 8h.01M10 11.5h.01" />
    </template>

    <!-- Check: the confirmation the copy swaps to. -->
    <template v-else>
      <path d="M3 8.5l3.25 3.25L13 5" />
    </template>
  </svg>
</template>
