/**
 * Number formatting shared by the Dashboard's stat tiles, chart, and
 * per-model table (see docs/admin-ui.md Dashboard page) so all three read
 * consistently. Every function is null-safe: `request_logs` token fields are
 * nullable ("unreported", not zero — see docs/database.md), and cache_rate is
 * null when no row in range is cache-known.
 */

const COMPACT_UNITS: [number, string][] = [
  [1_000_000_000, "B"],
  [1_000_000, "M"],
  [1_000, "K"],
]

/** Compact magnitude for large counts, e.g. 12300 -> "12.3K", 4560000 -> "4.56M". Values under 1000 render exactly. */
export function formatCompactNumber(n: number | null | undefined): string {
  if (n == null || Number.isNaN(n)) return "—"
  const abs = Math.abs(n)
  for (const [threshold, suffix] of COMPACT_UNITS) {
    if (abs >= threshold) {
      const value = n / threshold
      const digits = value < 10 ? 2 : value < 100 ? 1 : 0
      return `${value.toFixed(digits)}${suffix}`
    }
  }
  return String(Math.round(n))
}

/** Exact integer with thousands separators, for counts where precision matters (requests, errors). */
export function formatInt(n: number | null | undefined): string {
  if (n == null || Number.isNaN(n)) return "—"
  return Math.round(n).toLocaleString()
}

/** Ratio (0..1) as a percent with 1 decimal, e.g. 0.734 -> "73.4%". */
export function formatPercent1(ratio: number | null | undefined): string {
  if (ratio == null || Number.isNaN(ratio)) return "—"
  return `${(ratio * 100).toFixed(1)}%`
}

/** Whole-millisecond latency, e.g. 842 -> "842 ms". */
export function formatLatencyMs(ms: number | null | undefined): string {
  if (ms == null || Number.isNaN(ms)) return "—"
  return `${Math.round(ms)} ms`
}
