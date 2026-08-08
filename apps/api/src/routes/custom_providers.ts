import { Hono } from "hono"
import type { HonoEnv } from "../auth/session"
import { loadSessionUser } from "../auth/session"
import {
  deleteAccountsForProvider,
  insertAccount,
  listAccounts,
  updateAccountPayload,
  type AccountRow,
} from "../db/accounts"
import {
  countCustomProviders,
  deleteCustomProvider,
  getCustomProviderById,
  getCustomProviderBySlug,
  insertCustomProvider,
  listCustomProviders,
  reorderCustomProviders,
  updateCustomProviderFields,
  type CustomProviderRow,
} from "../db/custom_providers"
import { decryptJson, encryptJson } from "../crypto/token_crypto"
import type { Env } from "../env"
import { isBenched, clearBench } from "../pool/bench"
import type { StoredCredential } from "../pool/acquire"
import {
  MAX_CUSTOM_PROVIDERS_PER_USER,
  isCustomProviderFormat,
  isModelsMode,
  maskApiKey,
  parseManualModels,
  validateApiKey,
  validateBaseUrlLength,
  validateManualModels,
  validateName,
  validateSlug,
  type CustomProviderFormat,
} from "../utils/custom_provider"
import { validateUpstreamBaseUrl } from "../utils/upstream_url"

export const customProviderRoutes = new Hono<HonoEnv>()

async function requireUser(c: {
  env: HonoEnv["Bindings"]
  req: { header: (n: string) => string | undefined }
}) {
  const loaded = await loadSessionUser(c.env, c.req.header("cookie"))
  return loaded?.user ?? null
}

/** Bare lowercase hostnames for the SSRF/loop guard's "own host" check. */
function hostsFromRequest(c: {
  env: HonoEnv["Bindings"]
  req: { header: (n: string) => string | undefined }
}): { requestHost: string | null; appUrlHost: string | null } {
  const hostHeader = c.req.header("host") || c.req.header("x-forwarded-host")
  const requestHost = hostHeader ? hostHeader.split(":")[0]!.toLowerCase() : null
  let appUrlHost: string | null = null
  try {
    appUrlHost = c.env.APP_URL ? new URL(c.env.APP_URL).hostname.toLowerCase() : null
  } catch {
    appUrlHost = null
  }
  return { requestHost, appUrlHost }
}

/** Only two states surfaced for custom cards — no standby/unusable nuance. */
async function computeStatus(
  env: Env,
  userId: string,
  slug: string,
  accounts: AccountRow[],
): Promise<"active" | "benched"> {
  for (const a of accounts) {
    if (!(await isBenched(env, userId, slug, a.id))) return "active"
  }
  return "benched"
}

function keyMaskFromAccount(row: AccountRow | undefined): string | null {
  if (!row?.account_meta_json) return null
  try {
    const meta = JSON.parse(row.account_meta_json) as Record<string, unknown>
    return typeof meta.key_mask === "string" ? meta.key_mask : null
  } catch {
    return null
  }
}

async function toListItem(
  env: Env,
  userId: string,
  row: CustomProviderRow,
): Promise<Record<string, unknown>> {
  const accounts = await listAccounts(env.DB, userId, row.slug)
  const status = await computeStatus(env, userId, row.slug, accounts)
  return {
    id: row.id,
    slug: row.slug,
    name: row.name,
    format: row.format,
    base_url: row.base_url,
    models_mode: row.models_mode,
    manual_models: parseManualModels(row.manual_models_json),
    sort_order: row.sort_order,
    key_mask: keyMaskFromAccount(accounts[0]),
    status,
    created_at: row.created_at,
    updated_at: row.updated_at,
  }
}

customProviderRoutes.get("/", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const rows = await listCustomProviders(c.env.DB, user.id)
  const providers = await Promise.all(rows.map((r) => toListItem(c.env, user.id, r)))
  return c.json({ providers })
})

customProviderRoutes.post("/", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  if (!c.env.TOKEN_ENCRYPTION_KEY) {
    return c.json({ error: "TOKEN_ENCRYPTION_KEY not configured" }, 500)
  }

  let body: Record<string, unknown>
  try {
    body = await c.req.json()
  } catch {
    return c.json({ error: "invalid JSON" }, 400)
  }

  const name = typeof body.name === "string" ? body.name.trim() : ""
  const slug = typeof body.slug === "string" ? body.slug.trim().toLowerCase() : ""
  const format = body.format
  const baseUrlRaw = typeof body.base_url === "string" ? body.base_url.trim() : ""
  const apiKey = typeof body.api_key === "string" ? body.api_key : ""
  const modelsMode = body.models_mode ?? "auto"

  if (!isCustomProviderFormat(format)) {
    return c.json({ error: "format must be 'openai' or 'anthropic'" }, 400)
  }
  const slugErr = validateSlug(slug)
  if (slugErr) return c.json({ error: slugErr }, 400)
  const nameErr = validateName(name)
  if (nameErr) return c.json({ error: nameErr }, 400)
  const keyErr = validateApiKey(apiKey)
  if (keyErr) return c.json({ error: keyErr }, 400)
  if (!isModelsMode(modelsMode)) {
    return c.json({ error: "models_mode must be 'auto' or 'manual'" }, 400)
  }
  const manualRes = validateManualModels(body.manual_models)
  if (!manualRes.ok) return c.json({ error: manualRes.error }, 400)
  const urlLenErr = validateBaseUrlLength(baseUrlRaw)
  if (urlLenErr) return c.json({ error: urlLenErr }, 400)

  const { requestHost, appUrlHost } = hostsFromRequest(c)
  const urlRes = validateUpstreamBaseUrl(baseUrlRaw, { requestHost, appUrlHost })
  if (!urlRes.ok) return c.json({ error: urlRes.error }, 400)

  const count = await countCustomProviders(c.env.DB, user.id)
  if (count >= MAX_CUSTOM_PROVIDERS_PER_USER) {
    return c.json(
      { error: `maximum of ${MAX_CUSTOM_PROVIDERS_PER_USER} custom providers reached` },
      400,
    )
  }

  const existing = await getCustomProviderBySlug(c.env.DB, user.id, slug)
  if (existing) return c.json({ error: `slug "${slug}" is already in use` }, 409)

  const row = await insertCustomProvider(c.env.DB, {
    userId: user.id,
    slug,
    name,
    format,
    baseUrl: urlRes.url,
    modelsMode,
    manualModelsJson: manualRes.models.length ? JSON.stringify(manualRes.models) : null,
  })

  const credential: StoredCredential = { access_token: apiKey }
  const encrypted = await encryptJson(c.env.TOKEN_ENCRYPTION_KEY, credential)
  await insertAccount(c.env.DB, {
    userId: user.id,
    provider: slug,
    encryptedPayload: encrypted,
    label: name,
    accountMetaJson: JSON.stringify({ key_mask: maskApiKey(apiKey) }),
  })

  const item = await toListItem(c.env, user.id, row)
  return c.json(item, 201)
})

customProviderRoutes.put("/order", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)

  let body: unknown
  try {
    body = await c.req.json()
  } catch {
    return c.json({ error: "invalid JSON" }, 400)
  }

  if (!body || typeof body !== "object" || Array.isArray(body)) {
    return c.json({ error: "body must be an object" }, 400)
  }
  const ids = (body as Record<string, unknown>).ids
  if (!Array.isArray(ids) || ids.some((id) => typeof id !== "string")) {
    return c.json({ error: "ids must be an array of strings" }, 400)
  }

  const rows = await listCustomProviders(c.env.DB, user.id)
  const expected = new Set(rows.map((row) => row.id))
  const received = new Set(ids)
  if (
    received.size !== ids.length ||
    received.size !== expected.size ||
    ids.some((id) => !expected.has(id))
  ) {
    return c.json({ error: "ids must list every custom provider exactly once" }, 400)
  }

  await reorderCustomProviders(c.env.DB, user.id, ids)
  const updatedRows = await listCustomProviders(c.env.DB, user.id)
  const providers = await Promise.all(updatedRows.map((r) => toListItem(c.env, user.id, r)))
  return c.json({ providers })
})

customProviderRoutes.put("/:id", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const id = c.req.param("id")
  const existing = await getCustomProviderById(c.env.DB, user.id, id)
  if (!existing) return c.json({ error: "not found" }, 404)

  let body: Record<string, unknown>
  try {
    body = await c.req.json()
  } catch {
    return c.json({ error: "invalid JSON" }, 400)
  }

  if (body.slug !== undefined && body.slug !== existing.slug) {
    return c.json({ error: "slug is immutable" }, 400)
  }
  if (body.format !== undefined && body.format !== existing.format) {
    return c.json({ error: "format is immutable" }, 400)
  }

  let name: string | undefined
  if (body.name !== undefined) {
    name = typeof body.name === "string" ? body.name.trim() : ""
    const err = validateName(name)
    if (err) return c.json({ error: err }, 400)
  }

  let baseUrl: string | undefined
  if (body.base_url !== undefined) {
    const raw = typeof body.base_url === "string" ? body.base_url.trim() : ""
    const lenErr = validateBaseUrlLength(raw)
    if (lenErr) return c.json({ error: lenErr }, 400)
    const { requestHost, appUrlHost } = hostsFromRequest(c)
    const urlRes = validateUpstreamBaseUrl(raw, { requestHost, appUrlHost })
    if (!urlRes.ok) return c.json({ error: urlRes.error }, 400)
    baseUrl = urlRes.url
  }

  let modelsMode: "auto" | "manual" | undefined
  if (body.models_mode !== undefined) {
    if (!isModelsMode(body.models_mode)) {
      return c.json({ error: "models_mode must be 'auto' or 'manual'" }, 400)
    }
    modelsMode = body.models_mode
  }

  let manualModelsJson: string | undefined
  if (body.manual_models !== undefined) {
    const res = validateManualModels(body.manual_models)
    if (!res.ok) return c.json({ error: res.error }, 400)
    manualModelsJson = JSON.stringify(res.models)
  }

  // Blank/omitted api_key means "keep the stored key" — never echoed back,
  // so the admin UI cannot round-trip it, and a no-op edit must not error.
  let apiKey: string | undefined
  if (typeof body.api_key === "string" && body.api_key) {
    const err = validateApiKey(body.api_key)
    if (err) return c.json({ error: err }, 400)
    apiKey = body.api_key
  }

  await updateCustomProviderFields(c.env.DB, id, { name, baseUrl, modelsMode, manualModelsJson })

  if (apiKey) {
    if (!c.env.TOKEN_ENCRYPTION_KEY) {
      return c.json({ error: "TOKEN_ENCRYPTION_KEY not configured" }, 500)
    }
    const rows = await listAccounts(c.env.DB, user.id, existing.slug)
    const credential: StoredCredential = { access_token: apiKey }
    const encrypted = await encryptJson(c.env.TOKEN_ENCRYPTION_KEY, credential)
    const accountMetaJson = JSON.stringify({ key_mask: maskApiKey(apiKey) })
    if (rows[0]) {
      await updateAccountPayload(c.env.DB, rows[0].id, encrypted, { accountMetaJson })
    } else {
      await insertAccount(c.env.DB, {
        userId: user.id,
        provider: existing.slug,
        encryptedPayload: encrypted,
        label: name ?? existing.name,
        accountMetaJson,
      })
    }
  }

  const updated = await getCustomProviderById(c.env.DB, user.id, id)
  const item = await toListItem(c.env, user.id, updated ?? existing)
  return c.json(item)
})

customProviderRoutes.delete("/:id", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)
  const id = c.req.param("id")
  const existing = await getCustomProviderById(c.env.DB, user.id, id)
  if (!existing) return c.json({ error: "not found" }, 404)

  // Accounts first: if this crashes partway, a leftover account row without
  // a provider row is unreachable dead data — the reverse order would leave
  // it silently acquirable by the pool.
  const removedAccounts = await deleteAccountsForProvider(c.env.DB, user.id, existing.slug)
  await deleteCustomProvider(c.env.DB, user.id, id)
  for (const acc of removedAccounts) {
    try {
      await clearBench(c.env, user.id, existing.slug, acc.id)
    } catch {
      /* best-effort */
    }
  }
  return c.json({ ok: true })
})

type TestResult = {
  ok: boolean
  models_count?: number | null
  sample?: string[]
  note?: string
  error?: string
}

/** GET the models endpoint for a format and map the outcome. Never echoes the key or body. */
export async function testCustomProviderConnection(
  format: CustomProviderFormat,
  baseUrl: string,
  apiKey: string,
): Promise<TestResult> {
  const url = format === "anthropic" ? `${baseUrl}/v1/models` : `${baseUrl}/models`
  const headers: Record<string, string> =
    format === "anthropic"
      ? { "x-api-key": apiKey, "anthropic-version": "2023-06-01" }
      : { authorization: `Bearer ${apiKey}` }

  const controller = new AbortController()
  const timeout = setTimeout(() => controller.abort(), 10_000)
  try {
    const res = await fetch(url, { headers, signal: controller.signal })
    if (res.status === 404) {
      return { ok: true, models_count: null, note: "reachable; no models endpoint — use manual model ids" }
    }
    if (res.status === 401 || res.status === 403) {
      return { ok: false, error: `auth rejected (${res.status})` }
    }
    if (!res.ok) {
      return { ok: false, error: `HTTP ${res.status}` }
    }
    const json = (await res.json().catch(() => null)) as { data?: Array<{ id?: string }> } | null
    const ids = (json?.data ?? [])
      .map((m) => (typeof m.id === "string" ? m.id : null))
      .filter((v): v is string => !!v)
    return { ok: true, models_count: ids.length, sample: ids.slice(0, 5) }
  } catch {
    return { ok: false, error: "unreachable/timeout" }
  } finally {
    clearTimeout(timeout)
  }
}

customProviderRoutes.post("/test", async (c) => {
  const user = await requireUser(c)
  if (!user) return c.json({ error: "unauthorized" }, 401)

  let body: Record<string, unknown>
  try {
    body = await c.req.json()
  } catch {
    return c.json({ error: "invalid JSON" }, 400)
  }

  let format: CustomProviderFormat
  let baseUrl: string
  let apiKey: string

  if (typeof body.id === "string" && body.id) {
    const row = await getCustomProviderById(c.env.DB, user.id, body.id)
    if (!row) return c.json({ error: "not found" }, 404)
    format = row.format
    baseUrl =
      typeof body.base_url === "string" && body.base_url.trim() ? body.base_url.trim() : row.base_url
    const accounts = await listAccounts(c.env.DB, user.id, row.slug)
    if (!accounts[0]) return c.json({ ok: false, error: "no stored key for this provider" })
    if (!c.env.TOKEN_ENCRYPTION_KEY) {
      return c.json({ error: "TOKEN_ENCRYPTION_KEY not configured" }, 500)
    }
    const cred = await decryptJson<StoredCredential>(
      c.env.TOKEN_ENCRYPTION_KEY,
      accounts[0].encrypted_payload,
    )
    apiKey = cred.access_token
  } else {
    if (!isCustomProviderFormat(body.format)) {
      return c.json({ error: "format must be 'openai' or 'anthropic'" }, 400)
    }
    format = body.format
    baseUrl = typeof body.base_url === "string" ? body.base_url.trim() : ""
    apiKey = typeof body.api_key === "string" ? body.api_key : ""
    if (!apiKey) return c.json({ error: "api_key is required" }, 400)
  }

  const { requestHost, appUrlHost } = hostsFromRequest(c)
  const urlRes = validateUpstreamBaseUrl(baseUrl, { requestHost, appUrlHost })
  if (!urlRes.ok) return c.json({ error: urlRes.error }, 400)

  const result = await testCustomProviderConnection(format, urlRes.url, apiKey)
  return c.json(result)
})
