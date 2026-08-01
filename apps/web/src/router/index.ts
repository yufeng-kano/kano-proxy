import { createRouter, createWebHistory } from "vue-router"
import { useAuth } from "@/composables/useAuth"

const router = createRouter({
  history: createWebHistory(),
  routes: [
    {
      path: "/",
      redirect: "/accounts",
    },
    {
      path: "/login",
      name: "login",
      component: () => import("@/pages/LoginPage.vue"),
      meta: { public: true },
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
      path: "/:pathMatch(.*)*",
      redirect: "/accounts",
    },
  ],
})

router.beforeEach(async (to) => {
  const { ensureSession, isAuthenticated } = useAuth()
  await ensureSession()

  if (to.meta.public) {
    if (isAuthenticated.value && to.name === "login") {
      return { name: "accounts" }
    }
    return true
  }

  if (!isAuthenticated.value) {
    return { name: "login", query: { redirect: to.fullPath } }
  }

  return true
})

export default router
