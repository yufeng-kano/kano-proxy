/// <reference types="vite/client" />

/** Merges with Vite's built-in env keys (MODE, DEV, PROD, …). */
interface ImportMetaEnv {
  /** Origin of the Worker API. Unset/empty = same-origin (production deploy). */
  readonly VITE_API_ORIGIN?: string
  /** Contact address shown in the public footer. */
  readonly VITE_CONTACT_EMAIL?: string
}

declare module "*.vue" {
  import type { DefineComponent } from "vue"
  const component: DefineComponent<object, object, unknown>
  export default component
}
