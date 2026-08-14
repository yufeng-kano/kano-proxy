import { Hono } from "hono"
import type { Context } from "hono"
import type { HonoEnv } from "../auth/session"
import { loadSessionUser } from "../auth/session"
import { PROVIDERS } from "../env"
import { listCustomProviders } from "../db/custom_providers"
import type { UserRow } from "../db/users"
import {
  estimateCost,
  getPriceTable,
  hasSourceTaggedPriceTables,
  refreshPriceTable,
  type PriceTable,
} from "../pricing/litellm"

export const usageRoutes = new Hono<HonoEnv>()

async function requireUser(c: Context<HonoEnv>): Promise<UserRow | null> {
  const loaded = await loadSessionUser(c.env, c.req.header("cookie"))
  if (!loaded) return null
  return loaded.user
}

const ALLOWED_DAYS = new Set([1, 7, 30])

/** Columns this endpoint actually reads off `request_logs`. */
type UsageLogRow = {
  provider: string
  model: string
  account_id: string | null
  group_name: string | null
  status_code: number
  latency_ms: number
  prompt_tokens: number | null
  completion_tokens: number | null
  cache_read_input_tokens: number | null
  cache_creation_input_tokens: number | null
  cost: number | null
  created_at: string
}

type Totals = {
  requests: number
  errors: number
  avg_latency_ms: number | null
  prompt_tokens: number
  completion_tokens: number
  cache_read_input_tokens: number
  cache_creation_input_tokens: number
  cache_rate: number | null
  usage_known_requests: number
  cache_known_requests: number
  /** Estimated USD over priced rows; null when none is priced (docs/pricing.md). */
  cost: number | null
  /** Rows contributing to `cost` — lets the UI annotate partial coverage. */
  cost_known_requests: number
}

/** Shared totals math — used for the overall summary and for each model group. */
function accumulate(rows: UsageLogRow[]): Totals {
  let latencySum = 0
  let cacheRateNumerator = 0
  let cacheRateDenominator = 0
  let cacheRateRows = 0
  const totals: Totals = {
    requests: rows.length,
    errors: 0,
    avg_latency_ms: null,
    prompt_tokens: 0,
    completion_tokens: 0,
    cache_read_input_tokens: 0,
    cache_creation_input_tokens: 0,
    cache_rate: null,
    usage_known_requests: 0,
    cache_known_requests: 0,
    cost: null,
    cost_known_requests: 0,
  }
  for (const row of rows) {
    if (row.status_code >= 400) totals.errors++
    latencySum += row.latency_ms
    if (row.prompt_tokens != null) {
      totals.prompt_tokens += row.prompt_tokens
      totals.usage_known_requests++
    }
    if (row.completion_tokens != null) {
      totals.completion_tokens += row.completion_tokens
    }
    if (row.cache_read_input_tokens != null) {
      totals.cache_read_input_tokens += row.cache_read_input_tokens
      totals.cache_known_requests++
      if (row.prompt_tokens != null && row.prompt_tokens > 0) {
        cacheRateNumerator += row.cache_read_input_tokens
        cacheRateDenominator += row.prompt_tokens
        cacheRateRows++
      }
    }
    if (row.cache_creation_input_tokens != null) {
      totals.cache_creation_input_tokens += row.cache_creation_input_tokens
    }
    if (row.cost != null) {
      totals.cost = (totals.cost ?? 0) + row.cost
      totals.cost_known_requests++
    }
  }
  totals.avg_latency_ms = rows.length ? Math.round(latencySum / rows.length) : null
  totals.cache_rate = cacheRateRows > 0 ? cacheRateNumerator / cacheRateDenominator : null
  return totals
}

type ModelTotals = {
  provider: string
  model: string
  account_id: string | null
  account_label: string | null
  group_name: string | null
} & Omit<Totals, "avg_latency_ms">

/** Per (provider, model, account_id, group_name) breakdown, sorted by prompt+completion tokens desc. */
function modelBreakdown(rows: UsageLogRow[], accountLabels: Map<string, string | null>): ModelTotals[] {
  // The map key is only ever used for grouping identity, never parsed back
  // apart — each value is carried alongside it in the group itself, so no
  // separator choice can collide with an upstream model id. NULL dimensions
  // use sentinels distinct from strings, including an empty string.
  const groups = new Map<
    string,
    { provider: string; model: string; account_id: string | null; group_name: string | null; rows: UsageLogRow[] }
  >()
  for (const row of rows) {
    const key = `${row.provider}\u0000${row.model}\u0000${row.account_id === null ? "<null>" : `s:${row.account_id}`}\u0000${row.group_name === null ? "<null>" : `s:${row.group_name}`}`
    let group = groups.get(key)
    if (!group) {
      group = {
        provider: row.provider,
        model: row.model,
        account_id: row.account_id,
        group_name: row.group_name,
        rows: [],
      }
      groups.set(key, group)
    }
    group.rows.push(row)
  }
  const out: ModelTotals[] = []
  for (const group of groups.values()) {
    const { avg_latency_ms: _avgLatencyMs, ...rest } = accumulate(group.rows)
    out.push({
      provider: group.provider,
      model: group.model,
      account_id: group.account_id,
      account_label: group.account_id === null ? null : accountLabels.get(group.account_id) ?? null,
      group_name: group.group_name,
      ...rest,
    })
  }
  out.sort((a, b) => b.prompt_tokens + b.completion_tokens - (a.prompt_tokens + a.completion_tokens))
  return out
}

/** One (bucket, provider, model) group — the dashboard draws one bar/line per model from these. */
type SeriesPoint = {
  bucket: string
  provider: string
  model: string
  requests: number
  prompt_tokens: number
  completion_tokens: number
  cache_read_input_tokens: number
  /** Requests in this group with non-null cache_read_input_tokens — lets the client tell "0% cached" from "not reported". */
  cache_known_requests: number
  /** Estimated USD over this group's priced rows; null when none is. */
  cost: number | null
}

/** Hour bucket ("YYYY-MM-DDTHH") for days=1, day bucket ("YYYY-MM-DD") otherwise. */
function bucketKey(createdAt: string, days: number): string {
  return days === 1 ? createdAt.slice(0, 13) : createdAt.slice(0, 10)
}

/**
 * Sparse per-(bucket, model) series — only groups that actually have a row,
 * ascending by bucket then model. A bucket's totals are the client-side sum
 * over its model points (see docs/admin-ui.md § Series shape); emitting them
 * again as a separate field would be a second source of truth for the same
 * number.
 */
function buildSeries(rows: UsageLogRow[], days: number): SeriesPoint[] {
  // Same grouping-identity-only key rule as modelBreakdown: never parsed apart,
  // so no separator can collide with an upstream model id.
  const groups = new Map<string, SeriesPoint>()
  for (const row of rows) {
    const bucket = bucketKey(row.created_at, days)
    const key = `${bucket} ${row.provider} ${row.model}`
    let point = groups.get(key)
    if (!point) {
      point = {
        bucket,
        provider: row.provider,
        model: row.model,
        requests: 0,
        prompt_tokens: 0,
        completion_tokens: 0,
        cache_read_input_tokens: 0,
        cache_known_requests: 0,
        cost: null,
      }
      groups.set(key, point)
    }
    point.requests++
    if (row.prompt_tokens != null) point.prompt_tokens += row.prompt_tokens
    if (row.completion_tokens != null) point.completion_tokens += row.completion_tokens
    if (row.cache_read_input_tokens != null) {
      point.cache_read_input_tokens += row.cache_read_input_tokens
      point.cache_known_requests++
    }
    if (row.cost != null) point.cost = (point.cost ?? 0) + row.cost
  }
  return [...groups.values()].sort((a, b) => {
    if (a.bucket !== b.bucket) return a.bucket < b.bucket ? -1 : 1
    if (a.provider !== b.provider) return a.provider < b.provider ? -1 : 1
    return a.model < b.model ? -1 : a.model > b.model ? 1 : 0
  })
}

/**
 * Scope the summary to providers that still exist: the builtins plus the
 * user's live custom slugs (docs/admin-ui.md § Overview page). Rows from a
 * deleted custom endpoint, and junk prefixes recorded by invalid-model 400s
 * ("unknown", a typo'd prefix), would otherwise haunt the dashboard forever.
 */
export function filterToLiveProviders(
  rows: UsageLogRow[],
  customSlugs: Iterable<string>,
): UsageLogRow[] {
  const live = new Set<string>(PROVIDERS)
  for (const slug of customSlugs) live.add(slug)
  return rows.filter((r) => live.has(r.provider))
}

/**
 * Fill NULL costs at read time with the same shared resolver used at write
 * time, so pre-migration history still prices (docs/pricing.md). Stored
 * costs are never overwritten; a row that stays unpriced stays null.
 */
export function fillEstimatedCosts(rows: UsageLogRow[], table: PriceTable | null): UsageLogRow[] {
  if (!table) return rows
  return rows.map((row) => {
    if (row.cost != null) return row
    const cost = estimateCost(table, row.model, {
      promptTokens: row.prompt_tokens,
      completionTokens: row.completion_tokens,
      cacheReadInputTokens: row.cache_read_input_tokens,
      cacheCreationInputTokens: row.cache_creation_input_tokens,
    })
    return cost == null ? row : { ...row, cost }
  })
}

/**
 * Pure aggregation — exported for direct unit testing without a D1 round
 * trip. `rows` must already be scoped to one user and `created_at >= from`.
 */
export function summarizeUsageRows(
  rows: UsageLogRow[],
  days: number,
  from: string,
  accountLabels: Map<string, string | null> = new Map(),
): Record<string, unknown> {
  return {
    days,
    from,
    totals: accumulate(rows),
    models: modelBreakdown(rows, accountLabels),
    series: buildSeries(rows, days),
  }
}

usageRoutes.get("/summary", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)

  const daysParam = c.req.query("days")
  const days = daysParam === undefined ? 7 : Number(daysParam)
  if (!ALLOWED_DAYS.has(days)) {
    return c.json({ error: "invalid_days" }, 400)
  }

  const from = new Date(Date.now() - days * 86_400_000).toISOString()
  const res = await c.env.DB.prepare(
    `SELECT provider, model, account_id, group_name, status_code, latency_ms, prompt_tokens, completion_tokens,
            cache_read_input_tokens, cache_creation_input_tokens, cost, created_at
     FROM request_logs
     WHERE user_id = ? AND created_at >= ?
     ORDER BY created_at ASC`,
  )
    .bind(user.id, from)
    .all<UsageLogRow>()

  const custom = await listCustomProviders(c.env.DB, user.id)
  const scoped = filterToLiveProviders(
    res.results ?? [],
    custom.map((p) => p.slug),
  )

  // Read-time pricing for NULL-cost rows. First call after a deploy may find
  // no table anywhere — or only a legacy untagged snapshot — so fetch it
  // inline here (admin surface, not the proxy hot path). Tagged snapshots
  // remain a KV/memo read only.
  let table = await getPriceTable(c.env)
  if (!table || !hasSourceTaggedPriceTables()) table = await refreshPriceTable(c.env)
  const priced = fillEstimatedCosts(scoped, table)
  const accounts = await c.env.DB.prepare(
    `SELECT id, custom_label, label FROM upstream_accounts WHERE user_id = ?`,
  )
    .bind(user.id)
    .all<{ id: string; custom_label: string | null; label: string | null }>()
  const accountLabels = new Map(
    (accounts.results ?? []).map((account) => [account.id, account.custom_label || account.label || null]),
  )

  return c.json(summarizeUsageRows(priced, days, from, accountLabels))
})
