import { describe, expect, it } from "vitest"
import { modelGroupRoutes } from "../src/routes/model_groups"
import { createSession } from "../src/auth/session"
import { MAX_MODEL_GROUPS_PER_USER, MAX_TARGETS_PER_GROUP } from "../src/utils/model_group"
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
  name: "opus",
  targets: ["claude-code/claude-opus-5"],
}

async function createGroup(
  env: Env,
  cookie: string,
  overrides: Partial<{ name: string; targets: unknown }> = {},
) {
  return modelGroupRoutes.request("/", req("POST", cookie, { ...validCreateBody, ...overrides }), env)
}

describe("GET /api/model-groups", () => {
  it("requires auth", async () => {
    const db = new FakeD1()
    const res = await modelGroupRoutes.request("/", { method: "GET" }, buildEnv(db))
    expect(res.status).toBe(401)
  })

  it("lists the user's groups with targets in priority order", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    await createGroup(env, cookie, { targets: ["claude-code/claude-opus-5", "grok/grok-4.5"] })

    const res = await modelGroupRoutes.request("/", req("GET", cookie), env)
    const json = await readJson(res)
    expect(json.groups).toHaveLength(1)
    expect(json.groups[0]).toMatchObject({
      name: "opus",
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
      name: "opus",
      targets: [{ model: "claude-code/claude-opus-5", account_id: null, account_label: null }],
    })
    expect(typeof json.id).toBe("string")
  })

  it("rejects a name with whitespace", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, { name: "my group" })
    expect(res.status).toBe(400)
  })

  it("rejects a name containing '/'", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, { name: "claude-code/opus" })
    expect(res.status).toBe(400)
  })

  it("rejects an empty name", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, { name: "" })
    expect(res.status).toBe(400)
  })

  it("rejects a duplicate name for the same user", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    await createGroup(env, cookie)
    const res = await createGroup(env, cookie, { targets: ["grok/grok-4.5"] })
    expect(res.status).toBe(400)
  })

  it("allows the same name for a different user", async () => {
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
      const res = await createGroup(env, cookie, { name: `group-${i}` })
      expect(res.status).toBe(201)
    }
    const res = await createGroup(env, cookie, { name: "one-too-many" })
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

  it("renames a group — unlike a custom provider slug, the name is mutable", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const created = await readJson(await createGroup(env, cookie))

    const res = await modelGroupRoutes.request(
      `/${created.id}`,
      req("PUT", cookie, { name: "renamed-opus" }),
      env,
    )
    expect(res.status).toBe(200)
    const json = await readJson(res)
    expect(json.name).toBe("renamed-opus")
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
    expect(json.name).toBe("opus")
  })

  it("rejects renaming to a name already used by another of the caller's groups", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    await createGroup(env, cookie, { name: "existing" })
    const created = await readJson(await createGroup(env, cookie, { name: "renameable" }))

    const res = await modelGroupRoutes.request(
      `/${created.id}`,
      req("PUT", cookie, { name: "existing" }),
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
