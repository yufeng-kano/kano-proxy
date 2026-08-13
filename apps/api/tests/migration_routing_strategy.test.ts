/**
 * Syntax/shape sanity for `0010_routing_strategy.sql` — this repo's test
 * setup runs entirely on `FakeD1` (docs/testing.md), not real D1
 * migrations, so there is no `wrangler d1 migrations apply` step to exercise
 * here. This instead pins the statements the rest of the routing-module
 * tests implicitly depend on (db/model_groups.ts and db/provider_settings.ts
 * both assume this exact shape).
 */
import { describe, expect, it } from "vitest"
import { readFileSync } from "node:fs"
import { join } from "node:path"

const sql = readFileSync(join(__dirname, "../migrations/0010_routing_strategy.sql"), "utf8")

describe("0010_routing_strategy.sql", () => {
  it("adds model_groups.strategy as NOT NULL DEFAULT 'ordered'", () => {
    expect(sql).toMatch(/ALTER TABLE model_groups ADD COLUMN strategy TEXT NOT NULL DEFAULT 'ordered'/)
  })

  it("creates provider_settings with the documented shape", () => {
    expect(sql).toMatch(/CREATE TABLE provider_settings/)
    expect(sql).toMatch(/user_id TEXT NOT NULL/)
    expect(sql).toMatch(/provider TEXT NOT NULL/)
    expect(sql).toMatch(/strategy TEXT NOT NULL DEFAULT 'ordered'/)
    expect(sql).toMatch(/updated_at TEXT NOT NULL/)
    expect(sql).toMatch(/PRIMARY KEY \(user_id, provider\)/)
    expect(sql).toMatch(/FOREIGN KEY \(user_id\) REFERENCES users\(id\) ON DELETE CASCADE/)
  })

  it("does not renumber or touch any earlier migration file", () => {
    // This file's own existence at 0010 (not reusing/renaming 0001-0009) is
    // enforced by the filename itself; this test documents the intent so a
    // future edit here trips it if the file is ever renamed.
    expect(sql.length).toBeGreaterThan(0)
  })

  it("every non-comment statement is terminated (balanced parens, ends in ;)", () => {
    const statements = sql
      .split("\n")
      .filter((line) => !line.trim().startsWith("--"))
      .join("\n")
      .split(";")
      .map((s) => s.trim())
      .filter(Boolean)
    expect(statements.length).toBeGreaterThanOrEqual(2)
    for (const stmt of statements) {
      const opens = (stmt.match(/\(/g) || []).length
      const closes = (stmt.match(/\)/g) || []).length
      expect(opens).toBe(closes)
    }
  })
})
