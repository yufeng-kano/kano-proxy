/**
 * Site chrome for unauthenticated surfaces (login footer, etc.).
 *
 * The contact address is deploy-specific and comes only from
 * VITE_CONTACT_EMAIL (`.env.development` locally, `.env.production` or a Pages
 * build-environment variable for deploys). There is deliberately no fallback
 * address: a stale hardcoded default would ship a wrong contact to real users,
 * so an unset variable renders no link at all.
 */
export const SITE = {
  /**
   * Display brand for every user-facing surface (login wordmark, footer
   * copyright, topbar). `index.html` repeats it in <title> because the static
   * shell renders before the app boots — keep the two in sync when renaming.
   *
   * Not a rename target: the `sk-kano-proxy-` API key prefix and the
   * `kano-proxy:*` sessionStorage keys are wire/storage identifiers.
   */
  name: "Kano Proxy",
  /** Empty when VITE_CONTACT_EMAIL is unset — callers must omit the link. */
  contactEmail: import.meta.env.VITE_CONTACT_EMAIL?.trim() ?? "",
} as const
