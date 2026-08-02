/**
 * Minimal in-memory D1 substitute for route/db integration tests. Supports
 * exactly the query shapes this codebase's db/* modules issue: single-table
 * SELECT/INSERT/UPDATE/DELETE with equality WHERE clauses (AND-joined),
 * `SELECT COUNT(*)`, `SELECT COALESCE(MAX(col), 0)`, and `UPDATE ... SET
 * col = ?` / `col = COALESCE(?, col)`. Not a SQL engine — anything outside
 * these shapes throws so a mismatch fails loudly instead of silently no-op.
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

function filterRows(rows: Row[], whereClause: string | undefined, params: unknown[]): Row[] {
  if (!whereClause) return [...rows]
  const conditions = splitTopLevel(whereClause, /^\s+AND\s+/i).map((c) => c.trim())
  let pi = 0
  const consumed = conditions.map((cond) => {
    const eq = cond.match(/^(\w+)\s*=\s*\?$/)
    if (eq) return { col: eq[1]!, paramIndex: pi++ }
    throw new Error(`FakeD1: unsupported WHERE condition: ${cond}`)
  })
  return rows.filter((row) => consumed.every(({ col, paramIndex }) => row[col] === params[paramIndex]))
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
    const row: Row = {}
    cols.forEach((c, i) => (row[c] = params[i]))
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

  m = sql.match(/^SELECT .+? FROM (\w+)(?:\s+WHERE\s+(.+?))?(?:\s+ORDER BY\s+(.+))?$/i)
  if (m) {
    let rows = filterRows(db.rows(m[1]!), m[2], params)
    if (m[3]) rows = sortRows(rows, m[3]!)
    return { rows }
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
        throw new Error(`FakeD1: unsupported SET assignment: ${a}`)
      }
      pi = 0
    }
    return { rows: [], changes: rows.length }
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
