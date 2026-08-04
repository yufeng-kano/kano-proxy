/**
 * Estimated per-request cost from LiteLLM's community price table
 * (docs/pricing.md).
 *
 * The full table is ~2MB of JSON; only the four per-token rates this proxy
 * uses survive the trim. The trimmed table lives in the CACHE KV namespace
 * with a 24h freshness window (one KV write per day — the minimal-KV rule)
 * behind a per-isolate in-memory memo, so the request path costs ~0 KV
 * operations and **never** a network fetch: refreshes happen from the daily
 * cron, or inline from the admin usage-summary route the first time after a
 * deploy when KV has nothing yet.
 *
 * Everything degrades to "cost unknown" (null): a fetch failure serves the
 * last copy regardless of age, an unmatched model prices as null. Never
 * fabricate a rate, never fail or delay a proxied request over pricing.
 */

import type { Env } from "../env"

export const LITELLM_PRICING_URL =
  "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json"

const KV_KEY = "pricing:litellm:v1"
/** KV lifetime — long enough that a week of failed refreshes still stale-serves. */
const KV_TTL_SECONDS = 7 * 24 * 60 * 60
/** Age past which the daily cron re-fetches. */
export const PRICING_FRESH_MS = 24 * 60 * 60 * 1000
/** How long an isolate trusts its in-memory copy before re-consulting KV. */
const MEMO_RECHECK_MS = 5 * 60 * 1000

/** USD per token. `null` cache rates mean the table had none — see computeCost. */
export type ModelPrice = {
  input: number
  output: number
  cacheRead: number | null
  cacheCreation: number | null
}

/** Keyed by lowercased LiteLLM model id. */
export type PriceTable = Record<string, ModelPrice>

type CachedTable = { fetchedAt: number; table: PriceTable }

let memo: CachedTable | null = null
let memoCheckedAt = 0

export function _resetPricingForTests(): void {
  memo = null
  memoCheckedAt = 0
}

/**
 * Memo → KV → null. Never fetches. A KV failure or malformed entry falls back
 * to whatever the isolate already holds. The recheck window caches misses
 * too, so an isolate with no table (pre-first-cron) does not pay a KV read
 * on every log write.
 */
export async function getPriceTable(env: Env): Promise<PriceTable | null> {
  const now = Date.now()
  if (now - memoCheckedAt < MEMO_RECHECK_MS) return memo?.table ?? null
  memoCheckedAt = now
  try {
    const raw = await env.CACHE.get(KV_KEY, "json")
    if (raw && typeof raw === "object") {
      const snap = raw as CachedTable
      if (typeof snap.fetchedAt === "number" && snap.table && typeof snap.table === "object") {
        memo = snap
      }
    }
  } catch {
    /* KV hiccup: serve memory, if any */
  }
  return memo?.table ?? null
}

/**
 * Fetch + trim + store. Returns the freshest table it can — on any failure,
 * the previous copy (or null when there has never been one). Never throws.
 */
export async function refreshPriceTable(env: Env): Promise<PriceTable | null> {
  try {
    const res = await fetch(LITELLM_PRICING_URL, { headers: { accept: "application/json" } })
    if (!res.ok) return memo?.table ?? null
    const json = (await res.json()) as Record<string, unknown>
    const table = trimLiteLLMTable(json)
    // An empty trim means the upstream shape changed — keep the old copy
    // rather than overwriting a working table with nothing.
    if (Object.keys(table).length === 0) return memo?.table ?? null
    const snap: CachedTable = { fetchedAt: Date.now(), table }
    memo = snap
    memoCheckedAt = snap.fetchedAt
    try {
      await env.CACHE.put(KV_KEY, JSON.stringify(snap), { expirationTtl: KV_TTL_SECONDS })
    } catch {
      /* keep the in-memory copy */
    }
    return table
  } catch {
    return memo?.table ?? null
  }
}

/** Daily-cron entry: refetch only when the stored table is missing or past the freshness window. */
export async function ensureFreshPriceTable(env: Env): Promise<void> {
  await getPriceTable(env)
  if (memo && Date.now() - memo.fetchedAt < PRICING_FRESH_MS) return
  await refreshPriceTable(env)
}

function num(v: unknown): number | null {
  return typeof v === "number" && Number.isFinite(v) ? v : null
}

/** Keeps only entries that carry a usable rate; keys lowercased for lookup. */
export function trimLiteLLMTable(json: Record<string, unknown>): PriceTable {
  const out: PriceTable = {}
  for (const [key, value] of Object.entries(json)) {
    // "sample_spec" is LiteLLM's schema-documentation entry, not a model.
    if (key === "sample_spec" || !value || typeof value !== "object") continue
    const v = value as Record<string, unknown>
    const input = num(v.input_cost_per_token)
    const output = num(v.output_cost_per_token)
    if (input == null && output == null) continue
    out[key.toLowerCase()] = {
      input: input ?? 0,
      output: output ?? 0,
      cacheRead: num(v.cache_read_input_token_cost),
      cacheCreation: num(v.cache_creation_input_token_cost),
    }
  }
  return out
}

/** Lowercase, trimmed, bracket variant stripped: "claude-opus-5[1m]" → "claude-opus-5". */
function normalizeId(id: string): string {
  return id.trim().toLowerCase().replace(/\[[^\]]*\]$/, "")
}

/**
 * `rawModel` is this proxy's `provider/upstream…` id; table keys are LiteLLM's
 * own, sometimes vendor-prefixed. Matching chain (docs/pricing.md): the
 * upstream id exact, then with its own leading path segments progressively
 * stripped, then common LiteLLM prefix forms of each candidate. First hit
 * wins; no hit is null, never a guess.
 */
export function resolveModelPrice(table: PriceTable, rawModel: string): ModelPrice | null {
  const slash = rawModel.indexOf("/")
  const upstream = normalizeId(slash === -1 ? rawModel : rawModel.slice(slash + 1))
  if (!upstream) return null

  const candidates: string[] = [upstream]
  let rest = upstream
  for (let i = rest.indexOf("/"); i !== -1; i = rest.indexOf("/")) {
    rest = rest.slice(i + 1)
    if (rest) candidates.push(rest)
  }

  for (const c of candidates) {
    const hit = table[c]
    if (hit) return hit
  }
  const PREFIXES = ["anthropic", "openai", "xai", "gemini", "vertex_ai", "openrouter"]
  for (const c of candidates) {
    for (const p of PREFIXES) {
      const hit = table[`${p}/${c}`]
      if (hit) return hit
    }
  }
  return null
}

export type CostUsage = {
  promptTokens: number | null
  completionTokens: number | null
  cacheReadInputTokens: number | null
  cacheCreationInputTokens: number | null
}

/**
 * `promptTokens` is the stored **total** input count including cache reads
 * and writes (docs/database.md), so the uncached component is the remainder,
 * floored at 0 against inconsistent upstream numbers. A table entry without
 * cache rates bills cached input at the plain input rate. All-null usage is
 * null — nothing reported prices as nothing known, not $0.
 */
export function computeCost(price: ModelPrice, usage: CostUsage): number | null {
  if (
    usage.promptTokens == null &&
    usage.completionTokens == null &&
    usage.cacheReadInputTokens == null &&
    usage.cacheCreationInputTokens == null
  ) {
    return null
  }
  const cacheRead = usage.cacheReadInputTokens ?? 0
  const cacheCreation = usage.cacheCreationInputTokens ?? 0
  const uncached = Math.max(0, (usage.promptTokens ?? 0) - cacheRead - cacheCreation)
  return (
    uncached * price.input +
    cacheRead * (price.cacheRead ?? price.input) +
    cacheCreation * (price.cacheCreation ?? price.input) +
    (usage.completionTokens ?? 0) * price.output
  )
}

/** resolve + compute in one step — null when the model has no price or usage is all-null. */
export function estimateCost(
  table: PriceTable,
  rawModel: string,
  usage: CostUsage,
): number | null {
  const price = resolveModelPrice(table, rawModel)
  if (!price) return null
  return computeCost(price, usage)
}
