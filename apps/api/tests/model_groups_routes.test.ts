import { describe, expect, it } from "vitest"
import { modelGroupRoutes } from "../src/routes/model_groups"
import { createSession } from "../src/auth/session"
import {
  MAX_ALIASES_PER_GROUP,
  MAX_MODEL_GROUPS_PER_USER,
  MAX_TARGETS_PER_GROUP,
} from "../src/utils/model_group"
import type { Env } from "../src/env"
import { FakeD1, fakeKV } from "./helpers/fake_d1"

const SESSION_SECRET = "test-session-secret-not-real"
const APP_URL = "https://app.example.com"

function buildEnv(db: FakeD1): Env {
  return {
    DB: db as unknown as D1Database,
    BENCH: fakeKV(),
    CACHE: fakeKV(),
    APP_URL,
    SESSION_SECRET,
  } as unknown as Env
}

function seedUser(db: FakeD1, id = "user_1"): void {
  db.seed("users", [
    {
      id,
      google_sub: `sub-${id}`,
      email: `${id}@example.com`,
      name: "Test User",
      picture_url: null,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
}

function seedCustomProvider(db: FakeD1, userId: string, slug: string): void {
  db.seed("custom_providers", [
    {
      id: `cprov_${slug}`,
      user_id: userId,
      slug,
      name: slug,
      format: "openai",
      base_url: "https://upstream.example.com/v1",
      models_mode: "auto",
      manual_models_json: null,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
}

function seedAccount(
  db: FakeD1,
  opts: { id: string; userId: string; provider: string; label?: string | null; customLabel?: string | null },
): void {
  db.seed("upstream_accounts", [
    {
      id: opts.id,
      user_id: opts.userId,
      provider: opts.provider,
      external_account_id: null,
      label: opts.label ?? null,
      custom_label: opts.customLabel ?? null,
      priority: 1,
      encrypted_payload: "encrypted",
      account_meta_json: null,
      usage_snapshot_json: null,
      usage_fetched_at: null,
      usage_fetching_at: null,
      created_at: "2026-01-01T00:00:00.000Z",
      updated_at: "2026-01-01T00:00:00.000Z",
    },
  ])
}

/** Simulate account deletion — removes the row in place, same array the db module reads/writes. */
function deleteAccount(db: FakeD1, accountId: string): void {
  const rows = db.rows("upstream_accounts")
  const idx = rows.findIndex((r) => r.id === accountId)
  if (idx !== -1) rows.splice(idx, 1)
}

async function cookieFor(env: Env, userId: string): Promise<string> {
  const { cookie } = await createSession(env, userId)
  return cookie.split(";")[0]!
}

async function readJson(res: Response): Promise<any> {
  return res.json()
}

function req(method: string, cookie: string, body?: unknown): RequestInit {
  return {
    method,
    headers: { "content-type": "application/json", cookie, host: "app.example.com" },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  }
}

const validCreateBody = {
  name: "Opus",
  aliases: ["opus"],
  targets: ["claude-code/claude-opus-5"],
}

async function createGroup(
  env: Env,
  cookie: string,
  overrides: Partial<{ name: string; aliases: unknown; targets: unknown }> = {},
) {
  return modelGroupRoutes.request("/", req("POST", cookie, { ...validCreateBody, ...overrides }), env)
}

describe("GET /api/model-groups", () => {
  it("requires auth", async () => {
    const db = new FakeD1()
    const res = await modelGroupRoutes.request("/", { method: "GET" }, buildEnv(db))
    expect(res.status).toBe(401)
  })

  it("lists the user's groups with aliases and targets in priority order", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    await createGroup(env, cookie, {
      aliases: ["gpt-4o", "gpt-4"],
      targets: ["claude-code/claude-opus-5", "grok/grok-4.5"],
    })

    const res = await modelGroupRoutes.request("/", req("GET", cookie), env)
    const json = await readJson(res)
    expect(json.groups).toHaveLength(1)
    expect(json.groups[0]).toMatchObject({
      name: "Opus",
      aliases: ["gpt-4o", "gpt-4"],
      targets: [
        { model: "claude-code/claude-opus-5", account_id: null, account_label: null },
        { model: "grok/grok-4.5", account_id: null, account_label: null },
      ],
    })
  })

  it("never lists another user's groups", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    seedUser(db, "user_2")
    const env = buildEnv(db)
    const cookie1 = await cookieFor(env, "user_1")
    const cookie2 = await cookieFor(env, "user_2")
    await createGroup(env, cookie1)

    const res = await modelGroupRoutes.request("/", req("GET", cookie2), env)
    const json = await readJson(res)
    expect(json.groups).toHaveLength(0)
  })

  it("reports stored-state routing for resolved, unresolved, empty and pooled targets", async () => {
    const db = new FakeD1()
    seedUser(db)
    seedCustomProvider(db, "user_1", "deleted-endpoint")
    seedCustomProvider(db, "user_1", "empty-endpoint")
    seedAccount(db, { id: "limited", userId: "user_1", provider: "claude-code" })
    seedAccount(db, { id: "healthy", userId: "user_1", provider: "grok" })
    seedAccount(db, { id: "pool-limited", userId: "user_1", provider: "codex" })
    seedAccount(db, { id: "pool-healthy", userId: "user_1", provider: "codex" })
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const reset = new Date(Date.now() + 60_000).toISOString()
    for (const id of ["limited", "pool-limited"]) {
      db.rows("upstream_accounts").find((row) => row.id === id)!.usage_snapshot_json = JSON.stringify({
        windows: [{ utilization: 100, resets_at: reset }], error: null, stale: false, edgeBlocked: false,
      })
    }
    await createGroup(env, cookie, {
      targets: [
        { model: "claude-code/first", account_id: "limited" },
        { model: "grok/second", account_id: "healthy" },
        "deleted-endpoint/model",
        "codex/pooled",
      ],
    })
    // Preserve formerly valid stored targets the write-time validator would reject today.
    db.rows("custom_providers").splice(0, 1)
    const group = db.rows("model_groups")[0]!
    group.targets_json = JSON.stringify([
      { model: "claude-code/first", account_id: "limited" },
      { model: "grok/second", account_id: "healthy" },
      { model: "deleted-endpoint/model", account_id: null },
      { model: "claude-code/missing", account_id: "gone" },
      { model: "codex/pooled", account_id: null },
      { model: "empty-endpoint/no-pool", account_id: null },
    ])

    const json = await readJson(await modelGroupRoutes.request("/", req("GET", cookie), env))
    expect(json.groups[0].routing).toMatchObject({
      current_target_index: 1,
      targets: [
        { usable: false, reason: "limit", unusable_until: reset },
        { usable: true, reason: null, unusable_until: null },
        { usable: false, reason: "unresolved", unusable_until: null },
        { usable: false, reason: "no_account", unusable_until: null },
        { usable: true, reason: null, unusable_until: null },
        { usable: false, reason: "no_account", unusable_until: null },
      ],
    })
  })

  it("labels a longer bench as benched when both bench and limit apply", async () => {
    const db = new FakeD1()
    seedUser(db)
    seedAccount(db, { id: "acc", userId: "user_1", provider: "claude-code" })
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const reset = Date.now() + 60_000
    db.rows("upstream_accounts")[0]!.usage_snapshot_json = JSON.stringify({
      windows: [{ utilization: 100, resets_at: new Date(reset).toISOString() }], error: null, stale: false, edgeBlocked: false,
    })
    db.rows("upstream_accounts")[0]!.bench_until = new Date(reset + 60_000).toISOString()
    await createGroup(env, cookie, { targets: [{ model: "claude-code/model", account_id: "acc" }] })

    const json = await readJson(await modelGroupRoutes.request("/", req("GET", cookie), env))
    expect(json.groups[0].routing.targets[0]).toMatchObject({ usable: false, reason: "benched" })
  })
})

describe("POST /api/model-groups (create)", () => {
  it("requires auth", async () => {
    const db = new FakeD1()
    const res = await modelGroupRoutes.request("/", { method: "POST" }, buildEnv(db))
    expect(res.status).toBe(401)
  })

  it("creates a valid group and returns 201", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie)
    expect(res.status).toBe(201)
    const json = await readJson(res)
    expect(json).toMatchObject({
      name: "Opus",
      aliases: ["opus"],
      targets: [{ model: "claude-code/claude-opus-5", account_id: null, account_label: null }],
    })
    expect(typeof json.id).toBe("string")
  })

  describe("display name — free text since 0009_model_group_aliases.sql (not a callable id)", () => {
    it("allows whitespace in the display name", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const res = await createGroup(env, cookie, { name: "OpenAI GPT-4o family" })
      expect(res.status).toBe(201)
      const json = await readJson(res)
      expect(json.name).toBe("OpenAI GPT-4o family")
    })

    it("allows '/' in the display name", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const res = await createGroup(env, cookie, { name: "GPT-4o / GPT-4" })
      expect(res.status).toBe(201)
    })

    it("rejects an empty name", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const res = await createGroup(env, cookie, { name: "" })
      expect(res.status).toBe(400)
    })

    it("rejects a name over 64 characters", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const res = await createGroup(env, cookie, { name: "a".repeat(65) })
      expect(res.status).toBe(400)
    })

    it("rejects a duplicate display name for the same user", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      await createGroup(env, cookie)
      // Distinct aliases so the only possible conflict is the name itself.
      const res = await createGroup(env, cookie, { aliases: ["opus-2"], targets: ["grok/grok-4.5"] })
      expect(res.status).toBe(400)
    })

    it("allows the same display name for a different user", async () => {
      const db = new FakeD1()
      seedUser(db, "user_1")
      seedUser(db, "user_2")
      const env = buildEnv(db)
      const cookie1 = await cookieFor(env, "user_1")
      const cookie2 = await cookieFor(env, "user_2")
      await createGroup(env, cookie1)
      const res = await createGroup(env, cookie2)
      expect(res.status).toBe(201)
    })
  })

  describe("aliases", () => {
    it("rejects an empty aliases array", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const res = await createGroup(env, cookie, { aliases: [] })
      expect(res.status).toBe(400)
    })

    it("rejects more than the max aliases", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const res = await createGroup(env, cookie, {
        aliases: Array.from({ length: MAX_ALIASES_PER_GROUP + 1 }, (_, i) => `alias-${i}`),
      })
      expect(res.status).toBe(400)
    })

    it("accepts up to the max aliases", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const res = await createGroup(env, cookie, {
        aliases: Array.from({ length: MAX_ALIASES_PER_GROUP }, (_, i) => `alias-${i}`),
      })
      expect(res.status).toBe(201)
    })

    it("rejects an alias with whitespace", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const res = await createGroup(env, cookie, { aliases: ["my alias"] })
      expect(res.status).toBe(400)
    })

    it("rejects an alias containing '/'", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const res = await createGroup(env, cookie, { aliases: ["claude-code/opus"] })
      expect(res.status).toBe(400)
    })

    it("rejects a duplicate alias within the same payload", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const res = await createGroup(env, cookie, { aliases: ["opus", "opus"] })
      expect(res.status).toBe(400)
    })

    it("accepts multiple distinct aliases — any of them will be callable to the same targets", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const res = await createGroup(env, cookie, { aliases: ["gpt-4o", "gpt-4", "gpt-4-turbo"] })
      expect(res.status).toBe(201)
      const json = await readJson(res)
      expect(json.aliases).toEqual(["gpt-4o", "gpt-4", "gpt-4-turbo"])
    })

    it("rejects a cross-group alias conflict, naming the conflicting alias", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      await createGroup(env, cookie, { name: "Opus", aliases: ["opus"] })
      const res = await createGroup(env, cookie, {
        name: "Also Opus?",
        aliases: ["not-opus", "opus"],
      })
      expect(res.status).toBe(400)
      const json = await readJson(res)
      expect(json.error).toContain("opus")
    })

    it("allows the same alias string for a different user (aliases are unique per user, not globally)", async () => {
      const db = new FakeD1()
      seedUser(db, "user_1")
      seedUser(db, "user_2")
      const env = buildEnv(db)
      const cookie1 = await cookieFor(env, "user_1")
      const cookie2 = await cookieFor(env, "user_2")
      await createGroup(env, cookie1)
      const res = await createGroup(env, cookie2)
      expect(res.status).toBe(201)
    })
  })

  it("rejects an empty targets array", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, { targets: [] })
    expect(res.status).toBe(400)
  })

  it("rejects more than the max targets", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, {
      targets: Array.from({ length: MAX_TARGETS_PER_GROUP + 1 }, (_, i) => `claude-code/m${i}`),
    })
    expect(res.status).toBe(400)
  })

  it("rejects a target with an unknown provider prefix", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, { targets: ["not-a-real-provider/model"] })
    expect(res.status).toBe(400)
  })

  it("rejects a bare-name target (no nesting groups)", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, { targets: ["another-group"] })
    expect(res.status).toBe(400)
  })

  it("rejects duplicate targets", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, {
      targets: ["claude-code/claude-opus-5", "claude-code/claude-opus-5"],
    })
    expect(res.status).toBe(400)
  })

  it("accepts a target whose prefix is the caller's own custom provider slug", async () => {
    const db = new FakeD1()
    seedUser(db)
    seedCustomProvider(db, "user_1", "my-endpoint")
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, { targets: ["my-endpoint/gpt-4o"] })
    expect(res.status).toBe(201)
  })

  it("rejects a target whose prefix is another user's custom provider slug", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    seedUser(db, "user_2")
    seedCustomProvider(db, "user_2", "my-endpoint")
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, { targets: ["my-endpoint/gpt-4o"] })
    expect(res.status).toBe(400)
  })

  it("enforces the 50-group-per-user cap", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    for (let i = 0; i < MAX_MODEL_GROUPS_PER_USER; i++) {
      const res = await createGroup(env, cookie, { name: `group-${i}`, aliases: [`alias-${i}`] })
      expect(res.status).toBe(201)
    }
    const res = await createGroup(env, cookie, { name: "one-too-many", aliases: ["one-too-many"] })
    expect(res.status).toBe(400)
  })
})

describe("PUT /api/model-groups/:id (update)", () => {
  it("404s on another user's group id", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    seedUser(db, "user_2")
    const env = buildEnv(db)
    const cookie1 = await cookieFor(env, "user_1")
    const cookie2 = await cookieFor(env, "user_2")
    const created = await readJson(await createGroup(env, cookie1))

    const res = await modelGroupRoutes.request(
      `/${created.id}`,
      req("PUT", cookie2, { name: "hijacked" }),
      env,
    )
    expect(res.status).toBe(404)
  })

  it("404s on an unknown id", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await modelGroupRoutes.request(
      "/mgrp_nonexistent",
      req("PUT", cookie, { name: "x" }),
      env,
    )
    expect(res.status).toBe(404)
  })

  it("renames the display name — unlike a custom provider slug, it's mutable", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const created = await readJson(await createGroup(env, cookie))

    const res = await modelGroupRoutes.request(
      `/${created.id}`,
      req("PUT", cookie, { name: "Renamed Opus" }),
      env,
    )
    expect(res.status).toBe(200)
    const json = await readJson(res)
    expect(json.name).toBe("Renamed Opus")
    // Untouched — only `name` was in the PUT body.
    expect(json.aliases).toEqual(["opus"])
    expect(json.targets).toEqual([
      { model: "claude-code/claude-opus-5", account_id: null, account_label: null },
    ])
  })

  it("replaces the whole targets list (no per-entry patching)", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const created = await readJson(
      await createGroup(env, cookie, {
        targets: ["claude-code/claude-opus-5", "grok/grok-4.5"],
      }),
    )

    const res = await modelGroupRoutes.request(
      `/${created.id}`,
      req("PUT", cookie, { targets: ["codex/gpt-5.2"] }),
      env,
    )
    expect(res.status).toBe(200)
    const json = await readJson(res)
    expect(json.targets).toEqual([{ model: "codex/gpt-5.2", account_id: null, account_label: null }])
    expect(json.name).toBe("Opus")
  })

  it("rejects renaming to a display name already used by another of the caller's groups", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    await createGroup(env, cookie, { name: "Existing", aliases: ["existing"] })
    const created = await readJson(
      await createGroup(env, cookie, { name: "Renameable", aliases: ["renameable"] }),
    )

    const res = await modelGroupRoutes.request(
      `/${created.id}`,
      req("PUT", cookie, { name: "Existing" }),
      env,
    )
    expect(res.status).toBe(400)
  })

  it("rejects invalid targets on update the same way as create", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const created = await readJson(await createGroup(env, cookie))
    const res = await modelGroupRoutes.request(
      `/${created.id}`,
      req("PUT", cookie, { targets: ["bogus-provider/model"] }),
      env,
    )
    expect(res.status).toBe(400)
  })

  describe("aliases — replace-whole-list semantics", () => {
    it("rejects invalid aliases on update the same way as create", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const created = await readJson(await createGroup(env, cookie))
      const res = await modelGroupRoutes.request(
        `/${created.id}`,
        req("PUT", cookie, { aliases: [] }),
        env,
      )
      expect(res.status).toBe(400)
    })

    it("replaces the whole alias list — an alias omitted from the new list is dropped", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const created = await readJson(
        await createGroup(env, cookie, { aliases: ["gpt-4o", "gpt-4"] }),
      )

      const res = await modelGroupRoutes.request(
        `/${created.id}`,
        req("PUT", cookie, { aliases: ["gpt-4-turbo"] }),
        env,
      )
      expect(res.status).toBe(200)
      const json = await readJson(res)
      expect(json.aliases).toEqual(["gpt-4-turbo"])
    })

    it("a dropped alias becomes available again — a new group can claim it", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const created = await readJson(
        await createGroup(env, cookie, { name: "Opus", aliases: ["opus", "gpt-4o"] }),
      )
      await modelGroupRoutes.request(
        `/${created.id}`,
        req("PUT", cookie, { aliases: ["opus"] }),
        env,
      )

      // "gpt-4o" is no longer used by any group — a second group may claim it.
      const res = await createGroup(env, cookie, { name: "GPT-4o", aliases: ["gpt-4o"] })
      expect(res.status).toBe(201)
    })

    it("self-conflict-free: replacing with a superset of the group's own current aliases does not 400", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const created = await readJson(await createGroup(env, cookie, { aliases: ["opus"] }))

      const res = await modelGroupRoutes.request(
        `/${created.id}`,
        req("PUT", cookie, { aliases: ["opus", "opus-2"] }),
        env,
      )
      expect(res.status).toBe(200)
      const json = await readJson(res)
      expect(json.aliases).toEqual(["opus", "opus-2"])
    })

    it("rejects a cross-group alias conflict on update, naming the conflicting alias", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      await createGroup(env, cookie, { name: "Other", aliases: ["taken"] })
      const created = await readJson(
        await createGroup(env, cookie, { name: "Mine", aliases: ["mine"] }),
      )

      const res = await modelGroupRoutes.request(
        `/${created.id}`,
        req("PUT", cookie, { aliases: ["taken"] }),
        env,
      )
      expect(res.status).toBe(400)
      const json = await readJson(res)
      expect(json.error).toContain("taken")

      // Not partially applied — the group's aliases are unchanged.
      const list = await readJson(await modelGroupRoutes.request("/", req("GET", cookie), env))
      const mine = list.groups.find((g: any) => g.id === created.id)
      expect(mine.aliases).toEqual(["mine"])
    })
  })
})

describe("account pinning (docs/auth.md § Model groups, docs/providers.md § Model groups \"Account pinning\")", () => {
  it("accepts a target pinned to an account the caller owns whose provider matches", async () => {
    const db = new FakeD1()
    seedUser(db)
    seedAccount(db, { id: "acc_1", userId: "user_1", provider: "claude-code" })
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, {
      targets: [{ model: "claude-code/claude-opus-5", account_id: "acc_1" }],
    })
    expect(res.status).toBe(201)
    const json = await readJson(res)
    expect(json.targets).toEqual([
      { model: "claude-code/claude-opus-5", account_id: "acc_1", account_label: null },
    ])
  })

  it("rejects an account_id belonging to another user (foreign account) with 400", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    seedUser(db, "user_2")
    seedAccount(db, { id: "acc_other", userId: "user_2", provider: "claude-code" })
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, {
      targets: [{ model: "claude-code/claude-opus-5", account_id: "acc_other" }],
    })
    expect(res.status).toBe(400)
  })

  it("rejects an account_id whose provider doesn't match the target's prefix with 400", async () => {
    const db = new FakeD1()
    seedUser(db)
    seedAccount(db, { id: "acc_grok", userId: "user_1", provider: "grok" })
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, {
      // acc_grok belongs to "grok", not "claude-code" — mismatch.
      targets: [{ model: "claude-code/claude-opus-5", account_id: "acc_grok" }],
    })
    expect(res.status).toBe(400)
  })

  it("rejects a nonexistent account_id with 400", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, {
      targets: [{ model: "claude-code/claude-opus-5", account_id: "acc_never_existed" }],
    })
    expect(res.status).toBe(400)
  })

  it("allows the same model pinned to two different accounts as two targets", async () => {
    const db = new FakeD1()
    seedUser(db)
    seedAccount(db, { id: "acc_1", userId: "user_1", provider: "claude-code" })
    seedAccount(db, { id: "acc_2", userId: "user_1", provider: "claude-code" })
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, {
      targets: [
        { model: "claude-code/claude-opus-5", account_id: "acc_1" },
        { model: "claude-code/claude-opus-5", account_id: "acc_2" },
      ],
    })
    expect(res.status).toBe(201)
    const json = await readJson(res)
    expect(json.targets).toHaveLength(2)
  })

  it("rejects a duplicate (model, account_id) pair with 400", async () => {
    const db = new FakeD1()
    seedUser(db)
    seedAccount(db, { id: "acc_1", userId: "user_1", provider: "claude-code" })
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, {
      targets: [
        { model: "claude-code/claude-opus-5", account_id: "acc_1" },
        { model: "claude-code/claude-opus-5", account_id: "acc_1" },
      ],
    })
    expect(res.status).toBe(400)
  })

  it("PUT validates a pinned account_id the same way as create", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const created = await readJson(await createGroup(env, cookie))
    const res = await modelGroupRoutes.request(
      `/${created.id}`,
      req("PUT", cookie, {
        targets: [{ model: "claude-code/claude-opus-5", account_id: "acc_never_existed" }],
      }),
      env,
    )
    expect(res.status).toBe(400)
  })

  describe("account_label enrichment (read-time, never stored)", () => {
    it("custom_label wins over upstream label", async () => {
      const db = new FakeD1()
      seedUser(db)
      seedAccount(db, {
        id: "acc_1",
        userId: "user_1",
        provider: "claude-code",
        label: "upstream@example.com",
        customLabel: "My Opus Account",
      })
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const res = await createGroup(env, cookie, {
        targets: [{ model: "claude-code/claude-opus-5", account_id: "acc_1" }],
      })
      const json = await readJson(res)
      expect(json.targets[0].account_label).toBe("My Opus Account")
    })

    it("falls back to upstream label when there is no custom_label", async () => {
      const db = new FakeD1()
      seedUser(db)
      seedAccount(db, {
        id: "acc_1",
        userId: "user_1",
        provider: "claude-code",
        label: "upstream@example.com",
      })
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const res = await createGroup(env, cookie, {
        targets: [{ model: "claude-code/claude-opus-5", account_id: "acc_1" }],
      })
      const json = await readJson(res)
      expect(json.targets[0].account_label).toBe("upstream@example.com")
    })

    it("is null for an unpinned target", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const res = await createGroup(env, cookie)
      const json = await readJson(res)
      expect(json.targets[0].account_label).toBeNull()
    })

    it("is null when the pinned account has since been deleted — target still carries the stale account_id", async () => {
      const db = new FakeD1()
      seedUser(db)
      seedAccount(db, { id: "acc_1", userId: "user_1", provider: "claude-code", label: "gone@example.com" })
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const created = await readJson(
        await createGroup(env, cookie, {
          targets: [{ model: "claude-code/claude-opus-5", account_id: "acc_1" }],
        }),
      )
      expect(created.targets[0].account_label).toBe("gone@example.com")

      deleteAccount(db, "acc_1")

      const list = await readJson(await modelGroupRoutes.request("/", req("GET", cookie), env))
      const group = list.groups.find((g: any) => g.id === created.id)
      expect(group.targets[0]).toEqual({
        model: "claude-code/claude-opus-5",
        account_id: "acc_1",
        account_label: null,
      })
    })
  })
})

describe("DELETE /api/model-groups/:id", () => {
  it("404s on another user's group id", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    seedUser(db, "user_2")
    const env = buildEnv(db)
    const cookie1 = await cookieFor(env, "user_1")
    const cookie2 = await cookieFor(env, "user_2")
    const created = await readJson(await createGroup(env, cookie1))

    const res = await modelGroupRoutes.request(`/${created.id}`, req("DELETE", cookie2), env)
    expect(res.status).toBe(404)
    // Still present for the owner.
    const list = await readJson(await modelGroupRoutes.request("/", req("GET", cookie1), env))
    expect(list.groups).toHaveLength(1)
  })

  it("deletes the caller's own group", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const created = await readJson(await createGroup(env, cookie))

    const res = await modelGroupRoutes.request(`/${created.id}`, req("DELETE", cookie), env)
    expect(res.status).toBe(200)
    const list = await readJson(await modelGroupRoutes.request("/", req("GET", cookie), env))
    expect(list.groups).toHaveLength(0)
  })
})

describe("strategy (docs/providers.md § Routing module)", () => {
  it("POST without strategy defaults to ordered", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const created = await readJson(await createGroup(env, cookie))
    expect(created.strategy).toBe("ordered")
  })

  it("POST with strategy: 'ordered' is accepted and echoed", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const created = await readJson(await createGroup(env, cookie, { strategy: "ordered" } as any))
    expect(created.strategy).toBe("ordered")
  })

  it("POST with an unknown strategy value is 400 with a field-level message", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, { strategy: "usage-balanced" } as any)
    expect(res.status).toBe(400)
    const json = await readJson(res)
    expect(json.error).toMatch(/strategy/i)
  })

  it("GET lists strategy on every group", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    await createGroup(env, cookie)
    const list = await readJson(await modelGroupRoutes.request("/", req("GET", cookie), env))
    expect(list.groups[0].strategy).toBe("ordered")
  })

  it("PUT roundtrips strategy: 'ordered'", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const created = await readJson(await createGroup(env, cookie))

    const res = await modelGroupRoutes.request(
      `/${created.id}`,
      req("PUT", cookie, { strategy: "ordered" }),
      env,
    )
    expect(res.status).toBe(200)
    const json = await readJson(res)
    expect(json.strategy).toBe("ordered")
  })

  it("PUT with an unknown strategy value is 400 and leaves the stored value untouched", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const created = await readJson(await createGroup(env, cookie))

    const res = await modelGroupRoutes.request(
      `/${created.id}`,
      req("PUT", cookie, { strategy: "spend-aware" }),
      env,
    )
    expect(res.status).toBe(400)
    const list = await readJson(await modelGroupRoutes.request("/", req("GET", cookie), env))
    expect(list.groups[0].strategy).toBe("ordered")
  })

  it("PUT omitting strategy leaves it unchanged", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const created = await readJson(await createGroup(env, cookie))

    const res = await modelGroupRoutes.request(`/${created.id}`, req("PUT", cookie, { name: "Renamed" }), env)
    expect(res.status).toBe(200)
    const json = await readJson(res)
    expect(json.strategy).toBe("ordered")
  })
})
