import { createApp } from "vue"
import App from "./App.vue"
import { detectLocale, setLocale } from "./i18n"
import { createAppRouter } from "./router"
import { webExtensionsKey, type WebExtensions } from "./extensions"
import "./styles.css"

/** Creates an unmounted app so editions can compose before mounting. */
export function createWebApp(extensions: WebExtensions = {}) {
  setLocale(detectLocale())
  const router = createAppRouter(extensions.routes)
  const app = createApp(App).provide(webExtensionsKey, extensions).use(router)
  return { app, router }
}
