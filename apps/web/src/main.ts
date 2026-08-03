import { createApp } from "vue"
import App from "./App.vue"
import { detectLocale, setLocale } from "./i18n"
import router from "./router"
import "./styles.css"

// Before mount, so the first paint is already in the right language and
// <html lang> is correct for assistive tech and the browser's own UI.
setLocale(detectLocale())

createApp(App).use(router).mount("#app")
