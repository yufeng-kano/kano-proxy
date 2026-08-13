<script setup lang="ts">
/**
 * Nav glyphs, inlined.
 *
 * A 16px stroked set on a 16px grid, drawn to one spec (1.4 stroke, round
 * caps and joins) so they read as one family. Inline rather than an icon
 * dependency: six glyphs do not justify a package, and a remote sprite would
 * be a render-blocking request for chrome that must paint immediately.
 *
 * Always decorative — every nav item ships a text label beside it.
 */
defineProps<{
  name: "overview" | "providers" | "models" | "groups" | "keys" | "changelog"
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
    <!-- Overview: bar chart -->
    <template v-if="name === 'overview'">
      <path d="M2 13.5h12" />
      <path d="M4.25 13.5V8M8 13.5V3.5M11.75 13.5V6.25" />
    </template>

    <!-- Providers: stacked layers -->
    <template v-else-if="name === 'providers'">
      <path d="M8 2L14 5.25 8 8.5 2 5.25 8 2Z" />
      <path d="M2 10.75L8 14l6-3.25" />
      <path d="M2 8L8 11.25 14 8" />
    </template>

    <!-- Models: node graph -->
    <template v-else-if="name === 'models'">
      <circle cx="4" cy="4" r="2" />
      <circle cx="12" cy="8" r="2" />
      <circle cx="4" cy="12" r="2" />
      <path d="M5.75 5L10.25 7M5.75 11L10.25 9" />
    </template>

    <!-- Groups: one name on the left fanning out to the models it stands for -->
    <template v-else-if="name === 'groups'">
      <rect x="1.25" y="5.75" width="4.5" height="4.5" rx="1.5" />
      <path d="M5.75 8H8" />
      <path d="M8 8V5.5A1.5 1.5 0 019.5 4H14" />
      <path d="M8 8h6" />
      <path d="M8 8v2.5A1.5 1.5 0 009.5 12H14" />
    </template>

    <!-- Keys -->
    <template v-else-if="name === 'keys'">
      <circle cx="5.25" cy="10.75" r="2.75" />
      <path d="M7.25 8.75L13 3M11 5l1.5 1.5M9.5 6.5L11 8" />
    </template>

    <!-- Changelog: document with lines -->
    <template v-else>
      <path d="M9 2H4.5A1.5 1.5 0 003 3.5v9A1.5 1.5 0 004.5 14h7a1.5 1.5 0 001.5-1.5V6L9 2Z" />
      <path d="M9 2v3.5A.5.5 0 009.5 6H13" />
      <path d="M5.75 9.25h4.5M5.75 11.5h3" />
    </template>
  </svg>
</template>
