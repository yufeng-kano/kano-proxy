/**
 * Translation runtime.
 *
 * Hand-rolled rather than `vue-i18n`, for the same reason the dashboard chart
 * is hand-rolled SVG: the feature set this app needs (lookup, interpolation,
 * plural selection, locale-aware number/date formatting) is a few dozen lines,
 * and a dependency-free module keeps the whole surface typed end to end — a
 * missing or misspelled key is a compile error, not a string that ships
 * looking like `dashboard.titel`.
 *
 * English is the only locale today. Everything below is written so adding a
 * second one is a data change (a new catalog + a registry entry), not a
 * refactor: no component ever hardcodes "en", reads a message off the catalog
 * directly, or formats a number without going through `useI18n()`.
 *
 * See docs/i18n.md.
 */

import { computed, ref } from "vue"
import { en } from "./en"

/** Every locale this build ships. Add a catalog here to add a language. */
export const LOCALES = ["en"] as const

export type Locale = (typeof LOCALES)[number]

export const DEFAULT_LOCALE: Locale = "en"

/**
 * English is the reference catalog: it defines the key union, so a new locale
 * that omits a key (or invents one) fails to typecheck rather than rendering a
 * blank label at runtime.
 */
export type CatalogKey = keyof typeof en

/**
 * The base of a plural family, recovered from its own `_one` / `_other`
 * entries: `"models.count_one"` yields `"models.count"`.
 *
 * Callers pass the base and a `count`; the suffixed entries are what the
 * catalog stores but never what a component names. Deriving the union this way
 * means adding a plural family to `en.ts` makes its base callable
 * automatically, and a family whose forms get deleted stops typechecking at
 * every call site.
 */
type PluralBase<K> = K extends `${infer Base}_${PluralCategory}` ? Base : never

/** The CLDR categories; `_one` / `_other` are the two English needs. */
type PluralCategory = "zero" | "one" | "two" | "few" | "many" | "other"

/** Anything a component may pass to `t()`: an exact key, or a plural base. */
export type MessageKey = CatalogKey | PluralBase<CatalogKey>

type Catalog = Record<CatalogKey, string>

const CATALOGS: Record<Locale, Catalog> = { en }

/**
 * Values a message placeholder may take. Numbers and dates are formatted with
 * the active locale on the way in, so a caller never has to pre-format —
 * passing a raw `Date` or `number` is the intended path.
 */
export type InterpolationValue = string | number | Date

export type Interpolations = Record<string, InterpolationValue>

const locale = ref<Locale>(DEFAULT_LOCALE)

/** BCP-47 tag handed to `Intl.*`. Kept separate from `Locale` so a future "pt-BR" needs no special case. */
const intlLocale = computed(() => locale.value)

function isLocale(value: string): value is Locale {
  return (LOCALES as readonly string[]).includes(value)
}

/**
 * Picks the best supported locale for this browser, falling back to English.
 * With a single shipped locale this always resolves to English — it exists so
 * the fallback chain is already correct the day a second catalog lands.
 */
export function detectLocale(): Locale {
  if (typeof navigator === "undefined") return DEFAULT_LOCALE
  for (const tag of navigator.languages ?? []) {
    const base = tag.toLowerCase().split("-")[0]
    if (base && isLocale(base)) return base
  }
  return DEFAULT_LOCALE
}

export function setLocale(next: Locale): void {
  locale.value = next
  if (typeof document !== "undefined") document.documentElement.lang = next
}

/**
 * `{name}` placeholders. An unknown placeholder is left verbatim rather than
 * replaced with "undefined": a visible `{count}` in a screenshot is a bug
 * report, an empty gap is a mystery.
 */
const PLACEHOLDER_RE = /\{(\w+)\}/g

function interpolate(
  template: string,
  values: Interpolations | undefined,
  tag: string,
): string {
  if (!values) return template
  return template.replace(PLACEHOLDER_RE, (match, name: string) => {
    const value = values[name]
    if (value === undefined) return match
    if (typeof value === "number") return new Intl.NumberFormat(tag).format(value)
    if (value instanceof Date) return new Intl.DateTimeFormat(tag).format(value)
    return value
  })
}

/**
 * Plural-aware lookup. A key with plural forms is stored as sibling keys
 * suffixed by CLDR category (`…_one`, `…_other`); `Intl.PluralRules` picks
 * between them, so a locale with three or six forms needs only more catalog
 * entries, never a code change at the call site.
 */
function resolve(key: string, count: number | undefined, tag: string): string {
  const catalog = CATALOGS[locale.value]
  if (count !== undefined) {
    const category = new Intl.PluralRules(tag).select(count)
    const plural = catalog[`${key}_${category}` as CatalogKey]
    if (plural !== undefined) return plural
    // A locale without the selected category still gets a sentence: `other`
    // is the one form CLDR guarantees every language defines.
    const other = catalog[`${key}_other` as CatalogKey]
    if (other !== undefined) return other
  }
  const exact = catalog[key as CatalogKey]
  if (exact !== undefined) return exact
  // English is the reference catalog, so a key missing from a translation
  // falls back to it rather than rendering the raw key at the user.
  const fallback = CATALOGS[DEFAULT_LOCALE][key as CatalogKey]
  return fallback ?? key
}

export type TranslateOptions = Interpolations & {
  /** Selects the plural form and is itself available as `{count}`. */
  count?: number
}

/**
 * The one message lookup in the app.
 *
 * ```ts
 * t("keys.title")
 * t("keys.created", { name })
 * t("models.count", { count: models.length })
 * ```
 */
export function translate(key: MessageKey, options?: TranslateOptions): string {
  const active = tag()
  return interpolate(resolve(key, options?.count, active), options, active)
}

/**
 * Locale-bound formatters. Components call these instead of
 * `toLocaleString()` so every number, date, and list in the UI follows the
 * *app's* locale rather than whatever the OS happens to be set to.
 *
 * Each function reads the locale when it is *called*, not when this object is
 * built — so a language switch reformats live instead of leaving numbers and
 * dates in the previous locale until reload.
 */
function makeFormatters() {
  return {
    /** Exact integer with thousands separators — counts where precision matters. */
    integer(n: number | null | undefined): string {
      if (n == null || Number.isNaN(n)) return EM_DASH
      return new Intl.NumberFormat(tag(), { maximumFractionDigits: 0 }).format(n)
    },
    /** Compact magnitude for large counts: 12300 → "12.3K". */
    compact(n: number | null | undefined): string {
      if (n == null || Number.isNaN(n)) return EM_DASH
      if (Math.abs(n) < 1000) {
        return new Intl.NumberFormat(tag(), { maximumFractionDigits: 0 }).format(n)
      }
      return new Intl.NumberFormat(tag(), {
        notation: "compact",
        maximumFractionDigits: Math.abs(n) < 10_000 ? 1 : 2,
      }).format(n)
    },
    /** Ratio 0..1 as a percent: 0.734 → "73.4%". */
    percent(ratio: number | null | undefined, digits = 1): string {
      if (ratio == null || Number.isNaN(ratio)) return EM_DASH
      return new Intl.NumberFormat(tag(), {
        style: "percent",
        minimumFractionDigits: digits,
        maximumFractionDigits: digits,
      }).format(ratio)
    },
    /** Whole-percent value already on a 0–100 scale (upstream usage windows). */
    percentValue(value: number | null | undefined): string {
      if (value == null || Number.isNaN(value)) return EM_DASH
      return new Intl.NumberFormat(tag(), {
        style: "percent",
        maximumFractionDigits: 0,
      }).format(value / 100)
    },
    duration(ms: number | null | undefined): string {
      if (ms == null || Number.isNaN(ms)) return EM_DASH
      if (ms < 1000) return `${Math.round(ms)} ms`
      return `${new Intl.NumberFormat(tag(), { maximumFractionDigits: 1 }).format(ms / 1000)} s`
    },
    date(value: string | Date | null | undefined): string {
      const d = toDate(value)
      if (!d) return EM_DASH
      return new Intl.DateTimeFormat(tag(), { dateStyle: "medium" }).format(d)
    },
    dateTime(value: string | Date | null | undefined): string {
      const d = toDate(value)
      if (!d) return EM_DASH
      return new Intl.DateTimeFormat(tag(), {
        dateStyle: "medium",
        timeStyle: "short",
      }).format(d)
    },
    /** Axis-tick label: "3 PM" for hourly buckets, "Mar 4" for daily ones. */
    bucketLabel(date: Date, hourly: boolean): string {
      return new Intl.DateTimeFormat(
        tag(),
        hourly ? { hour: "numeric" } : { month: "short", day: "numeric" },
      ).format(date)
    },
    /** Tooltip / table label for a time bucket — spelled out, unlike the axis tick. */
    bucketFull(date: Date, hourly: boolean): string {
      return new Intl.DateTimeFormat(
        tag(),
        hourly
          ? { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" }
          : { weekday: "short", month: "short", day: "numeric" },
      ).format(date)
    },
    /**
     * "in 3 hours" / "2 days ago". Coarsest sensible unit — a reset time five
     * hours out reads better as "in 5 hours" than "in 300 minutes".
     */
    relative(value: string | Date | null | undefined): string {
      const d = toDate(value)
      if (!d) return EM_DASH
      const deltaMs = d.getTime() - Date.now()
      const rtf = new Intl.RelativeTimeFormat(tag(), { numeric: "auto" })
      for (const [unit, ms] of RELATIVE_UNITS) {
        if (Math.abs(deltaMs) >= ms || unit === "minute") {
          return rtf.format(Math.round(deltaMs / ms), unit)
        }
      }
      return rtf.format(0, "minute")
    },
  }
}

/** Active BCP-47 tag, read per call so a locale switch reformats live. */
function tag(): string {
  return intlLocale.value
}

/** Built once — every method reads the live locale, so there is nothing per-call to rebuild. */
const formatters = makeFormatters()

export type Formatters = ReturnType<typeof makeFormatters>

const EM_DASH = "—"

const RELATIVE_UNITS: [Intl.RelativeTimeFormatUnit, number][] = [
  ["day", 86_400_000],
  ["hour", 3_600_000],
  ["minute", 60_000],
]

function toDate(value: string | Date | null | undefined): Date | null {
  if (value == null) return null
  const d = value instanceof Date ? value : new Date(value)
  return Number.isNaN(d.getTime()) ? null : d
}

/**
 * Component-facing entry point.
 *
 * `t` is a plain function rather than a computed string map so templates read
 * naturally (`{{ t("nav.accounts") }}`), and `locale` is a ref so a future
 * language switcher re-renders every consumer without a reload.
 */
export function useI18n() {
  return {
    t: translate,
    locale: computed(() => locale.value),
    setLocale,
    format: formatters,
  }
}
