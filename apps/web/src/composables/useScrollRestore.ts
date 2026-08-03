/**
 * Scroll persistence for one page.
 *
 * The shell is a fixed frame: only its content region scrolls, so `window`
 * never moves and `window.scrollY` is always 0. Everything here therefore
 * targets that element, published by AppShell through
 * `services/scrollRegion.ts`.
 *
 * Restore also needs a later moment than mount. A data-driven page is a few
 * hundred pixels tall until its fetch resolves, so applying the offset on
 * mount would clamp to the top and silently lose the position — hence
 * `markReady()`, which the page calls once its content has painted. See
 * docs/admin-ui.md § View preferences.
 */

import { nextTick, onBeforeUnmount, onMounted, ref } from "vue"
import { useRoute } from "vue-router"
import { getScroll, setScroll } from "@/services/prefs"
import { getScrollRegion } from "@/services/scrollRegion"

export function useScrollRestore() {
  const route = useRoute()
  const path = route.path
  /** Set once the offset has been applied — or abandoned. Restore is a one-shot. */
  const settled = ref(false)
  let saveTimer: number | null = null
  let region: HTMLElement | null = null

  function persist() {
    if (saveTimer !== null) return
    // Coalesce a scroll burst into one write — every wheel tick otherwise
    // means a read-modify-write of the whole prefs blob.
    saveTimer = window.setTimeout(() => {
      saveTimer = null
      if (region) setScroll(path, region.scrollTop)
    }, 250)
  }

  /**
   * A user scroll before the restore lands means they have already chosen a
   * position; honour it instead of yanking the page out from under them.
   */
  function onUserScroll() {
    if (!settled.value) settled.value = true
    persist()
  }

  onMounted(() => {
    region = getScrollRegion()
    region?.addEventListener("scroll", onUserScroll, { passive: true })
  })

  onBeforeUnmount(() => {
    region?.removeEventListener("scroll", onUserScroll)
    if (saveTimer !== null) {
      window.clearTimeout(saveTimer)
      // Flush rather than drop: leaving the page is exactly when the last
      // position matters.
      if (region) setScroll(path, region.scrollTop)
    }
  })

  /**
   * Call once the page's own content has rendered (data loaded, cards laid
   * out). Applies the saved offset if the user has not already scrolled and
   * the region is genuinely tall enough for it.
   */
  async function markReady() {
    if (settled.value) return
    const offset = getScroll(path)
    settled.value = true
    if (offset <= 0) return
    await nextTick()
    const el = region ?? getScrollRegion()
    if (!el) return
    // Ignore a restore for a page that never got tall enough to hold it.
    if (el.scrollHeight - el.clientHeight >= offset - 1) el.scrollTop = offset
  }

  return { markReady }
}
