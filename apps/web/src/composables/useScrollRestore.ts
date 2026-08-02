/**
 * Cross-session scroll restore for one page.
 *
 * The router's own `scrollBehavior` covers in-session back/forward. This
 * covers the other case: the user closes the tab mid-page and comes back
 * later. That needs the offset in localStorage (services/prefs.ts) *and* a
 * later restore moment — a data-driven page is a few hundred pixels tall
 * until its fetch resolves, so scrolling on mount would clamp to the top and
 * silently lose the position.
 *
 * Hence `markReady()`: the page calls it once its content has painted, and
 * only then is the offset applied. See docs/admin-ui.md § View preferences.
 */

import { nextTick, onBeforeUnmount, onMounted, ref } from "vue"
import { useRoute } from "vue-router"
import { getScroll, setScroll } from "@/services/prefs"

/** Ignore restores for a page that never actually got tall enough to hold the offset. */
function canScrollTo(offset: number): boolean {
  return document.documentElement.scrollHeight - window.innerHeight >= offset - 1
}

export function useScrollRestore() {
  const route = useRoute()
  const path = route.path
  /** Set once the offset has been applied — or abandoned. Restore is a one-shot. */
  const settled = ref(false)
  let saveTimer: number | null = null

  function persist() {
    if (saveTimer !== null) return
    // Coalesce a scroll burst into one write — every wheel tick otherwise
    // means a read-modify-write of the whole prefs blob.
    saveTimer = window.setTimeout(() => {
      saveTimer = null
      setScroll(path, window.scrollY)
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
    window.addEventListener("scroll", onUserScroll, { passive: true })
  })

  onBeforeUnmount(() => {
    window.removeEventListener("scroll", onUserScroll)
    if (saveTimer !== null) {
      window.clearTimeout(saveTimer)
      // Flush rather than drop: leaving the page is exactly when the last
      // position matters.
      setScroll(path, window.scrollY)
    }
  })

  /**
   * Call once the page's own content has rendered (data loaded, cards laid
   * out). Applies the saved offset if the user has not already scrolled and
   * the document is genuinely tall enough for it.
   */
  async function markReady() {
    if (settled.value) return
    const offset = getScroll(path)
    settled.value = true
    if (offset <= 0) return
    await nextTick()
    if (canScrollTo(offset)) window.scrollTo({ top: offset, behavior: "auto" })
  }

  return { markReady }
}
