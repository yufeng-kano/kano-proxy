import { createRouter, createWebHistory } from "vue-router"
import { useAuth } from "@/composables/useAuth"
import { readPrefs, setLastPath } from "@/services/prefs"

/**
 * Paths the app itself lands on rather than the user choosing them — a
 * restore onto one of these would be a no-op at best and a redirect loop at
 * worst, so they are never recorded as "where the user was".
 */
const NON_RESTORABLE = new Set(["/", "/login"])

const router = createRouter({
  history: createWebHistory(),
  /**
   * Back/forward keeps the browser's own remembered offset; a fresh
   * navigation starts at the top. The cross-session restore is a separate
   * mechanism (see the afterEach below and useScrollRestore) — it only
   * applies to the boot-time landing, and only after that page's data has
   * painted, since the document is too short to scroll before then.
   */
  scrollBehavior(_to, _from, savedPosition) {
    return savedPosition ?? { top: 0 }
  },
  routes: [
    {
      path: "/",
      redirect: "/dashboard",
    },
    {
      path: "/login",
      name: "login",
      component: () => import("@/pages/LoginPage.vue"),
      meta: { public: true },
    },
    {
      path: "/dashboard",
      name: "dashboard",
      component: () => import("@/pages/DashboardPage.vue"),
    },
    {
      path: "/accounts",
      name: "accounts",
      component: () => import("@/pages/AccountsPage.vue"),
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
    {
      path: "/:pathMatch(.*)*",
      redirect: "/dashboard",
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
 * it. The guard below can't read this off `to`: "/" redirects to /dashboard
 * *before* the guard runs, so by then a bare "/" and an explicit
 * "/dashboard" look identical — and replaying a stored route over an
 * explicit /dashboard would mean the user can never navigate there.
 */
const entryPath = typeof window === "undefined" ? "/" : window.location.pathname

router.beforeEach(async (to) => {
  const { ensureSession, isAuthenticated } = useAuth()
  await ensureSession()
  const isBoot = isBootNavigation
  isBootNavigation = false

  if (to.meta.public) {
    if (isAuthenticated.value && to.name === "login") {
      return { name: "accounts" }
    }
    return true
  }

  if (!isAuthenticated.value) {
    return { name: "login", query: { redirect: to.fullPath } }
  }

  // Restore the last visited page — but only when the browser landed on the
  // bare app root, never when the user typed or bookmarked a specific URL
  // (including /dashboard itself). Runs after the auth check above, so a
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
