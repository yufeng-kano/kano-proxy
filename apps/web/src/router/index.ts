import { createRouter, createWebHistory } from "vue-router"
import { takeLoginRedirect, useAuth } from "@/composables/useAuth"
import { readPrefs, setLastPath } from "@/services/prefs"
import { SITE } from "@/config/site"
import { useI18n, type MessageKey } from "@/i18n"

/**
 * Paths the app itself lands on rather than the user choosing them — a
 * restore onto one of these would be a no-op at best and a redirect loop at
 * worst, so they are never recorded as "where the user was". The legacy
 * aliases are here too: restoring `/accounts` would only bounce to
 * `/providers`, so the canonical path is what gets stored.
 */
// /cli/authorize carries its request id in the query, which lastPath does not
// keep — a restore onto the bare path could only show "request missing".
const NON_RESTORABLE = new Set(["/", "/login", "/dashboard", "/accounts", "/cli", "/cli/authorize"])

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
      meta: { public: true, titleKey: "login.signIn" },
    },
    {
      path: "/overview",
      name: "overview",
      component: () => import("@/pages/OverviewPage.vue"),
      meta: { titleKey: "nav.overview" },
    },
    {
      path: "/logs",
      name: "logs",
      component: () => import("@/pages/LogsPage.vue"),
      meta: { titleKey: "nav.logs" },
    },
    {
      path: "/providers",
      name: "providers",
      component: () => import("@/pages/ProvidersPage.vue"),
      meta: { titleKey: "nav.providers" },
    },
    {
      path: "/groups",
      name: "groups",
      component: () => import("@/pages/GroupsPage.vue"),
      meta: { titleKey: "nav.groups" },
    },
    {
      // The authorize view a `kano-proxy init` login lands on (docs/cli.md).
      // Session-gated like every non-public route — the guard redirects
      // through login and back, query string intact. `bare`: rendered outside
      // the shell — a blank page with one centered card (docs/admin-ui.md
      // § CLI authorize view).
      path: "/cli/authorize",
      name: "cli-authorize",
      component: () => import("@/pages/CliAuthorizePage.vue"),
      meta: { bare: true, titleKey: "cli.authorize.title" },
    },
    {
      path: "/keys",
      name: "keys",
      component: () => import("@/pages/KeysPage.vue"),
      meta: { titleKey: "nav.keys" },
    },
    {
      path: "/models",
      name: "models",
      component: () => import("@/pages/ModelsPage.vue"),
      meta: { titleKey: "nav.models" },
    },
    {
      path: "/changelog",
      name: "changelog",
      component: () => import("@/pages/ChangelogPage.vue"),
      meta: { titleKey: "nav.changelog" },
    },
    // Pre-2.0 paths. A bookmark or a persisted last-route from an older build
    // must land on the renamed page, not fall through the catch-all.
    { path: "/dashboard", redirect: "/overview" },
    { path: "/accounts", redirect: "/providers" },
    // The CLI page merged into Providers (docs/admin-ui.md § CLI sections).
    { path: "/cli", redirect: "/providers" },
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

  // A sign-in that started from a guarded deep link (e.g. the CLI authorize
  // view) comes back through the OAuth callback on the bare root — finish the
  // journey the guard started. Checked before the lastPath restore so the
  // deliberate destination outranks the habitual one; the auth guard above
  // already ran, so this can never skip login.
  if (isBoot && entryPath === "/") {
    const stashed = takeLoginRedirect()
    if (stashed && stashed !== to.fullPath && router.resolve(stashed).matched.length > 0) {
      return stashed
    }
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
  // "<page> · <site>" so tabs and history are readable; the static <title> in
  // index.html is only the pre-boot value (docs/admin-ui.md § Pages).
  const key = to.meta.titleKey as MessageKey | undefined
  document.title = key ? `${useI18n().t(key)} · ${SITE.name}` : SITE.name
})

export default router
