/**
 * Minimal in-memory D1 substitute for route/db integration tests. Supports
 * exactly the query shapes this codebase's db/* modules issue: single-table
 * SELECT/INSERT/UPDATE/DELETE with `=`, `>=`, `<`, `IS [NOT] NULL`, or `NOT IN
 * (?, …)` WHERE conditions (AND-joined, with one bracketed `OR` group for the
 * usage lock's compare-and-swap), `SELECT COUNT(*)`, `SELECT COALESCE(MAX(col),
 * 0)`, `SELECT COALESCE(SUM(col), 0)`, `UPDATE ... SET col = ?` / `col =
 * COALESCE(?, col)` / `col = NULL`, and the retention sweep's batched `DELETE
 * ... WHERE col IN (SELECT col FROM <same table> WHERE <cond> LIMIT n)` shape.
 * Not a SQL engine — anything outside these shapes throws so a mismatch fails
 * loudly instead of silently no-op.
 */

type Row = Record<string, unknown>

export class FakeD1 {
  private tables = new Map<string, Row[]>()

  /** Direct row injection, bypassing SQL — for seeding fixtures. */
  seed(table: string, rows: Row[]): void {
    this.table(table).push(...rows.map((r) => ({ ...r })))
  }

  rows(table: string): Row[] {
    return this.table(table)
  }

  private table(name: string): Row[] {
    if (!this.tables.has(name)) this.tables.set(name, [])
    return this.tables.get(name)!
  }

  prepare(sql: string): FakeStatement {
    return new FakeStatement(this, sql, [])
  }

  async batch(statements: FakeStatement[]): Promise<Array<{ success: boolean; meta: { changes: number } }>> {
    return Promise.all(statements.map((statement) => statement.run()))
  }
}

class FakeStatement {
  constructor(
    private db: FakeD1,
    private sql: string,
    private params: unknown[],
  ) {}

  bind(...params: unknown[]): FakeStatement {
    return new FakeStatement(this.db, this.sql, params)
  }

  async run(): Promise<{ success: boolean; meta: { changes: number } }> {
    const { changes } = execute(this.db, this.sql, this.params)
    return { success: true, meta: { changes: changes ?? 0 } }
  }

  async first<T>(): Promise<T | null> {
    const { rows } = execute(this.db, this.sql, this.params)
    return (rows[0] as T) ?? null
  }

  async all<T>(): Promise<{ results: T[] }> {
    const { rows } = execute(this.db, this.sql, this.params)
    return { results: rows as T[] }
  }
}

function normalize(sql: string): string {
  return sql.replace(/\s+/g, " ").trim()
}

/** Split on `sep` at paren-depth 0 — a COALESCE(?, col) contains a comma that must not split. */
function splitTopLevel(s: string, sep: RegExp): string[] {
  const out: string[] = []
  let depth = 0
  let cur = ""
  let i = 0
  while (i < s.length) {
    const ch = s[i]!
    if (ch === "(") depth++
    if (ch === ")") depth--
    if (depth === 0) {
      sep.lastIndex = 0
      const m = sep.exec(s.slice(i))
      if (m && m.index === 0) {
        out.push(cur)
        cur = ""
        i += m[0].length
        continue
      }
    }
    cur += ch
    i++
  }
  out.push(cur)
  return out
}

type WhereCond =
  | { kind: "cmp"; col: string; op: string; paramIndex: number }
  | { kind: "isNull"; col: string; negated: boolean }
  | { kind: "notIn"; col: string; paramStart: number; count: number }
  | { kind: "or"; branches: WhereCond[] }

/**
 * Parses one AND-term. Bracketed alternatives (`(a IS NULL OR a < ?)` — the
 * usage lock's compare-and-swap) recurse, so parameter positions stay in
 * left-to-right order across the whole clause.
 */
function parseCond(cond: string, next: () => number): WhereCond {
  const grouped = cond.match(/^\((.+)\)$/s)
  if (grouped && splitTopLevel(grouped[1]!, /^\s+OR\s+/i).length > 1) {
    return {
      kind: "or",
      branches: splitTopLevel(grouped[1]!, /^\s+OR\s+/i).map((b) => parseCond(b.trim(), next)),
    }
  }
  const cmp = cond.match(/^(\w+)\s*(=|>=|<)\s*\?$/)
  if (cmp) return { kind: "cmp", col: cmp[1]!, op: cmp[2]!, paramIndex: next() }
  const isNull = cond.match(/^(\w+)\s+IS\s+(NOT\s+)?NULL$/i)
  if (isNull) return { kind: "isNull", col: isNull[1]!, negated: !!isNull[2] }
  const notIn = cond.match(/^(\w+)\s+NOT IN\s*\(([?,\s]+)\)$/i)
  if (notIn) {
    const count = (notIn[2]!.match(/\?/g) || []).length
    const start = next()
    for (let i = 1; i < count; i++) next()
    return { kind: "notIn", col: notIn[1]!, paramStart: start, count }
  }
  throw new Error(`FakeD1: unsupported WHERE condition: ${cond}`)
}

function evalCond(c: WhereCond, row: Row, params: unknown[]): boolean {
  if (c.kind === "or") return c.branches.some((b) => evalCond(b, row, params))
  if (c.kind === "isNull") {
    const isNull = row[c.col] === null || row[c.col] === undefined
    return c.negated ? !isNull : isNull
  }
  if (c.kind === "notIn") {
    const excluded = params.slice(c.paramStart, c.paramStart + c.count)
    return !excluded.includes(row[c.col])
  }
  const actual = row[c.col] as string | number
  const expected = params[c.paramIndex] as string | number
  // SQL: any comparison against NULL is NULL, i.e. not true. Without this a
  // JS `null < "2026-…"` would coerce to 0 and match, so a free lock would
  // read as a broken one.
  if (actual === null || actual === undefined) return false
  if (c.op === ">=") return actual >= expected
  if (c.op === "<") return actual < expected
  return actual === expected
}

function filterRows(rows: Row[], whereClause: string | undefined, params: unknown[]): Row[] {
  if (!whereClause) return [...rows]
  const conditions = splitTopLevel(whereClause, /^\s+AND\s+/i).map((c) => c.trim())
  let pi = 0
  const next = () => pi++
  const consumed: WhereCond[] = conditions.map((cond) => parseCond(cond, next))
  return rows.filter((row) => consumed.every((c) => evalCond(c, row, params)))
}

function sortRows(rows: Row[], orderByClause: string): Row[] {
  const terms = orderByClause.split(",").map((t) => {
    const parts = t.trim().split(/\s+/)
    return { col: parts[0]!, dir: (parts[1] || "ASC").toUpperCase() }
  })
  return [...rows].sort((a, b) => {
    for (const { col, dir } of terms) {
      const av = a[col] as string | number
      const bv = b[col] as string | number
      if (av < bv) return dir === "DESC" ? 1 : -1
      if (av > bv) return dir === "DESC" ? -1 : 1
    }
    return 0
  })
}

function execute(
  db: FakeD1,
  rawSql: string,
  params: unknown[],
): { rows: Row[]; changes?: number } {
  const sql = normalize(rawSql)
  let m: RegExpMatchArray | null

  m = sql.match(/^INSERT INTO (\w+)\s*\(([^)]+)\)\s*VALUES\s*\(([^)]+)\)$/i)
  if (m) {
    const table = m[1]!
    const cols = m[2]!.split(",").map((s) => s.trim())
    const values = splitTopLevel(m[3]!, /^\s*,\s*/)
    const row: Row = {}
    let pi = 0
    cols.forEach((c, i) => {
      const value = values[i]!.trim()
      if (value === "?") row[c] = params[pi++]
      else if (/^'.*'$/.test(value)) row[c] = value.slice(1, -1).replace(/''/g, "'")
      else if (value.toUpperCase() === "NULL") row[c] = null
      else throw new Error(`FakeD1: unsupported INSERT value: ${value}`)
    })
    db.rows(table).push(row)
    return { rows: [], changes: 1 }
  }

  m = sql.match(/^SELECT COUNT\(\*\) as c FROM (\w+)(?:\s+WHERE\s+(.+))?$/i)
  if (m) {
    const rows = filterRows(db.rows(m[1]!), m[2], params)
    return { rows: [{ c: rows.length }] }
  }

  m = sql.match(/^SELECT COALESCE\(MAX\((\w+)\),\s*0\)\s*as m FROM (\w+)(?:\s+WHERE\s+(.+))?$/i)
  if (m) {
    const col = m[1]!
    const rows = filterRows(db.rows(m[2]!), m[3], params)
    const max = rows.reduce((acc, r) => Math.max(acc, Number(r[col] ?? 0)), 0)
    return { rows: [{ m: max }] }
  }

  // SUM over nullable columns: SQL's SUM skips NULLs, COALESCE floors "no
  // rows at all" to 0 — mirror both.
  m = sql.match(/^SELECT COALESCE\(SUM\((\w+)\),\s*0\)\s*as s FROM (\w+)(?:\s+WHERE\s+(.+))?$/i)
  if (m) {
    const col = m[1]!
    const rows = filterRows(db.rows(m[2]!), m[3], params)
    const sum = rows.reduce((acc, r) => acc + (r[col] == null ? 0 : Number(r[col])), 0)
    return { rows: [{ s: sum }] }
  }

  m = sql.match(/^SELECT .+? FROM (\w+)(?:\s+WHERE\s+(.+?))?(?:\s+ORDER BY\s+(.+))?$/i)
  if (m) {
    let rows = filterRows(db.rows(m[1]!), m[2], params)
    if (m[3]) rows = sortRows(rows, m[3]!)
    return { rows }
  }

  // Atomic 520/522/524 strike feedback: this deliberately mirrors the one
  // conditional UPDATE used in production, including its returned resulting
  // count. Keep it explicit rather than making this test helper a SQL engine.
  if (
    /^UPDATE upstream_accounts SET edge_strikes = CASE WHEN edge_strike_at IS NULL OR edge_strike_at < \? THEN 1 WHEN edge_strikes >= 2 THEN 0 ELSE edge_strikes \+ 1 END, edge_strike_at = \?, updated_at = \? WHERE id = \? AND user_id = \? AND provider = \? RETURNING edge_strikes$/i.test(sql)
  ) {
    const [staleBefore, at, updatedAt, accountId, userId, provider] = params
    const rows = db.rows("upstream_accounts").filter(
      (row) => row.id === accountId && row.user_id === userId && row.provider === provider,
    )
    for (const row of rows) {
      const priorAt = row.edge_strike_at
      row.edge_strikes =
        priorAt === null || priorAt === undefined || (priorAt as string) < (staleBefore as string)
          ? 1
          : Number(row.edge_strikes ?? 0) >= 2
            ? 0
            : Number(row.edge_strikes ?? 0) + 1
      row.edge_strike_at = at
      row.updated_at = updatedAt
    }
    return { rows: rows.map((row) => ({ edge_strikes: row.edge_strikes })), changes: rows.length }
  }

  m = sql.match(/^UPDATE (\w+) SET (.+) WHERE (.+)$/i)
  if (m) {
    const table = m[1]!
    const setClause = m[2]!
    const whereClause = m[3]!
    const assignments = splitTopLevel(setClause, /^\s*,\s*/)
    const setParamCount = (setClause.match(/\?/g) || []).length
    const setParams = params.slice(0, setParamCount)
    const whereParams = params.slice(setParamCount)
    const rows = filterRows(db.rows(table), whereClause, whereParams)
    let pi = 0
    for (const row of rows) {
      for (const raw of assignments) {
        const a = raw.trim()
        const coalesce = a.match(/^(\w+)\s*=\s*COALESCE\(\?,\s*\w+\)$/i)
        if (coalesce) {
          const v = setParams[pi++]
          if (v !== null && v !== undefined) row[coalesce[1]!] = v
          continue
        }
        const plain = a.match(/^(\w+)\s*=\s*\?$/i)
        if (plain) {
          row[plain[1]!] = setParams[pi++]
          continue
        }
        // Literal NULL consumes no parameter — the usage lock releases this way.
        const nulled = a.match(/^(\w+)\s*=\s*NULL$/i)
        if (nulled) {
          row[nulled[1]!] = null
          continue
        }
        throw new Error(`FakeD1: unsupported SET assignment: ${a}`)
      }
      pi = 0
    }
    return { rows: [], changes: rows.length }
  }

  // Batched retention-style delete: `DELETE FROM t WHERE col IN (SELECT col
  // FROM t WHERE <cond> LIMIT n)`. Checked ahead of the plain DELETE below —
  // that one would otherwise "match" too (the whole IN (...) clause as its
  // WHERE) and then fail deeper inside filterRows on the subquery text.
  m = sql.match(/^DELETE FROM (\w+) WHERE (\w+) IN \(SELECT \2 FROM \1 WHERE (.+) LIMIT (\d+)\)$/i)
  if (m) {
    const table = m[1]!
    const idCol = m[2]!
    const innerWhere = m[3]!
    const limit = Number(m[4])
    const all = db.rows(table)
    const candidates = filterRows(all, innerWhere, params).slice(0, limit)
    const ids = new Set(candidates.map((r) => r[idCol]))
    const remaining = all.filter((r) => !ids.has(r[idCol]))
    const changes = all.length - remaining.length
    all.length = 0
    all.push(...remaining)
    return { rows: [], changes }
  }

  m = sql.match(/^DELETE FROM (\w+)(?:\s+WHERE\s+(.+))?$/i)
  if (m) {
    const table = m[1]!
    const all = db.rows(table)
    const toDelete = new Set(filterRows(all, m[2], params))
    const remaining = all.filter((r) => !toDelete.has(r))
    all.length = 0
    all.push(...remaining)
    return { rows: [], changes: toDelete.size }
  }

  throw new Error(`FakeD1: unsupported SQL: ${sql}`)
}

export function fakeKV(): KVNamespace {
  const store = new Map<string, string>()
  return {
    // Real KV parses the stored value when `type === "json"`; readModelsCache
    // relies on that (`typeof raw !== "object"` gates a cache miss).
    get: (async (key: string, type?: string) => {
      const v = store.get(key)
      if (v === undefined) return null
      return type === "json" ? JSON.parse(v) : v
    }) as KVNamespace["get"],
    put: (async (key: string, value: string) => {
      store.set(key, value)
    }) as KVNamespace["put"],
    delete: (async (key: string) => {
      store.delete(key)
    }) as KVNamespace["delete"],
    list: (async () => ({ keys: [], list_complete: true, cacheStatus: null })) as KVNamespace["list"],
    getWithMetadata: (async () => ({ value: null, metadata: null, cacheStatus: null })) as unknown as KVNamespace["getWithMetadata"],
  } as unknown as KVNamespace
}
