<script setup lang="ts">
/**
 * The Settings dialog (docs/admin-ui.md § Settings dialog).
 *
 * Opened over whatever page the account menu was used on. The open section is
 * the route's `?settings=` query, so a link can open it and a reload keeps it;
 * the shell mounts this component only while that query is present.
 */
import { computed, ref, watch, type Component } from "vue"
import { useRoute, useRouter, type LocationQueryRaw } from "vue-router"
import { useWebExtensions } from "@/extensions"
import { useI18n } from "@/i18n"
import Modal from "@/components/ui/Modal.vue"
import NavIcon from "@/components/ui/NavIcon.vue"
import AccountSection from "./AccountSection.vue"
import GeneralSection from "./GeneralSection.vue"

type Section = {
  name: string
  label: string
  component: Component
  icon?: Component
  navIcon?: "settings" | "account"
  clearQuery?: readonly string[]
}

const { t } = useI18n()
const route = useRoute()
const router = useRouter()
const { settingsSections = [] } = useWebExtensions()

const sections = computed<Section[]>(() => {
  const core: Section[] = [
    { name: "general", label: t("settings.general"), component: GeneralSection, navIcon: "settings" },
    { name: "account", label: t("settings.account"), component: AccountSection, navIcon: "account" },
  ]
  const taken = new Set(core.map((s) => s.name))
  return [...core, ...settingsSections.filter((s) => !taken.has(s.name))]
})

/** An unknown or empty `?settings=` opens General. */
const active = computed(() => sections.value.find((s) => s.name === route.query.settings) ?? sections.value[0]!)

const pane = ref<HTMLElement | null>(null)
watch(
  () => active.value.name,
  () => {
    if (pane.value) pane.value.scrollTop = 0
  },
)

/** Leaving a section drops the one-shot query it read, so coming back does not replay it. */
function select(name: string) {
  if (name === active.value.name) return
  const query: LocationQueryRaw = { ...route.query, settings: name }
  for (const key of active.value.clearQuery ?? []) delete query[key]
  void router.replace({ query })
}

/**
 * Back where the menu's push came from when that is the entry behind this
 * one, so the browser's Back button is not left pointing at the same page;
 * otherwise (a deep link, a payment return) the URL is rewritten in place.
 */
function close() {
  const query = { ...route.query }
  delete query.settings
  for (const section of sections.value) for (const key of section.clearQuery ?? []) delete query[key]
  const back: unknown = window.history.state?.back
  if (typeof back === "string" && back === router.resolve({ query }).fullPath) router.back()
  else void router.replace({ query })
}
</script>

<template>
  <Modal :title="t('settings.title')" size="lg" flush @close="close">
    <div class="settings">
      <nav class="sections" :aria-label="t('settings.sections')">
        <button
          v-for="section in sections"
          :key="section.name"
          type="button"
          class="section-item"
          :class="{ active: section.name === active.name }"
          :aria-current="section.name === active.name ? 'page' : undefined"
          @click="select(section.name)"
        >
          <NavIcon v-if="section.navIcon" :name="section.navIcon" />
          <component :is="section.icon" v-else />
          <span class="section-label">{{ section.label }}</span>
        </button>
      </nav>
      <div ref="pane" class="pane">
        <h3 class="pane-title">{{ active.label }}</h3>
        <component :is="active.component" :key="active.name" />
      </div>
    </div>
  </Modal>
</template>

<style scoped>
/* A fixed height, so moving between a short and a long section never resizes the dialog. */
.settings {
  display: flex;
  height: min(560px, calc(100dvh - 160px));
  min-height: 0;
}

.sections {
  display: flex;
  flex-direction: column;
  gap: 2px;
  width: 196px;
  flex-shrink: 0;
  padding: var(--space-2);
  overflow-y: auto;
  background: var(--surface-2);
  border-right: 1px solid var(--border);
  border-bottom-left-radius: var(--radius-lg);
}

.section-item {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  height: 34px;
  padding: 0 var(--space-2);
  border: none;
  border-radius: var(--radius-sm);
  background: transparent;
  color: var(--muted);
  font: inherit;
  font-size: var(--text-sm);
  text-align: left;
  cursor: pointer;
  transition:
    background var(--duration-fast) var(--ease),
    color var(--duration-fast) var(--ease);
}

.section-item:hover {
  background: var(--hover);
  color: var(--text);
}

/* Fill and a weight step, like the sidebar's active item: never colour alone. */
.section-item.active {
  background: var(--hover);
  color: var(--text);
  font-weight: var(--weight-medium);
}

.section-item :deep(svg) {
  width: 16px;
  height: 16px;
  flex-shrink: 0;
  pointer-events: none;
}

.section-label {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.pane {
  display: flex;
  flex: 1;
  flex-direction: column;
  gap: var(--space-5);
  min-width: 0;
  padding: var(--space-5) var(--space-6) var(--space-6);
  overflow-y: auto;
  overscroll-behavior: contain;
}

.pane-title {
  margin: 0;
  font-size: var(--text-md);
  font-weight: var(--weight-semibold);
  letter-spacing: var(--tracking-tight);
}

/* The list becomes one scrollable row of tabs above the section. */
@media (max-width: 720px) {
  .settings {
    flex-direction: column;
  }

  .sections {
    flex-direction: row;
    width: auto;
    padding: var(--space-2) var(--space-3);
    overflow-x: auto;
    overflow-y: hidden;
    border-right: none;
    border-bottom: 1px solid var(--border);
    border-bottom-left-radius: 0;
    background: var(--surface);
  }

  .section-item {
    flex-shrink: 0;
    height: 36px;
    padding: 0 var(--space-3);
    border-radius: var(--radius-full);
    background: var(--hover);
  }

  .section-item.active {
    background: var(--accent);
    color: var(--accent-fg);
  }

  .pane {
    padding: var(--space-4);
  }
}

@media (max-width: 640px) {
  .settings {
    height: calc(92dvh - 60px);
  }
}
</style>
