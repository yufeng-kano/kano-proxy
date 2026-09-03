import DefaultTheme from "vitepress/theme"
import type { Theme } from "vitepress"
import { installOriginFill } from "./fill-origin"

export default {
  extends: DefaultTheme,
  enhanceApp({ router }) {
    installOriginFill(router)
  },
} satisfies Theme
