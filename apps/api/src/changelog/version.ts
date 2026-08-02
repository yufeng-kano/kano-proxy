/**
 * SemVer comparison for the running-version badge.
 *
 * Pure — no Worker APIs — so the update-available rules stay unit-testable.
 */

/** `MAJOR.MINOR.PATCH` with an optional leading `v`; anything else is not a version. */
export function parseSemver(v: string): [number, number, number] | null {
  const m = /^v?(\d+)\.(\d+)\.(\d+)$/.exec(v.trim())
  if (!m) return null
  return [Number(m[1]), Number(m[2]), Number(m[3])]
}

/**
 * Numeric per-component compare, so `1.10.0` sorts above `1.9.0` (a string
 * compare would get that backwards). Unparseable input compares equal — the
 * caller cannot act on an ordering it can't trust, and `isUpdateAvailable`
 * turns that into "no update".
 */
export function compareSemver(a: string, b: string): number {
  const pa = parseSemver(a)
  const pb = parseSemver(b)
  if (!pa || !pb) return 0
  for (let i = 0; i < 3; i++) {
    if (pa[i] !== pb[i]) return pa[i] < pb[i] ? -1 : 1
  }
  return 0
}

/**
 * True only when `current` is strictly behind `latest`.
 *
 * A local version *ahead* of the newest release is the normal state between a
 * version bump and its release, so it must not read as "update available".
 */
export function isUpdateAvailable(
  current: string,
  latest: string | null,
): boolean {
  if (!latest) return false
  if (!parseSemver(current) || !parseSemver(latest)) return false
  return compareSemver(current, latest) < 0
}
