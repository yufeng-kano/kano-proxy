/**
 * Estimated per-request cost from LiteLLM's community price table plus the
 * public OpenRouter catalog for OpenRouter-specific ids (docs/pricing.md).
 *
 * The source tables are trimmed to the four per-token rates this proxy uses.
 * The combined table lives in the CACHE KV namespace with a 24h freshness
 * window (one KV write per day — the minimal-KV rule) behind a per-isolate
 * in-memory memo, so the request path costs ~0 KV operations and **never** a
 * network fetch: refreshes happen from the daily cron, or inline from the
 * admin usage-summary route the first time after a deploy when KV has nothing
 * yet.
 *
 * Everything degrades to "cost unknown" (null): a fetch failure serves the
 * last copy regardless of age, an unmatched model prices as null. Never
 * fabricate a rate, never fail or delay a proxied request over pricing.
 */

import type { Env } from "../env"

export const LITELLM_PRICING_URL =
  "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json"
export const OPENROUTER_MODELS_URL = "https://openrouter.ai/api/v1/models"

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
  /** Source is persisted so OpenRouter rows cannot use a LiteLLM lookalike. */
  source?: "litellm" | "openrouter"
}

/** Keyed by lowercased LiteLLM model id. */
export type PriceTable = Record<string, ModelPrice>

type CachedTable = {
  fetchedAt: number
  table: PriceTable
  /** Present in snapshots created after source tagging was added. */
  litellmTable?: PriceTable
  openRouterTable?: PriceTable
}

let memo: CachedTable | null = null
let memoCheckedAt = 0

export function _resetPricingForTests(): void {
  memo = null
  memoCheckedAt = 0
}

/** Whether the loaded snapshot has separately persisted, source-tagged tables. */
export function hasSourceTaggedPriceTables(): boolean {
  return memo?.litellmTable != null && memo.openRouterTable != null
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

async function fetchAndTrim(
  url: string,
  trim: (json: unknown) => PriceTable,
): Promise<PriceTable | null> {
  try {
    const res = await fetch(url, { headers: { accept: "application/json" } })
    if (!res.ok) return null
    const table = trim(await res.json())
    // An empty trim means the upstream shape changed — do not replace a
    // working source table with nothing.
    return Object.keys(table).length > 0 ? table : null
  } catch {
    return null
  }
}

/**
 * Fetch + trim + store both sources. A failure of either source stale-serves
 * only that source's prior copy, so an OpenRouter outage cannot discard
 * LiteLLM prices (or vice versa). Never throws.
 */
export async function refreshPriceTable(env: Env): Promise<PriceTable | null> {
  const [freshLiteLLM, freshOpenRouter] = await Promise.all([
    fetchAndTrim(LITELLM_PRICING_URL, (json) =>
      trimLiteLLMTable(json as Record<string, unknown>),
    ),
    fetchAndTrim(OPENROUTER_MODELS_URL, trimOpenRouterTable),
  ])

  // Legacy combined snapshots have no provenance: preserve their LiteLLM
  // entries for non-OpenRouter traffic only, and never treat any of their
  // openrouter/<id> entries as catalog prices. A source-tagged snapshot's
  // `table` is just a merged read view and must not become a legacy fallback.
  const legacyLiteLLM =
    memo && !memo.litellmTable && !memo.openRouterTable ? stripOpenRouterEntries(memo.table) : {}
  // Empty source tables record an attempted fetch, letting the cache retain
  // its normal 24h refresh cadence after a source outage.
  const litellmTable = freshLiteLLM ?? memo?.litellmTable ?? legacyLiteLLM
  const openRouterTable = freshOpenRouter ?? memo?.openRouterTable ?? {}
  const table = { ...litellmTable, ...openRouterTable }
  if (Object.keys(table).length === 0) return memo?.table ?? null

  // The public OpenRouter catalog is authoritative for its exact model keys.
  const snap: CachedTable = { fetchedAt: Date.now(), table, litellmTable, openRouterTable }
  memo = snap
  memoCheckedAt = snap.fetchedAt
  try {
    await env.CACHE.put(KV_KEY, JSON.stringify(snap), { expirationTtl: KV_TTL_SECONDS })
  } catch {
    /* keep the in-memory copy */
  }
  return table
}

/** Daily-cron entry: refetch when a source is missing, or the stored table is stale. */
export async function ensureFreshPriceTable(env: Env): Promise<void> {
  await getPriceTable(env)
  if (memo && hasSourceTaggedPriceTables() && Date.now() - memo.fetchedAt < PRICING_FRESH_MS) {
    return
  }
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

function decimal(v: unknown): number | null {
  if (typeof v !== "string" || !v.trim()) return null
  const parsed = Number(v)
  return Number.isFinite(parsed) ? parsed : null
}

/** Legacy combined snapshots cannot prove an OpenRouter price's source. */
function stripOpenRouterEntries(table: PriceTable): PriceTable {
  return Object.fromEntries(
    Object.entries(table).filter(([key]) => !key.toLowerCase().startsWith("openrouter/")),
  )
}

/**
 * Add OpenRouter's default rates only under its complete catalog id. Conditional
 * overrides cannot be applied from the aggregate usage we log, so skip them
 * rather than guessing. Never use these entries for another provider.
 */
export function trimOpenRouterTable(json: unknown): PriceTable {
  const out: PriceTable = {}
  if (!json || typeof json !== "object") return out
  const data = (json as { data?: unknown }).data
  if (!Array.isArray(data)) return out
  for (const model of data) {
    if (!model || typeof model !== "object") continue
    const { id, pricing } = model as { id?: unknown; pricing?: unknown }
    if (typeof id !== "string" || !pricing || typeof pricing !== "object") continue
    const p = pricing as Record<string, unknown>
    if (Array.isArray(p.overrides) && p.overrides.length > 0) continue
    const input = decimal(p.prompt)
    const output = decimal(p.completion)
    // We cannot safely price a partially specified token pair as a zero rate.
    if (input == null || output == null) continue
    out[`openrouter/${normalizeId(id)}`] = {
      input,
      output,
      cacheRead: decimal(p.input_cache_read),
      cacheCreation: decimal(p.input_cache_write),
      source: "openrouter",
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
  const provider = normalizeId(slash === -1 ? "" : rawModel.slice(0, slash))
  const upstream = normalizeId(slash === -1 ? rawModel : rawModel.slice(slash + 1))
  if (!upstream) return null

  // OpenRouter's catalog prices an exact provider/model id. Do not let an
  // unrelated bare, LiteLLM, or Cloudflare entry price an OpenRouter request.
  if (provider === "openrouter") {
    const hit = table[`openrouter/${upstream}`]
    return hit?.source === "openrouter" ? hit : null
  }

  const baseCandidates: string[] = [upstream]
  let rest = upstream
  for (let i = rest.indexOf("/"); i !== -1; i = rest.indexOf("/")) {
    rest = rest.slice(i + 1)
    if (rest) baseCandidates.push(rest)
  }

  const PREFIXES = ["anthropic", "openai", "xai", "gemini", "vertex_ai", "openrouter"]

  // 1. Bare exact candidate matches across all path segments first
  for (const c of baseCandidates) {
    const hit = table[c]
    if (hit?.source !== "openrouter" && hit) return hit
  }

  // 2. Vendor-prefixed exact matches across all path segments
  for (const c of baseCandidates) {
    for (const p of PREFIXES) {
      const hit = table[`${p}/${c}`]
      if (hit?.source !== "openrouter" && hit) return hit
    }
  }

  // 3. Antigravity / Gemini reasoning effort & preview fallback
  // Restrict to verified Antigravity or Gemini model ID segments (starting
  // with "gemini-" or under the "antigravity" provider) to prevent guessing
  // rates for arbitrary non-Gemini models (e.g. notagemini-high).
  const isGeminiModel = (c: string): boolean =>
    provider === "antigravity" ||
    c.startsWith("gemini-") ||
    c.startsWith("gemini/") ||
    c === "gemini" ||
    c.includes("/gemini-") ||
    c.endsWith("/gemini")

  const variantCandidates: string[] = []
  for (const c of baseCandidates) {
    if (!isGeminiModel(c)) continue
    const stripped = c
      .replace(/-(?:thinking-)?(?:high|medium|low|tiered)$/, "")
      .replace(/-(?:thinking|thought)$/, "")
    if (stripped !== c) {
      variantCandidates.push(stripped)
    }
    if (!c.endsWith("-preview")) {
      variantCandidates.push(`${c}-preview`)
      if (stripped !== c) variantCandidates.push(`${stripped}-preview`)
    } else {
      variantCandidates.push(c.slice(0, -"-preview".length))
    }
  }

  if (variantCandidates.length > 0) {
    // 3a. Bare variant matches
    for (const v of variantCandidates) {
      const hit = table[v]
      if (hit?.source !== "openrouter" && hit) return hit
    }

    // 3b. Prefixed variant matches
    for (const v of variantCandidates) {
      for (const p of PREFIXES) {
        const hit = table[`${p}/${v}`]
        if (hit?.source !== "openrouter" && hit) return hit
      }
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
