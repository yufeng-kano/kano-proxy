<script setup lang="ts">
/**
 * The signed-in frame.
 *
 * A fixed grid, not a scrolling document: the sidebar and the page header stay
 * put while only the content region scrolls. That is what makes the app's
 * controls reachable at any depth (docs/admin-ui.md § Layout).
 *
 * The scroll container being an element rather than the window is load-bearing
 * — it is published through services/scrollRegion.ts so scroll restore, the
 * reset-on-navigate, and the Providers section nav all move the right thing.
 *
 * Below 1080px the sidebar becomes a drawer behind a header menu button. No
 * icon rail: with five destinations the labels are doing the work, and no
 * bottom tab bar — it would cost the scarcest axis on a phone.
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue"
import { useRoute, useRouter } from "vue-router"
import { useAuth } from "@/composables/useAuth"
import { useChangelog } from "@/composables/useChangelog"
import { SITE } from "@/config/site"
import { useI18n } from "@/i18n"
import { resetScroll, setScrollRegion } from "@/services/scrollRegion"
import NavIcon from "./NavIcon.vue"

const { t } = useI18n()
const route = useRoute()
const router = useRouter()
const { user, logout } = useAuth()
const { data: changelog, setUserId, load: loadChangelog } = useChangelog()

const region = ref<HTMLElement | null>(null)
const sidebar = ref<HTMLElement | null>(null)
const drawerOpen = ref(false)
const menuButton = ref<HTMLElement | null>(null)

const NAV = [
  { name: "overview", to: "/overview", label: "nav.overview" },
  { name: "providers", to: "/providers", label: "nav.providers" },
  { name: "models", to: "/models", label: "nav.models" },
  { name: "keys", to: "/keys", label: "nav.keys" },
] as const

/** Blank until the first load resolves, so the badge never shows a wrong version. */
const version = computed(() => changelog.value?.current ?? "")
/** Server-computed — a local build ahead of the newest release is not an update. */
const updateAvailable = computed(() => changelog.value?.updateAvailable === true)

const userLabel = computed(() => user.value?.email || user.value?.name || "")

// The badge is part of the signed-in shell, so it loads once the session is
// known. The composable dedupes against the Changelog page's own load when
// both mount on the same tick.
watch(
  () => user.value?.id ?? null,
  (id) => {
    setUserId(id)
    if (id) void loadChangelog()
  },
  { immediate: true },
)

// A route change is a new page: put it at the top, and close the drawer the
// user just navigated from.
watch(
  () => route.path,
  () => {
    resetScroll()
    drawerOpen.value = false
  },
)

/**
 * Open, the drawer covers the page behind a scrim — that makes it modal, and
 * modal surfaces owe the same three things a dialog does: focus moves in,
 * Tab stays in, and focus returns to the trigger on close. Without the trap,
 * Tab walks invisibly through the page underneath.
 */
watch(drawerOpen, async (open) => {
  if (!isDrawerLayout()) return
  if (!open) {
    menuButton.value?.focus()
    return
  }
  await nextTick()
  sidebar.value?.querySelector<HTMLElement>(FOCUSABLE)?.focus()
})

const FOCUSABLE = 'a[href], button:not([disabled]), [tabindex]:not([tabindex="-1"])'

/** Matches the 1080px breakpoint in this component's own stylesheet. */
function isDrawerLayout(): boolean {
  return typeof matchMedia !== "undefined" && matchMedia("(max-width: 1080px)").matches
}

function onKeydown(event: KeyboardEvent) {
  // `drawerOpen` is only meaningful below the shell breakpoint — above it the
  // sidebar is a static column, and trapping focus in it would strand the
  // user in the nav. The flag can be left true from a resize, so check the
  // layout rather than trusting it alone.
  if (!drawerOpen.value || !isDrawerLayout()) return

  if (event.key === "Escape") {
    drawerOpen.value = false
    return
  }
  if (event.key !== "Tab" || !sidebar.value) return

  const items = [...sidebar.value.querySelectorAll<HTMLElement>(FOCUSABLE)]
  if (!items.length) return
  const first = items[0]!
  const last = items[items.length - 1]!
  const active = document.activeElement

  // Also catches focus already outside the drawer (a click on the scrim, say),
  // pulling it back rather than letting Tab continue through the page behind.
  if (event.shiftKey && (active === first || !sidebar.value.contains(active))) {
    event.preventDefault()
    last.focus()
  } else if (!event.shiftKey && (active === last || !sidebar.value.contains(active))) {
    event.preventDefault()
    first.focus()
  }
}

onMounted(() => {
  setScrollRegion(region.value)
  window.addEventListener("keydown", onKeydown)
})

onBeforeUnmount(() => {
  setScrollRegion(null)
  window.removeEventListener("keydown", onKeydown)
})

async function onSignOut() {
  await logout()
  await router.push({ name: "login" })
}
</script>

<template>
  <div class="shell">
    <a class="skip-link" href="#content">{{ t("app.skipToContent") }}</a>

    <!-- Scrim sits under the drawer and above the content; clicking it closes. -->
    <div v-if="drawerOpen" class="scrim" @click="drawerOpen = false" />

    <aside ref="sidebar" class="sidebar" :class="{ open: drawerOpen }">
      <div class="sidebar-head">
        <RouterLink to="/overview" class="brand">
          <span class="brand-mark" aria-hidden="true">k</span>
          <span class="brand-name">{{ SITE.name }}</span>
        </RouterLink>
        <button
          type="button"
          class="drawer-close"
          :aria-label="t('app.closeMenu')"
          @click="drawerOpen = false"
        >
          <svg viewBox="0 0 16 16" fill="none" aria-hidden="true">
            <path d="M4 4l8 8M12 4l-8 8" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
          </svg>
        </button>
      </div>

      <nav class="nav" :aria-label="t('app.primaryNav')">
        <RouterLink
          v-for="item in NAV"
          :key="item.name"
          :to="item.to"
          class="nav-item"
          active-class="active"
        >
          <NavIcon :name="item.name" />
          <span class="nav-label">{{ t(item.label) }}</span>
        </RouterLink>
      </nav>

      <!-- mt-auto: pinned to the bottom of the scroll area, above the user block. -->
      <div class="sidebar-secondary">
        <RouterLink to="/changelog" class="nav-item subtle" active-class="active">
          <NavIcon name="changelog" />
          <span class="nav-label">{{ t("nav.changelog") }}</span>
          <span v-if="version" class="version">
            {{ version }}
            <span v-if="updateAvailable" class="update-dot" aria-hidden="true" />
            <span v-if="updateAvailable" class="sr-only">{{ t("app.updateAvailable") }}</span>
          </span>
        </RouterLink>
      </div>

      <div class="sidebar-foot">
        <div class="user">
          <img
            v-if="user?.picture_url"
            :src="user.picture_url"
            alt=""
            class="avatar"
            referrerpolicy="no-referrer"
          />
          <span v-else class="avatar avatar-fallback" aria-hidden="true">
            {{ (userLabel[0] ?? "?").toUpperCase() }}
          </span>
          <span class="user-label" :title="userLabel">{{ userLabel }}</span>
          <button
            type="button"
            class="sign-out"
            :aria-label="t('app.signOut')"
            :title="t('app.signOut')"
            @click="onSignOut"
          >
            <svg viewBox="0 0 16 16" fill="none" aria-hidden="true">
              <path
                d="M6 14H3.5A1.5 1.5 0 012 12.5v-9A1.5 1.5 0 013.5 2H6M10.5 11L14 8l-3.5-3M14 8H6"
                stroke="currentColor"
                stroke-width="1.4"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
            </svg>
          </button>
        </div>
      </div>
    </aside>

    <div class="frame">
      <header class="mobile-bar">
        <button
          ref="menuButton"
          type="button"
          class="menu-button"
          :aria-label="t('app.openMenu')"
          :aria-expanded="drawerOpen"
          @click="drawerOpen = true"
        >
          <svg viewBox="0 0 16 16" fill="none" aria-hidden="true">
            <path d="M2 4h12M2 8h12M2 12h12" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
          </svg>
        </button>
        <span class="mobile-brand">
          <span class="brand-mark" aria-hidden="true">k</span>
          {{ SITE.name }}
        </span>
      </header>

      <main id="content" ref="region" class="content">
        <div class="content-inner">
          <slot />
        </div>
      </main>
    </div>
  </div>
</template>

<style scoped>
.shell {
  display: grid;
  grid-template-columns: var(--sidebar-width) minmax(0, 1fr);
  height: 100dvh;
  overflow: hidden;
  background: var(--bg);
}

.skip-link {
  position: absolute;
  top: var(--space-2);
  left: var(--space-2);
  z-index: 80;
  padding: var(--space-2) var(--space-3);
  border-radius: var(--radius-sm);
  background: var(--accent);
  color: var(--accent-fg);
  font-size: var(--text-sm);
  font-weight: var(--weight-medium);
  transform: translateY(-200%);
}

.skip-link:focus {
  transform: none;
}

/* --- Sidebar ----------------------------------------------------------- */

.sidebar {
  display: flex;
  flex-direction: column;
  min-height: 0;
  padding: var(--space-3);
  gap: var(--space-1);
  background: var(--surface);
  border-right: 1px solid var(--border);
}

.sidebar-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  height: var(--header-height);
  padding: 0 var(--space-2);
  margin-bottom: var(--space-2);
}

.brand {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  min-width: 0;
  font-weight: var(--weight-semibold);
  letter-spacing: var(--tracking-tight);
}

.brand-mark {
  display: inline-grid;
  place-items: center;
  width: 24px;
  height: 24px;
  flex-shrink: 0;
  border-radius: var(--radius-sm);
  background: var(--accent);
  color: var(--accent-fg);
  font-size: var(--text-xs);
  font-weight: var(--weight-bold);
}

.brand-name {
  font-size: var(--text-sm);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.drawer-close {
  display: none;
}

.nav {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-height: 0;
  overflow-y: auto;
  /* As a drawer this sits over the content region; scrolling it to its end
     must not chain through to the page behind. */
  overscroll-behavior: contain;
}

.nav-item {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  height: 34px;
  padding: 0 var(--space-2);
  border-radius: var(--radius-sm);
  color: var(--muted);
  font-size: var(--text-sm);
  font-weight: var(--weight-normal);
  overflow: hidden;
  transition:
    background var(--duration-fast) var(--ease),
    color var(--duration-fast) var(--ease);
}

.nav-item:hover {
  background: var(--hover);
  color: var(--text);
}

/* Active is a filled pill *and* a weight shift — the weight carries as much
   of the signal as the fill, which is what keeps a ~2% luminance delta
   readable. */
.nav-item.active {
  background: var(--hover);
  color: var(--text);
  font-weight: var(--weight-medium);
}

.nav-item :deep(svg) {
  width: 16px;
  height: 16px;
  flex-shrink: 0;
  pointer-events: none;
}

.nav-label {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.sidebar-secondary {
  margin-top: auto;
  padding-top: var(--space-2);
}

.nav-item.subtle {
  color: var(--faint);
}

.nav-item.subtle:hover,
.nav-item.subtle.active {
  color: var(--text-secondary);
}

.version {
  display: inline-flex;
  align-items: center;
  gap: var(--space-1);
  margin-left: auto;
  padding-left: var(--space-2);
  font-family: var(--mono);
  font-size: var(--text-2xs);
  color: var(--faint);
}

/* Never color-only: the dot is paired with an sr-only label. */
.update-dot {
  width: 5px;
  height: 5px;
  border-radius: var(--radius-full);
  background: var(--chart-input);
}

.sidebar-foot {
  padding-top: var(--space-2);
  margin-top: var(--space-1);
  border-top: 1px solid var(--border);
}

.user {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  padding: var(--space-2);
  border-radius: var(--radius-sm);
}

.avatar {
  width: 26px;
  height: 26px;
  flex-shrink: 0;
  border-radius: var(--radius-sm);
  object-fit: cover;
  border: 1px solid var(--border);
  /* Keeps a colorful profile photo from being the loudest thing in the chrome. */
  filter: grayscale(1);
}

.avatar-fallback {
  display: grid;
  place-items: center;
  background: var(--surface-2);
  color: var(--muted);
  font-size: var(--text-xs);
  font-weight: var(--weight-semibold);
  filter: none;
}

.user-label {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text-secondary);
  font-size: var(--text-xs);
}

.sign-out {
  display: grid;
  place-items: center;
  width: 26px;
  height: 26px;
  flex-shrink: 0;
  border: none;
  border-radius: var(--radius-sm);
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  transition:
    background var(--duration-fast) var(--ease),
    color var(--duration-fast) var(--ease);
}

.sign-out:hover {
  background: var(--hover);
  color: var(--text);
}

.sign-out svg {
  width: 15px;
  height: 15px;
}

/* --- Content ----------------------------------------------------------- */

.frame {
  display: flex;
  flex-direction: column;
  min-width: 0;
  min-height: 0;
}

.mobile-bar {
  display: none;
}

/* The one scrolling element in the signed-in app. `min-height: 0` is what
   lets it actually shrink inside the flex parent — without it the region
   grows and the scrollbar never appears.

   The gutter is reserved permanently: pages differ in height, and without it
   the content column slides sideways by the scrollbar width on every
   navigation between one that scrolls and one that does not. */
.content {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  overscroll-behavior: contain;
  scrollbar-gutter: stable;
}

/**
 * The page gutter is published as a custom property rather than only applied
 * as padding: PageHeader has to cancel it with a negative margin so its blur
 * reaches the region's edges, and a second hardcoded copy of these values
 * would silently disagree at one breakpoint. Inheritance makes this the single
 * source of truth for both.
 */
.content-inner {
  --page-gutter: var(--space-8);
  --page-top: var(--space-6);
  /* The safe-area inset lives *inside* this value rather than being applied
     separately, so every consumer stays correct without knowing about it: the
     padding below, and the pages that size themselves to the viewport by
     subtracting it. `viewport-fit=cover` puts the page under the home
     indicator, so the frame keeps its full height — shrinking it instead would
     leave a permanent strip of bare `body` under the app — and this bottom
     gutter is what keeps content clear of the indicator. */
  --page-bottom: calc(var(--space-12) + env(safe-area-inset-bottom, 0px));
  /* Height the shell's own chrome takes out of the viewport above this
     region. Zero on desktop; the mobile bar below the shell breakpoint. A page
     sizing itself to the viewport subtracts this rather than hardcoding a
     breakpoint-dependent guess. */
  --page-chrome: 0px;

  max-width: var(--content-max);
  margin: 0 auto;
  padding: var(--page-top) var(--page-gutter) var(--page-bottom);
}

/* --- Responsive --------------------------------------------------------- */

@media (max-width: 1200px) {
  .content-inner {
    --page-gutter: var(--space-6);
  }
}

@media (max-width: 1080px) {
  .shell {
    grid-template-columns: minmax(0, 1fr);
  }

  /* The mobile bar appears at this breakpoint and eats into the height a
     viewport-sized page can claim. */
  .content-inner {
    --page-chrome: var(--header-height);
  }

  .scrim {
    position: fixed;
    inset: 0;
    z-index: 40;
    background: var(--overlay);
    overscroll-behavior: contain;
    animation: fade var(--duration) var(--ease-enter);
  }

  .sidebar {
    position: fixed;
    inset: 0 auto 0 0;
    z-index: 50;
    width: var(--sidebar-width);
    transform: translateX(-100%);
    box-shadow: var(--shadow-lg);
    /* Off-screen *and* inert: a translated-away drawer is still focusable, so
       Tab would walk into a menu nobody can see. `visibility` fixes that, but
       it is not interpolable — applied plainly it would snap to hidden on the
       first frame and eat the slide-out. Hence the `0s` step delayed by the
       transform's own duration on close, and undelayed on open. */
    visibility: hidden;
    transition:
      transform var(--duration) var(--ease-exit),
      visibility 0s linear var(--duration);
  }

  .sidebar.open {
    transform: none;
    visibility: visible;
    transition:
      transform var(--duration-slow) var(--ease-enter),
      visibility 0s;
  }

  .drawer-close {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    border: none;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--muted);
    cursor: pointer;
  }

  .drawer-close:hover {
    background: var(--hover);
    color: var(--text);
  }

  .drawer-close svg {
    width: 16px;
    height: 16px;
  }

  .mobile-bar {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    height: var(--header-height);
    flex-shrink: 0;
    padding: 0 var(--space-4);
    background: var(--surface);
    border-bottom: 1px solid var(--border);
  }

  .menu-button {
    display: grid;
    place-items: center;
    width: 36px;
    height: 36px;
    margin-left: calc(var(--space-2) * -1);
    border: none;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text-secondary);
    cursor: pointer;
  }

  .menu-button:hover {
    background: var(--hover);
    color: var(--text);
  }

  .menu-button svg {
    width: 18px;
    height: 18px;
  }

  .mobile-brand {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--text-sm);
    font-weight: var(--weight-semibold);
    letter-spacing: var(--tracking-tight);
  }

  .nav-item,
  .user {
    height: 40px;
  }

  @keyframes fade {
    from {
      opacity: 0;
    }
  }
}

@media (max-width: 640px) {
  .content-inner {
    --page-gutter: var(--space-4);
    --page-top: var(--space-4);
    /* Keeps the safe-area inset — this is the breakpoint that actually has
       one, so dropping it here would undo the whole thing. */
    --page-bottom: calc(var(--space-10) + env(safe-area-inset-bottom, 0px));
  }
}
</style>
