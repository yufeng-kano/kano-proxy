import type { UsageRange, UsageRangeKind } from "@/types"

export function pad2(n: number): string {
  return String(n).padStart(2, "0")
}

/** Formats a Date as YYYY-MM-DD in local time. */
export function formatLocalDate(d: Date): string {
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`
}

/** Formats a Date as YYYY-MM in local time. */
export function formatLocalMonth(d: Date): string {
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}`
}

/** Returns the local start of day (00:00:00.000). */
export function getStartOfDay(d: Date): Date {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate(), 0, 0, 0, 0)
}

/** Returns the local end of day (23:59:59.999). */
export function getEndOfDay(d: Date): Date {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate(), 23, 59, 59, 999)
}

/** Returns the local start of week (Monday 00:00:00.000). */
export function getStartOfWeek(d: Date): Date {
  const day = d.getDay() // 0 = Sun, 1 = Mon, ... 6 = Sat
  const diff = (day + 6) % 7 // days since Monday
  return new Date(d.getFullYear(), d.getMonth(), d.getDate() - diff, 0, 0, 0, 0)
}

/** Returns the local end of week (Sunday 23:59:59.999). */
export function getEndOfWeek(d: Date): Date {
  const start = getStartOfWeek(d)
  return new Date(start.getFullYear(), start.getMonth(), start.getDate() + 6, 23, 59, 59, 999)
}

/** Returns the local start of month (1st 00:00:00.000). */
export function getStartOfMonth(d: Date): Date {
  return new Date(d.getFullYear(), d.getMonth(), 1, 0, 0, 0, 0)
}

/** Returns the local end of month (last day 23:59:59.999). */
export function getEndOfMonth(d: Date): Date {
  return new Date(d.getFullYear(), d.getMonth() + 1, 0, 23, 59, 59, 999)
}

/** Builds an exact UsageRange for given kind and anchor date. */
export function buildUsageRange(kind: UsageRangeKind, d: Date): UsageRange {
  if (kind === "day") {
    const start = getStartOfDay(d)
    const end = getEndOfDay(d)
    return {
      kind,
      anchor: formatLocalDate(d),
      from: start.toISOString(),
      to: end.toISOString(),
      grain: "hour",
    }
  }
  if (kind === "week") {
    const start = getStartOfWeek(d)
    const end = getEndOfWeek(d)
    return {
      kind,
      anchor: formatLocalDate(start),
      from: start.toISOString(),
      to: end.toISOString(),
      grain: "day",
    }
  }
  // month
  const start = getStartOfMonth(d)
  const end = getEndOfMonth(d)
  return {
    kind,
    anchor: formatLocalMonth(start),
    from: start.toISOString(),
    to: end.toISOString(),
    grain: "day",
  }
}

/** Checks if two dates represent the same calendar day in local time. */
export function isSameDay(a: Date, b: Date): boolean {
  return (
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate()
  )
}

/** Checks if two dates fall into the same calendar week (Mon–Sun) in local time. */
export function isSameWeek(a: Date, b: Date): boolean {
  return isSameDay(getStartOfWeek(a), getStartOfWeek(b))
}

/** Checks if two dates fall into the same calendar month in local time. */
export function isSameMonth(a: Date, b: Date): boolean {
  return a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth()
}
