/**
 * Confirm-first, in the app's own dialog (docs/admin-ui.md § Component
 * primitives) — the replacement for `window.confirm()`, whose box is titled
 * with the hostname, ignores the theme and labels its buttons in the
 * browser's language.
 *
 * One request at a time, rendered by the single `ConfirmDialog` that
 * `App.vue` mounts. A new request cancels the one still open, so a caller
 * awaiting it always gets an answer.
 */
import { shallowRef } from "vue"

export interface ConfirmOptions {
  /** The question: "Revoke this key?" */
  title: string
  /** The consequence, without repeating the question. */
  message: string
  /** The verb the confirm button performs — never "OK". */
  confirmLabel: string
  /** `danger` (the default) fires something irreversible. */
  tone?: "danger" | "default"
}

interface Pending extends ConfirmOptions {
  resolve: (confirmed: boolean) => void
}

/** The open request, read by `ConfirmDialog`. */
export const pendingConfirm = shallowRef<Pending | null>(null)

export function settleConfirm(confirmed: boolean) {
  const pending = pendingConfirm.value
  if (!pending) return
  pendingConfirm.value = null
  pending.resolve(confirmed)
}

function confirm(options: ConfirmOptions): Promise<boolean> {
  settleConfirm(false)
  return new Promise((resolve) => {
    pendingConfirm.value = { ...options, resolve }
  })
}

export function useConfirm() {
  return confirm
}
