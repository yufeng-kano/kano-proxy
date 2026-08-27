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
} & Omit<Totals, "avg_latency_ms">

/** Per (provider, model) breakdown, sorted by prompt+completion tokens desc. */
function modelBreakdown(rows: UsageLogRow[]): ModelTotals[] {
  // The key is only used for grouping identity, never parsed back apart, so no
  // separator choice can collide with an upstream model id.
  const groups = new Map<string, { provider: string; model: string; rows: UsageLogRow[] }>()
  for (const row of rows) {
    const key = `${row.provider}\u0000${row.model}`
    let group = groups.get(key)
    if (!group) {
      group = { provider: row.provider, model: row.model, rows: [] }
      groups.set(key, group)
    }
    group.rows.push(row)
  }
  const out: ModelTotals[] = []
  for (const group of groups.values()) {
    const { avg_latency_ms: _avgLatencyMs, ...rest } = accumulate(group.rows)
    out.push({ provider: group.provider, model: group.model, ...rest })
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

/**
 * Hour bucket ("YYYY-MM-DDTHH") for grain='hour', day bucket ("YYYY-MM-DD")
 * otherwise, in the caller's calendar.
 *
 * `offsetMinutes` is the client's minutes east of UTC. The range picker selects
 * a *local* day / week / month, so its `from`..`to` lands on local midnights —
 * which in UTC straddle a day boundary for every browser off UTC. Slicing the
 * raw UTC string there spreads a 7-day week over eight keys, and the chart's
 * fixed-width grid then silently drops one end while the totals still count it.
 * Shifting first buckets in the same calendar the user picked in. 0 = UTC,
 * which is what the legacy `days=` window uses.
 */
function bucketKey(createdAt: string, grain: "hour" | "day", offsetMinutes: number): string {
  const iso =
    offsetMinutes === 0
      ? createdAt
      : new Date(Date.parse(createdAt) + offsetMinutes * 60_000).toISOString()
  return grain === "hour" ? iso.slice(0, 13) : iso.slice(0, 10)
}

/**
 * Sparse per-(bucket, model) series — only groups that actually have a row,
 * ascending by bucket then model. A bucket's totals are the client-side sum
 * over its model points (see docs/admin-ui.md § Series shape); emitting them
 * again as a separate field would be a second source of truth for the same
 * number.
 */
function buildSeries(
  rows: UsageLogRow[],
  grain: "hour" | "day",
  offsetMinutes: number,
): SeriesPoint[] {
  // Same grouping-identity-only key rule as modelBreakdown: never parsed apart,
  // so no separator can collide with an upstream model id.
  const groups = new Map<string, SeriesPoint>()
  for (const row of rows) {
    const bucket = bucketKey(row.created_at, grain, offsetMinutes)
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
type CostLogRow = {
  model: string
  prompt_tokens: number | null
  completion_tokens: number | null
  cache_read_input_tokens: number | null
  cache_creation_input_tokens: number | null
  cost: number | null
}

export function fillEstimatedCosts<T extends CostLogRow>(rows: T[], table: PriceTable | null): T[] {
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
 * trip. `rows` must already be scoped to one user and `created_at >= from AND created_at <= to`.
 */
export function summarizeUsageRows(
  rows: UsageLogRow[],
  days: number,
  from: string,
  to?: string,
  grain?: "hour" | "day",
  offsetMinutes = 0,
): Record<string, unknown> {
  const actualGrain: "hour" | "day" = grain ?? (days === 1 ? "hour" : "day")
  return {
    days,
    from,
    to: to ?? new Date().toISOString(),
    grain: actualGrain,
    // Echoed so the client zero-fills its grid in the same calendar the rows
    // were bucketed in; a client that sent no offset reads back 0.
    offset: offsetMinutes,
    totals: accumulate(rows),
    models: modelBreakdown(rows),
    series: buildSeries(rows, actualGrain, offsetMinutes),
  }
}

usageRoutes.get("/summary", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)

  const fromParam = c.req.query("from")
  const toParam = c.req.query("to")
  const grainParam = c.req.query("grain")
  const offsetParam = c.req.query("offset")
  const daysParam = c.req.query("days")

  // Bucket calendar, minutes east of UTC. Widest real zone is UTC+14, and
  // half-hour/45-minute zones are why this is minutes rather than hours.
  let offsetMinutes = 0
  if (offsetParam !== undefined) {
    const parsed = Number(offsetParam)
    if (!Number.isInteger(parsed) || Math.abs(parsed) > 840) {
      return c.json({ error: "invalid_offset" }, 400)
    }
    offsetMinutes = parsed
  }

  let from: string
  let to: string
  let grain: "hour" | "day"
  let days: number

  if (fromParam !== undefined) {
    const fromMs = Date.parse(fromParam)
    if (Number.isNaN(fromMs)) {
      return c.json({ error: "invalid_from" }, 400)
    }
    let toMs = Date.now()
    if (toParam !== undefined) {
      toMs = Date.parse(toParam)
      if (Number.isNaN(toMs)) {
        return c.json({ error: "invalid_to" }, 400)
      }
    }
    if (fromMs > toMs) {
      return c.json({ error: "invalid_range" }, 400)
    }
    const MAX_SPAN_MS = 366 * 86_400_000
    if (toMs - fromMs > MAX_SPAN_MS) {
      return c.json({ error: "range_too_large" }, 400)
    }

    from = new Date(fromMs).toISOString()
    to = new Date(toMs).toISOString()

    if (grainParam !== undefined) {
      if (grainParam !== "hour" && grainParam !== "day") {
        return c.json({ error: "invalid_grain" }, 400)
      }
      grain = grainParam
    } else {
      grain = toMs - fromMs <= 36 * 3_600_000 ? "hour" : "day"
    }

    days = Math.max(1, Math.round((toMs - fromMs) / 86_400_000))
  } else {
    days = daysParam === undefined ? 7 : Number(daysParam)
    if (!ALLOWED_DAYS.has(days)) {
      return c.json({ error: "invalid_days" }, 400)
    }

    from = new Date(Date.now() - days * 86_400_000).toISOString()
    to = new Date().toISOString()
    grain = days === 1 ? "hour" : "day"
  }

  const res = await c.env.DB.prepare(
    `SELECT provider, model, status_code, latency_ms, prompt_tokens, completion_tokens,
            cache_read_input_tokens, cache_creation_input_tokens, cost, created_at
     FROM request_logs
     WHERE user_id = ? AND created_at >= ? AND created_at <= ?
     ORDER BY created_at ASC`,
  )
    .bind(user.id, from, to)
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
  return c.json(summarizeUsageRows(priced, days, from, to, grain, offsetMinutes))
})
