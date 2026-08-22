import { describe, expect, it } from "vitest"
import { modelGroupRoutes } from "../src/routes/model_groups"
import { createSession } from "../src/auth/session"
import {
  MAX_MODEL_GROUPS_PER_USER,
  MAX_MODELS_PER_GROUP,
  MAX_TARGETS_PER_MODEL,
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
  slug: "opus-ep",
  models: [{ name: "opus", targets: ["claude-code/claude-opus-5"] }],
}

async function createGroup(
  env: Env,
  cookie: string,
  overrides: Partial<{ name: string; slug: string; models: unknown }> = {},
) {
  return modelGroupRoutes.request("/", req("POST", cookie, { ...validCreateBody, ...overrides }), env)
}

describe("GET /api/model-groups", () => {
  it("requires auth", async () => {
    const db = new FakeD1()
    const res = await modelGroupRoutes.request("/", { method: "GET" }, buildEnv(db))
    expect(res.status).toBe(401)
  })

  it("lists the user's groups with slug and per-model targets in priority order", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    await createGroup(env, cookie, {
      models: [
        { name: "gpt-4o", targets: ["claude-code/claude-opus-5", "grok/grok-4.5"] },
        { name: "gpt-4", targets: ["grok/grok-4.5"] },
      ],
    })

    const res = await modelGroupRoutes.request("/", req("GET", cookie), env)
    const json = await readJson(res)
    expect(json.groups).toHaveLength(1)
    expect(json.groups[0]).toMatchObject({
      name: "Opus",
      slug: "opus-ep",
      models: [
        {
          name: "gpt-4o",
          targets: [
            { model: "claude-code/claude-opus-5", account_id: null, account_label: null },
            { model: "grok/grok-4.5", account_id: null, account_label: null },
          ],
        },
        {
          name: "gpt-4",
          targets: [{ model: "grok/grok-4.5", account_id: null, account_label: null }],
        },
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

  it("reports stored-state routing for resolved, unresolved, empty and pooled targets, per model", async () => {
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
      models: [
        {
          name: "the-model",
          targets: [
            { model: "claude-code/first", account_id: "limited" },
            { model: "grok/second", account_id: "healthy" },
            "deleted-endpoint/model",
            "codex/pooled",
          ],
        },
      ],
    })
    // Preserve formerly valid stored targets the write-time validator would reject today.
    db.rows("custom_providers").splice(0, 1)
    const modelRow = db.rows("model_group_models")[0]!
    modelRow.targets_json = JSON.stringify([
      { model: "claude-code/first", account_id: "limited" },
      { model: "grok/second", account_id: "healthy" },
      { model: "deleted-endpoint/model", account_id: null },
      { model: "claude-code/missing", account_id: "gone" },
      { model: "codex/pooled", account_id: null },
      { model: "empty-endpoint/no-pool", account_id: null },
    ])

    const json = await readJson(await modelGroupRoutes.request("/", req("GET", cookie), env))
    expect(json.groups[0].models[0].routing).toMatchObject({
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
    await createGroup(env, cookie, {
      models: [{ name: "the-model", targets: [{ model: "claude-code/model", account_id: "acc" }] }],
    })

    const json = await readJson(await modelGroupRoutes.request("/", req("GET", cookie), env))
    expect(json.groups[0].models[0].routing.targets[0]).toMatchObject({ usable: false, reason: "benched" })
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
      slug: "opus-ep",
      models: [
        {
          name: "opus",
          targets: [{ model: "claude-code/claude-opus-5", account_id: null, account_label: null }],
        },
      ],
    })
    expect(typeof json.id).toBe("string")
  })

  describe("display name — free text (not part of the URL)", () => {
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

    it("rejects an empty name and a name over 64 characters", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      expect((await createGroup(env, cookie, { name: "" })).status).toBe(400)
      expect((await createGroup(env, cookie, { name: "a".repeat(65) })).status).toBe(400)
    })

    it("rejects a duplicate display name for the same user", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      await createGroup(env, cookie)
      // Distinct slug so the only possible conflict is the name itself.
      const res = await createGroup(env, cookie, { slug: "opus-2" })
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

  describe("slug — the endpoint's URL id (docs/providers.md § Model groups)", () => {
    it("rejects a malformed slug", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      expect((await createGroup(env, cookie, { slug: "UPPER" })).status).toBe(400)
      expect((await createGroup(env, cookie, { slug: "a" })).status).toBe(400)
      expect((await createGroup(env, cookie, { slug: "-lead" })).status).toBe(400)
    })

    it("rejects a slug already used by another of the caller's groups, naming it", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      await createGroup(env, cookie)
      const res = await createGroup(env, cookie, { name: "Other", slug: "opus-ep" })
      expect(res.status).toBe(400)
      const json = await readJson(res)
      expect(json.error).toContain("opus-ep")
    })

    it("allows the same slug for a different user (slugs are unique per user, not globally)", async () => {
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

  describe("models", () => {
    it("rejects an empty models array", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const res = await createGroup(env, cookie, { models: [] })
      expect(res.status).toBe(400)
    })

    it("rejects more than the max models", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const res = await createGroup(env, cookie, {
        models: Array.from({ length: MAX_MODELS_PER_GROUP + 1 }, (_, i) => ({
          name: `model-${i}`,
          targets: ["claude-code/claude-opus-5"],
        })),
      })
      expect(res.status).toBe(400)
    })

    it("accepts up to the max models", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const res = await createGroup(env, cookie, {
        models: Array.from({ length: MAX_MODELS_PER_GROUP }, (_, i) => ({
          name: `model-${i}`,
          targets: ["claude-code/claude-opus-5"],
        })),
      })
      expect(res.status).toBe(201)
    })

    it("rejects a model name with whitespace; accepts one with '/'", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      expect(
        (await createGroup(env, cookie, { models: [{ name: "my model", targets: ["grok/grok-4.5"] }] }))
          .status,
      ).toBe(400)
      expect(
        (
          await createGroup(env, cookie, {
            models: [{ name: "claude-code/claude-opus-5", targets: ["grok/grok-4.5"] }],
          })
        ).status,
      ).toBe(201)
    })

    it("rejects a duplicate model name within the same payload", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const res = await createGroup(env, cookie, {
        models: [
          { name: "opus", targets: ["claude-code/claude-opus-5"] },
          { name: "opus", targets: ["grok/grok-4.5"] },
        ],
      })
      expect(res.status).toBe(400)
    })

    it("allows the same model name in two different groups — the endpoint is the namespace", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      await createGroup(env, cookie)
      const res = await createGroup(env, cookie, { name: "Second", slug: "second-ep" })
      expect(res.status).toBe(201)
    })
  })

  it("rejects a model with an empty targets array", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, { models: [{ name: "opus", targets: [] }] })
    expect(res.status).toBe(400)
  })

  it("rejects more than the max targets on one model", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, {
      models: [
        {
          name: "opus",
          targets: Array.from({ length: MAX_TARGETS_PER_MODEL + 1 }, (_, i) => `claude-code/m${i}`),
        },
      ],
    })
    expect(res.status).toBe(400)
  })

  it("rejects a target with an unknown provider prefix, naming the model", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, {
      models: [{ name: "opus", targets: ["not-a-real-provider/model"] }],
    })
    expect(res.status).toBe(400)
    const json = await readJson(res)
    expect(json.error).toContain('model "opus"')
  })

  it("rejects a bare-name target (no nesting groups)", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, {
      models: [{ name: "opus", targets: ["another-group-model"] }],
    })
    expect(res.status).toBe(400)
  })

  it("rejects duplicate targets within one model", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, {
      models: [{ name: "opus", targets: ["claude-code/claude-opus-5", "claude-code/claude-opus-5"] }],
    })
    expect(res.status).toBe(400)
  })

  it("accepts a target whose prefix is the caller's own custom provider slug", async () => {
    const db = new FakeD1()
    seedUser(db)
    seedCustomProvider(db, "user_1", "my-endpoint")
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, {
      models: [{ name: "opus", targets: ["my-endpoint/gpt-4o"] }],
    })
    expect(res.status).toBe(201)
  })

  it("rejects a target whose prefix is another user's custom provider slug", async () => {
    const db = new FakeD1()
    seedUser(db, "user_1")
    seedUser(db, "user_2")
    seedCustomProvider(db, "user_2", "my-endpoint")
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, {
      models: [{ name: "opus", targets: ["my-endpoint/gpt-4o"] }],
    })
    expect(res.status).toBe(400)
  })

  it("enforces the 50-group-per-user cap", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    for (let i = 0; i < MAX_MODEL_GROUPS_PER_USER; i++) {
      const res = await createGroup(env, cookie, { name: `group-${i}`, slug: `slug-${i}` })
      expect(res.status).toBe(201)
    }
    const res = await createGroup(env, cookie, { name: "one-too-many", slug: "one-too-many" })
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

  it("renames the display name without touching slug or models", async () => {
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
    expect(json.slug).toBe("opus-ep")
    expect(json.models).toHaveLength(1)
    expect(json.models[0].name).toBe("opus")
  })

  it("renames the slug — mutable, moving the endpoint URL (docs/providers.md § Model groups)", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const created = await readJson(await createGroup(env, cookie))

    const res = await modelGroupRoutes.request(
      `/${created.id}`,
      req("PUT", cookie, { slug: "moved-ep" }),
      env,
    )
    expect(res.status).toBe(200)
    const json = await readJson(res)
    expect(json.slug).toBe("moved-ep")
    expect(json.name).toBe("Opus")
  })

  it("rejects renaming the slug onto another of the caller's groups", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    await createGroup(env, cookie, { name: "First", slug: "first-ep" })
    const created = await readJson(await createGroup(env, cookie, { name: "Second", slug: "second-ep" }))

    const res = await modelGroupRoutes.request(
      `/${created.id}`,
      req("PUT", cookie, { slug: "first-ep" }),
      env,
    )
    expect(res.status).toBe(400)
  })

  it("keeping the group's own slug on PUT does not self-conflict", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const created = await readJson(await createGroup(env, cookie))

    const res = await modelGroupRoutes.request(
      `/${created.id}`,
      req("PUT", cookie, { slug: "opus-ep", name: "Still Opus" }),
      env,
    )
    expect(res.status).toBe(200)
  })

  it("replaces the whole model set (no per-entry patching)", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const created = await readJson(
      await createGroup(env, cookie, {
        models: [
          { name: "opus", targets: ["claude-code/claude-opus-5", "grok/grok-4.5"] },
          { name: "sonnet", targets: ["grok/grok-4.5"] },
        ],
      }),
    )

    const res = await modelGroupRoutes.request(
      `/${created.id}`,
      req("PUT", cookie, { models: [{ name: "gpt", targets: ["codex/gpt-5.2"] }] }),
      env,
    )
    expect(res.status).toBe(200)
    const json = await readJson(res)
    expect(json.models).toEqual([
      {
        name: "gpt",
        targets: [{ model: "codex/gpt-5.2", account_id: null, account_label: null }],
        routing: expect.anything(),
      },
    ])
    expect(json.name).toBe("Opus")
  })

  it("rejects renaming to a display name already used by another of the caller's groups", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    await createGroup(env, cookie, { name: "Existing", slug: "existing-ep" })
    const created = await readJson(
      await createGroup(env, cookie, { name: "Renameable", slug: "renameable-ep" }),
    )

    const res = await modelGroupRoutes.request(
      `/${created.id}`,
      req("PUT", cookie, { name: "Existing" }),
      env,
    )
    expect(res.status).toBe(400)
  })

  it("rejects invalid models on update the same way as create, leaving the stored set untouched", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const created = await readJson(await createGroup(env, cookie))
    const res = await modelGroupRoutes.request(
      `/${created.id}`,
      req("PUT", cookie, { models: [{ name: "opus", targets: ["bogus-provider/model"] }] }),
      env,
    )
    expect(res.status).toBe(400)

    const list = await readJson(await modelGroupRoutes.request("/", req("GET", cookie), env))
    expect(list.groups[0].models[0].targets).toEqual([
      { model: "claude-code/claude-opus-5", account_id: null, account_label: null },
    ])
  })
})

describe("account pinning (docs/auth.md § Model groups, docs/providers.md § Model groups \"Account pinning\")", () => {
  function modelsWith(targets: unknown): unknown {
    return [{ name: "opus", targets }]
  }

  it("accepts a target pinned to an account the caller owns whose provider matches", async () => {
    const db = new FakeD1()
    seedUser(db)
    seedAccount(db, { id: "acc_1", userId: "user_1", provider: "claude-code" })
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, {
      models: modelsWith([{ model: "claude-code/claude-opus-5", account_id: "acc_1" }]),
    })
    expect(res.status).toBe(201)
    const json = await readJson(res)
    expect(json.models[0].targets).toEqual([
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
      models: modelsWith([{ model: "claude-code/claude-opus-5", account_id: "acc_other" }]),
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
      models: modelsWith([{ model: "claude-code/claude-opus-5", account_id: "acc_grok" }]),
    })
    expect(res.status).toBe(400)
  })

  it("rejects a nonexistent account_id with 400", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, {
      models: modelsWith([{ model: "claude-code/claude-opus-5", account_id: "acc_never_existed" }]),
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
      models: modelsWith([
        { model: "claude-code/claude-opus-5", account_id: "acc_1" },
        { model: "claude-code/claude-opus-5", account_id: "acc_2" },
      ]),
    })
    expect(res.status).toBe(201)
    const json = await readJson(res)
    expect(json.models[0].targets).toHaveLength(2)
  })

  it("rejects a duplicate (model, account_id) pair with 400", async () => {
    const db = new FakeD1()
    seedUser(db)
    seedAccount(db, { id: "acc_1", userId: "user_1", provider: "claude-code" })
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const res = await createGroup(env, cookie, {
      models: modelsWith([
        { model: "claude-code/claude-opus-5", account_id: "acc_1" },
        { model: "claude-code/claude-opus-5", account_id: "acc_1" },
      ]),
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
        models: modelsWith([{ model: "claude-code/claude-opus-5", account_id: "acc_never_existed" }]),
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
        models: modelsWith([{ model: "claude-code/claude-opus-5", account_id: "acc_1" }]),
      })
      const json = await readJson(res)
      expect(json.models[0].targets[0].account_label).toBe("My Opus Account")
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
        models: modelsWith([{ model: "claude-code/claude-opus-5", account_id: "acc_1" }]),
      })
      const json = await readJson(res)
      expect(json.models[0].targets[0].account_label).toBe("upstream@example.com")
    })

    it("is null for an unpinned target", async () => {
      const db = new FakeD1()
      seedUser(db)
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const res = await createGroup(env, cookie)
      const json = await readJson(res)
      expect(json.models[0].targets[0].account_label).toBeNull()
    })

    it("is null when the pinned account has since been deleted — target still carries the stale account_id", async () => {
      const db = new FakeD1()
      seedUser(db)
      seedAccount(db, { id: "acc_1", userId: "user_1", provider: "claude-code", label: "gone@example.com" })
      const env = buildEnv(db)
      const cookie = await cookieFor(env, "user_1")
      const created = await readJson(
        await createGroup(env, cookie, {
          models: modelsWith([{ model: "claude-code/claude-opus-5", account_id: "acc_1" }]),
        }),
      )
      expect(created.models[0].targets[0].account_label).toBe("gone@example.com")

      deleteAccount(db, "acc_1")

      const list = await readJson(await modelGroupRoutes.request("/", req("GET", cookie), env))
      const group = list.groups.find((g: any) => g.id === created.id)
      expect(group.models[0].targets[0]).toEqual({
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

  it("deletes the caller's own group and its model rows", async () => {
    const db = new FakeD1()
    seedUser(db)
    const env = buildEnv(db)
    const cookie = await cookieFor(env, "user_1")
    const created = await readJson(await createGroup(env, cookie))

    const res = await modelGroupRoutes.request(`/${created.id}`, req("DELETE", cookie), env)
    expect(res.status).toBe(200)
    const list = await readJson(await modelGroupRoutes.request("/", req("GET", cookie), env))
    expect(list.groups).toHaveLength(0)
    expect(db.rows("model_group_models")).toHaveLength(0)
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
