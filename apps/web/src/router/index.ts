import { createRouter, createWebHistory } from "vue-router"
import { useAuth } from "@/composables/useAuth"
import { readPrefs, setLastPath } from "@/services/prefs"

/**
 * Paths the app itself lands on rather than the user choosing them — a
 * restore onto one of these would be a no-op at best and a redirect loop at
 * worst, so they are never recorded as "where the user was". The legacy
 * aliases are here too: restoring `/accounts` would only bounce to
 * `/providers`, so the canonical path is what gets stored.
 */
// /cli/authorize carries its request id in the query, which lastPath does not
// keep — a restore onto the bare path could only show "request missing".
const NON_RESTORABLE = new Set(["/", "/login", "/dashboard", "/accounts", "/cli/authorize"])

const router = createRouter({
  history: createWebHistory(),
  /**
   * The shell's content region is the scroll container, not the document, so
   * the router's own scroll handling has nothing to move here. In-session
   * back/forward and the cross-session restore are both handled by
   * useScrollRestore against that element.
   */
  scrollBehavior() {
    return false
  },
  routes: [
    {
      path: "/",
      redirect: "/overview",
    },
    {
      path: "/login",
      name: "login",
      component: () => import("@/pages/LoginPage.vue"),
      meta: { public: true },
    },
    {
      path: "/overview",
      name: "overview",
      component: () => import("@/pages/OverviewPage.vue"),
    },
    {
      path: "/logs",
      name: "logs",
      component: () => import("@/pages/LogsPage.vue"),
    },
    {
      path: "/providers",
      name: "providers",
      component: () => import("@/pages/ProvidersPage.vue"),
    },
    {
      path: "/groups",
      name: "groups",
      component: () => import("@/pages/GroupsPage.vue"),
    },
    {
      path: "/cli",
      name: "cli",
      component: () => import("@/pages/CliPage.vue"),
    },
    {
      // The authorize view a `kano-proxy init` login lands on (docs/cli.md).
      // Session-gated like every non-public route — the guard redirects
      // through login and back, query string intact.
      path: "/cli/authorize",
      name: "cli-authorize",
      component: () => import("@/pages/CliAuthorizePage.vue"),
    },
    {
      path: "/keys",
      name: "keys",
      component: () => import("@/pages/KeysPage.vue"),
    },
    {
      path: "/models",
      name: "models",
      component: () => import("@/pages/ModelsPage.vue"),
    },
    {
      path: "/changelog",
      name: "changelog",
      component: () => import("@/pages/ChangelogPage.vue"),
    },
    // Pre-2.0 paths. A bookmark or a persisted last-route from an older build
    // must land on the renamed page, not fall through the catch-all.
    { path: "/dashboard", redirect: "/overview" },
    { path: "/accounts", redirect: "/providers" },
    {
      path: "/:pathMatch(.*)*",
      redirect: "/overview",
    },
  ],
})

/**
 * True only for the very first navigation of a page load — the one moment a
 * persisted route may be replayed. Every later navigation is the user's own
 * click and must be left alone.
 */
let isBootNavigation = true

/**
 * The URL the browser actually loaded, captured before any redirect rewrites
 * it. The guard below can't read this off `to`: "/" redirects to /overview
 * *before* the guard runs, so by then a bare "/" and an explicit "/overview"
 * look identical — and replaying a stored route over an explicit /overview
 * would mean the user can never navigate there.
 */
const entryPath = typeof window === "undefined" ? "/" : window.location.pathname

router.beforeEach(async (to) => {
  const { ensureSession, isAuthenticated } = useAuth()
  await ensureSession()
  const isBoot = isBootNavigation
  isBootNavigation = false

  if (to.meta.public) {
    if (isAuthenticated.value && to.name === "login") {
      return { name: "overview" }
    }
    return true
  }

  if (!isAuthenticated.value) {
    return { name: "login", query: { redirect: to.fullPath } }
  }

  // Restore the last visited page — but only when the browser landed on the
  // bare app root, never when the user typed or bookmarked a specific URL
  // (including /overview itself). Runs after the auth check above, so a
  // persisted path can't skip login; and `resolve().matched` keeps a route
  // deleted since it was stored from 404ing the boot.
  if (isBoot && entryPath === "/" && !to.query.redirect) {
    const saved = readPrefs().lastPath
    if (
      saved &&
      saved !== to.path &&
      !NON_RESTORABLE.has(saved) &&
      // A route deleted since the pref was written would otherwise fall
      // through the catch-all and bounce straight back here.
      router.resolve(saved).matched.length > 0
    ) {
      return saved
    }
  }

  return true
})

router.afterEach((to) => {
  if (!NON_RESTORABLE.has(to.path)) setLastPath(to.path)
})

export default router
