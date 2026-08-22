/**
 * Google Antigravity — Gemini models behind a Google AI Pro / Ultra
 * subscription, reached through the CloudCode internal API
 * (`v1internal:*`). Contract and caveats: docs/providers.md § Antigravity.
 *
 * Wire details are derived from CLIProxyAPI (MIT), which is the source of
 * truth for this undocumented backend — each non-obvious choice cites the file
 * it came from. Both public surfaces are conversions (`proxy/gemini_openai.ts`,
 * `proxy/gemini_anthropic.ts`); there is no native passthrough here.
 */

import { ANTIGRAVITY_OAUTH, antigravityOAuthClient } from "../auth/provider_oauth"
import type { Env } from "../env"
import type { AcquiredAccount, StoredCredential } from "../pool/acquire"
import { saveCredential } from "../pool/acquire"
import { RATELIMIT_RESET_HINT_HEADER } from "../routing/feedback"
import { antigravityBenchUntil, isAntigravityNoCapacity } from "./antigravity_limits"
import { refreshOAuthCredential } from "./refresh"
import type { ChatCompletionRequest, ProviderAdapter, UsageWindow } from "./types"

/**
 * Tried in order. `daily-` is the one the desktop app prefers and is the
 * higher-capacity of the two; `cloudcode-pa` is the fallback. CLIProxyAPI
 * `antigravityBaseURLFallbackOrder` (executor_request.go).
 */
const BASE_URLS = [
  "https://daily-cloudcode-pa.googleapis.com",
  "https://cloudcode-pa.googleapis.com",
] as const

const GENERATE_PATH = "/v1internal:generateContent"
const STREAM_PATH = "/v1internal:streamGenerateContent?alt=sse"
const COUNT_TOKENS_PATH = "/v1internal:countTokens"
const LOAD_CODE_ASSIST_PATH = "/v1internal:loadCodeAssist"
const ONBOARD_USER_PATH = "/v1internal:onboardUser"
const MODELS_PATH = "/v1internal:fetchAvailableModels"

/**
 * `antigravity/hub/<version> <platform>` — CLIProxyAPI `AntigravityUserAgent()`
 * (misc/antigravity_version.go), which fetches the live version from the Hub
 * updater manifest and falls back to a pinned one. This proxy only pins:
 * an extra network call per request to look up a version string is not worth
 * it, and `ANTIGRAVITY_CLIENT_VERSION` exists for when the pin goes stale.
 */
const FALLBACK_CLIENT_VERSION = "2.2.1"
const CLIENT_PLATFORM = "darwin/arm64"
/** The long control-plane UA `onboardUser` expects. */
const NODE_API_CLIENT_UA = "google-api-nodejs-client/10.3.0"
const GOOG_API_CLIENT_UA = "gl-node/22.21.1"

/** Bound side fetches (refresh / models / usage) so a hung Google edge cannot stall the Worker. */
const SIDE_FETCH_TIMEOUT_MS = 10_000

function clientVersion(env: Env): string {
  return env.ANTIGRAVITY_CLIENT_VERSION || FALLBACK_CLIENT_VERSION
}

function userAgent(env: Env): string {
  return `antigravity/hub/${clientVersion(env)} ${CLIENT_PLATFORM}`
}


function apiHeaders(env: Env, accessToken: string, accept: string): Record<string, string> {
  return {
    authorization: `Bearer ${accessToken}`,
    "content-type": "application/json",
    accept,
    "user-agent": userAgent(env),
  }
}

// ── OAuth refresh ──────────────────────────────────────────────────────────

async function refreshAntigravity(env: Env, account: AcquiredAccount): Promise<AcquiredAccount> {
  return refreshOAuthCredential(
    env,
    account,
    (credential) => {
      if (!credential.refresh_token) return false
      const exp = credential.expires_at ? Date.parse(credential.expires_at) : 0
      // Google access tokens live an hour; refresh with five minutes to spare.
      return !exp || exp - 300_000 <= Date.now()
    },
    async (credential) => {
      // Confidential client: Google rejects a refresh without the secret, and
      // this deploy has none unless the operator configured the pair. Keep the
      // stored credential rather than burning the refresh token on a call that
      // cannot succeed — the eventual 401 benches the account with a real cause.
      const configured = antigravityOAuthClient(env)
      if (!configured) return null
      const res = await fetch(credential.token_endpoint || ANTIGRAVITY_OAUTH.tokenUrl, {
        method: "POST",
        headers: { "content-type": "application/x-www-form-urlencoded" },
        body: new URLSearchParams({
          grant_type: "refresh_token",
          refresh_token: credential.refresh_token!,
          client_id: credential.client_id || configured.clientId,
          client_secret: configured.clientSecret,
        }),
        signal: AbortSignal.timeout(SIDE_FETCH_TIMEOUT_MS),
      })
      if (!res.ok) return null
      const json = (await res.json()) as {
        access_token: string
        refresh_token?: string
        expires_in?: number
      }
      return {
        ...credential,
        access_token: json.access_token,
        // Google does not rotate the refresh token on this grant, and omits it
        // from the response — keep the stored one rather than nulling it.
        refresh_token: json.refresh_token ?? credential.refresh_token,
        expires_at: json.expires_in
          ? new Date(Date.now() + json.expires_in * 1000).toISOString()
          : credential.expires_at,
      }
    },
  )
}

// ── CloudCode project bootstrap ────────────────────────────────────────────

export type AntigravityProject = {
  projectId: string
  tierId: string | null
}

function extractProject(payload: unknown): string {
  if (!payload || typeof payload !== "object") return ""
  const data = payload as Record<string, unknown>
  for (const key of ["cloudaicompanionProject", "projectId", "project"]) {
    const value = data[key]
    if (typeof value === "string" && value.trim()) return value.trim()
    if (value && typeof value === "object") {
      const id = (value as { id?: unknown }).id
      if (typeof id === "string" && id.trim()) return id.trim()
    }
  }
  return ""
}

/** `allowedTiers[].isDefault`, else `currentTier.id`, else Google's own free tier id. */
function defaultTierId(payload: unknown): string {
  if (!payload || typeof payload !== "object") return "free-tier"
  const data = payload as { allowedTiers?: unknown; currentTier?: { id?: unknown } }
  if (Array.isArray(data.allowedTiers)) {
    for (const raw of data.allowedTiers) {
      if (!raw || typeof raw !== "object") continue
      const tier = raw as { isDefault?: unknown; id?: unknown }
      if (tier.isDefault === true && typeof tier.id === "string" && tier.id.trim()) {
        return tier.id.trim()
      }
    }
  }
  if (typeof data.currentTier?.id === "string" && data.currentTier.id.trim()) {
    return data.currentTier.id.trim()
  }
  return "free-tier"
}

export type LoadCodeAssist = {
  projectId: string
  tierId: string
  /**
   * `paidTier.availableCredits` Google One AI balance, when the tier has one.
   * The entry's `minimumCreditAmountForUsage` is deliberately not read: it
   * would be a usability gate on credits whose semantics are unverified
   * (docs/providers.md § Antigravity).
   */
  credits: number | null
  paidTierId: string | null
}

/**
 * `v1internal:loadCodeAssist` — the account's CloudCode project, tier, and
 * (for paid tiers) its Google One AI credit balance. CLIProxyAPI
 * `internal/auth/antigravity/auth.go` `FetchProjectID` for the project/tier
 * halves, `antigravity_executor_credits.go` for the credit half.
 */
export async function loadCodeAssist(
  env: Env,
  accessToken: string,
): Promise<LoadCodeAssist> {
  const res = await fetch(`${BASE_URLS[1]}${LOAD_CODE_ASSIST_PATH}`, {
    method: "POST",
    headers: apiHeaders(env, accessToken, "*/*"),
    body: JSON.stringify({ metadata: { ideType: "ANTIGRAVITY" } }),
    signal: AbortSignal.timeout(SIDE_FETCH_TIMEOUT_MS),
  })
  if (!res.ok) {
    const detail = (await res.text()).trim().slice(0, 200) || `HTTP ${res.status}`
    throw new Error(`loadCodeAssist ${res.status}: ${detail}`)
  }
  const json = (await res.json()) as Record<string, unknown>
  const paidTier = json.paidTier as
    | { id?: unknown; availableCredits?: unknown }
    | undefined
  let credits: number | null = null
  if (Array.isArray(paidTier?.availableCredits)) {
    for (const raw of paidTier.availableCredits) {
      if (!raw || typeof raw !== "object") continue
      const credit = raw as { creditType?: unknown; creditAmount?: unknown }
      if (String(credit.creditType ?? "").toUpperCase() !== "GOOGLE_ONE_AI") continue
      const amount = Number(credit.creditAmount)
      if (Number.isFinite(amount)) credits = amount
      break
    }
  }
  // TEMPORARY (added v3.11.2, remove in v3.11.3 — docs/logging.md § Temporary
  // diagnostics). A paid tier reporting no balance is indistinguishable from
  // here between "this plan has no credit pool" and "the pool is under a
  // creditType this parser does not match", and the response is only
  // observable in production. Tier ids and credit amounts only — the payload
  // carries no token, prompt, or completion.
  if (credits === null && paidTier) {
    console.log("[antigravity] no GOOGLE_ONE_AI credit entry", {
      topLevelKeys: Object.keys(json),
      paidTier: JSON.stringify(paidTier).slice(0, 1000),
    })
  }
  return {
    projectId: extractProject(json),
    tierId: defaultTierId(json),
    credits,
    paidTierId: typeof paidTier?.id === "string" ? paidTier.id : null,
  }
}

/**
 * `v1internal:onboardUser` — creates the CloudCode project when
 * `loadCodeAssist` returned none. It is a long-running operation: poll until
 * `done`. CLIProxyAPI polls five times at 2s; a Worker request cannot afford
 * that latency on the dispatch path, so this only ever runs during login.
 */
export async function onboardUser(
  env: Env,
  accessToken: string,
  tierId: string,
): Promise<string> {
  const ua = `${userAgent(env)} ${NODE_API_CLIENT_UA}`
  const body = JSON.stringify({
    tier_id: tierId,
    metadata: {
      ide_type: "ANTIGRAVITY",
      ide_version: clientVersion(env),
      ide_name: "antigravity",
    },
  })
  for (let attempt = 0; attempt < 5; attempt++) {
    const res = await fetch(`${BASE_URLS[0]}${ONBOARD_USER_PATH}`, {
      method: "POST",
      headers: {
        ...apiHeaders(env, accessToken, "*/*"),
        "user-agent": ua,
        "x-goog-api-client": GOOG_API_CLIENT_UA,
      },
      body,
      signal: AbortSignal.timeout(30_000),
    })
    if (!res.ok) {
      const detail = (await res.text()).trim().slice(0, 200) || `HTTP ${res.status}`
      throw new Error(`onboardUser ${res.status}: ${detail}`)
    }
    const json = (await res.json()) as { done?: unknown; response?: unknown }
    if (json.done === true) {
      const projectId = extractProject(json.response)
      if (projectId) return projectId
      throw new Error("onboardUser completed without a project id")
    }
    await new Promise((resolve) => setTimeout(resolve, 2000))
  }
  throw new Error("onboardUser did not complete")
}

/** Project id + tier for a fresh account, run once during login. */
export async function bootstrapAntigravityProject(
  env: Env,
  accessToken: string,
): Promise<AntigravityProject> {
  const loaded = await loadCodeAssist(env, accessToken)
  if (loaded.projectId) return { projectId: loaded.projectId, tierId: loaded.tierId }
  const projectId = await onboardUser(env, accessToken, loaded.tierId)
  return { projectId, tierId: loaded.tierId }
}

function storedProject(credential: StoredCredential): string {
  const value = credential.extra?.project_id
  return typeof value === "string" ? value : ""
}

/**
 * The project id lives inside the encrypted credential payload — it is
 * account-scoped state, not a new column (docs/database.md). An account bound
 * before the id was resolvable (a manual import, or a login that raced the
 * project's creation) fills it in here, once, and persists it.
 */
async function ensureProject(env: Env, account: AcquiredAccount): Promise<AcquiredAccount> {
  if (storedProject(account.credential)) return account
  const { projectId, tierId } = await bootstrapAntigravityProject(
    env,
    account.credential.access_token,
  )
  const credential: StoredCredential = {
    ...account.credential,
    extra: { ...account.credential.extra, project_id: projectId, tier_id: tierId },
  }
  await saveCredential(env, account.row.id, credential)
  return { row: account.row, credential }
}

// ── Upstream request ───────────────────────────────────────────────────────

/**
 * Keeps one conversation on one backend shard so its prompt cache hits.
 * CLIProxyAPI `generateStableSessionID` hashes the first user text part and
 * formats it as a negative int63 — the same derivation, so the same
 * conversation gets the same id whichever proxy sent it.
 */
async function stableSessionId(request: { contents?: Array<{ role?: string; parts?: unknown[] }> }): Promise<string> {
  const first = (request.contents ?? []).find((c) => c.role === "user")
  const text = (first?.parts as Array<{ text?: unknown }> | undefined)?.find(
    (p) => typeof p?.text === "string" && p.text,
  )?.text
  if (typeof text !== "string") {
    return `-${Math.floor(Math.random() * 9_000_000_000_000_000)}`
  }
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text))
  const view = new DataView(digest)
  // Top 8 bytes big-endian, masked to 63 bits — BigInt because the value
  // exceeds Number.MAX_SAFE_INTEGER.
  const value = view.getBigUint64(0, false) & 0x7fffffffffffffffn
  return `-${value}`
}

export type AntigravityEnvelope = Record<string, unknown>

/**
 * Wraps a Gemini request in the CloudCode envelope. CLIProxyAPI
 * `geminiToAntigravity` (executor_request.go): the model, project and
 * `userAgent: "antigravity"` sit outside `request`, and `requestType` is
 * `agent` for everything but the image models.
 */
export async function buildAntigravityEnvelope(opts: {
  model: string
  projectId: string
  request: Record<string, unknown>
  sessionId?: string
}): Promise<AntigravityEnvelope> {
  const request = { ...opts.request }
  const isImageModel = opts.model.includes("image")
  if (!request.sessionId) {
    request.sessionId = opts.sessionId || (await stableSessionId(request))
  }

  // Claude models served through Antigravity need VALIDATED function calling
  // when tools are present, and Gemini models reject `maxOutputTokens`
  // alongside tools or a response schema. Both rules are CLIProxyAPI
  // `buildRequest`'s, kept because the backend 400s without them and neither
  // can be probed for free. VALIDATED must not be attached to a tool-less
  // request (e.g. structured output only) — an unattached function-calling
  // config is itself rejected.
  const isClaude = opts.model.toLowerCase().includes("claude")
  const hasTools = Array.isArray(request.tools) && request.tools.length > 0
  const hasSchema =
    hasTools ||
    !!(request.generationConfig as Record<string, unknown> | undefined)?.responseSchema
  if (isClaude) {
    if (hasTools) {
      const existing = (
        request.toolConfig as { functionCallingConfig?: Record<string, unknown> } | undefined
      )?.functionCallingConfig
      const mode = typeof existing?.mode === "string" ? existing.mode : ""
      // VALIDATED replaces only the default AUTO. An explicit client choice —
      // NONE (never call) or ANY (forced call, possibly with
      // allowedFunctionNames) — is a semantic the proxy must not overwrite:
      // doing so could produce a tool call the client prohibited, or text
      // where a call was required.
      if (!mode || mode === "AUTO") {
        request.toolConfig = {
          ...(request.toolConfig as Record<string, unknown> | undefined),
          functionCallingConfig: { ...(existing ?? {}), mode: "VALIDATED" },
        }
      }
    }
  } else if (hasSchema && request.generationConfig) {
    const { maxOutputTokens: _dropped, ...rest } = request.generationConfig as Record<
      string,
      unknown
    >
    request.generationConfig = rest
  }

  return {
    model: opts.model,
    ...(opts.projectId ? { project: opts.projectId } : {}),
    userAgent: "antigravity",
    requestType: isImageModel ? "image_gen" : "agent",
    requestId: `agent-${crypto.randomUUID()}`,
    request,
  }
}

type UpstreamResult = { response: Response; body: string | null }

/**
 * POST to the first base URL that answers, falling back on a transport error,
 * a 429, or an explicit "no capacity". The fallback list is the only retry:
 * a 429 that survives both base URLs is the routing module's problem, not a
 * loop this adapter runs (docs/providers.md § Antigravity).
 */
async function postWithFallback(
  env: Env,
  accessToken: string,
  path: string,
  body: unknown,
  opts: { stream: boolean; signal?: AbortSignal },
): Promise<UpstreamResult> {
  const accept = opts.stream ? "text/event-stream" : "*/*"
  const payload = JSON.stringify(body)
  let last: UpstreamResult | null = null
  let lastError: unknown = null

  for (let i = 0; i < BASE_URLS.length; i++) {
    let res: Response
    try {
      res = await fetch(`${BASE_URLS[i]}${path}`, {
        method: "POST",
        headers: apiHeaders(env, accessToken, accept),
        body: payload,
        signal: opts.signal,
      })
    } catch (e) {
      // A transport failure never wins over an earlier real HTTP answer: the
      // saved response still carries the body dispatch needs for feedback
      // (bench + candidate failover), which a thrown error would erase.
      lastError = e
      continue
    }

    if (res.ok) return { response: res, body: null }

    // Read the error body once: it decides both the fallback and, for a 429,
    // how long the account is benched.
    const text = await res.text()
    last = { response: res, body: text }
    if (i + 1 >= BASE_URLS.length) break
    let parsed: unknown = null
    try {
      parsed = JSON.parse(text)
    } catch {
      /* a non-JSON error body cannot say "no capacity" */
    }
    if (res.status === 429 || isAntigravityNoCapacity(res.status, parsed)) continue
    break
  }

  if (last) return last
  throw lastError ?? new Error("antigravity: no base url available")
}

/**
 * Rebuilds a failed upstream response for dispatch, attaching the reset hint
 * when the 429 body says how long to wait. The body is passed through
 * verbatim so the client sees Google's own error.
 */
function errorResponse(result: UpstreamResult): Response {
  const { response, body } = result
  const headers = new Headers({
    "content-type": response.headers.get("content-type") || "application/json",
  })
  if (response.status === 429 && body) {
    let parsed: unknown = null
    try {
      parsed = JSON.parse(body)
    } catch {
      /* keep the default bench */
    }
    if (isAntigravityNoCapacity(response.status, parsed)) {
      // "No capacity" is a fleet-side condition, not this credential's fault
      // (docs/providers.md § Antigravity): a 429 that survived both base URLs
      // must not bench the account for the routing module's 300s default.
      // An already-expired reset hint keeps the ordinary failover walk (try
      // the next candidate now) while making the recorded bench a no-op.
      headers.set(RATELIMIT_RESET_HINT_HEADER, String(Date.now()))
    } else {
      const until = antigravityBenchUntil(parsed)
      if (until !== null) headers.set(RATELIMIT_RESET_HINT_HEADER, String(until))
    }
  }
  return new Response(body ?? "", { status: response.status, headers })
}

// ── Adapter ────────────────────────────────────────────────────────────────

export const antigravityAdapter: ProviderAdapter = {
  id: "antigravity",

  async refreshIfNeeded(env, account) {
    // Project bootstrap is part of making this credential usable, so it runs
    // here: a bootstrap failure (loadCodeAssist / onboardUser rejecting)
    // stays account-scoped — dispatch skips the candidate and continues the
    // walk, same as an unreadable credential — instead of throwing inside the
    // request method and turning into a request-terminal 502 that blocks
    // every account after this one in the pool.
    return ensureProject(env, await refreshAntigravity(env, account))
  },

  /**
   * `v1internal:fetchAvailableModels` — a live map of `{id: {displayName, …}}`.
   * There is no offline mirror for this list and no hard-coded fallback: a
   * failure returns empty plus the error, never an invented catalog.
   */
  async listModels(env, account) {
    let acc = await refreshAntigravity(env, account)
    // The catalog is normally how a user discovers a callable model, so an
    // account whose login-time project bootstrap failed retries it here —
    // otherwise it would stay stuck on "models fetch failed" until some
    // known-id chat request happened to run the bootstrap first. A bootstrap
    // failure is still tolerated: the fetch below may work without a project.
    try {
      acc = await ensureProject(env, acc)
    } catch {
      /* keep going without a project id */
    }
    const projectId = storedProject(acc.credential)
    for (const base of BASE_URLS) {
      try {
        const res = await fetch(`${base}${MODELS_PATH}`, {
          method: "POST",
          headers: apiHeaders(env, acc.credential.access_token, "*/*"),
          body: JSON.stringify(projectId ? { project: projectId } : {}),
          signal: AbortSignal.timeout(SIDE_FETCH_TIMEOUT_MS),
        })
        if (!res.ok) continue
        const json = (await res.json()) as { models?: Record<string, { displayName?: unknown }> }
        // An array here means the undocumented endpoint changed shape —
        // treating it as a map would publish the array indexes ("0", "1", …)
        // as callable model ids and cache that invented catalog for an hour.
        if (!json.models || typeof json.models !== "object" || Array.isArray(json.models)) continue
        const models = Object.entries(json.models)
          .filter(([id]) => id.trim())
          .map(([id, info]) => ({
            id,
            display_name:
              typeof info?.displayName === "string" && info.displayName ? info.displayName : null,
          }))
        // A well-formed `{models: {}}` is a real answer — an account with no
        // currently available models — not an upstream failure to retry or
        // cache as an error for an hour.
        return { models, error: null }
      } catch {
        continue
      }
    }
    return { models: [], error: "models fetch failed" }
  },

  /** `/openai/v1`: Chat Completions ↔ Gemini `GenerateContent`. */
  async chatCompletions(env, account, req, extras) {
    const acc = await ensureProject(env, await refreshAntigravity(env, account))
    const { openaiToGeminiRequest, geminiResponseToOpenAI, geminiSseToOpenAIStream } =
      await import("../proxy/gemini_openai")

    const envelope = await buildAntigravityEnvelope({
      model: req.upstreamModel,
      projectId: storedProject(acc.credential),
      request: openaiToGeminiRequest(req) as unknown as Record<string, unknown>,
      sessionId: req.prompt_cache_key || req.affinity?.convId,
    })

    const stream = !!req.stream
    const result = await postWithFallback(
      env,
      acc.credential.access_token,
      stream ? STREAM_PATH : GENERATE_PATH,
      envelope,
      { stream, signal: extras?.signal },
    )
    if (!result.response.ok) return errorResponse(result)

    if (stream) {
      if (!result.response.body) return errorResponse({ response: result.response, body: "" })
      return new Response(geminiSseToOpenAIStream(result.response.body, req.rawModel), {
        status: 200,
        headers: { "content-type": "text/event-stream; charset=utf-8", "cache-control": "no-cache" },
      })
    }
    const json = await result.response.json()
    return Response.json(geminiResponseToOpenAI(json, req.rawModel))
  },

  /** `/anthropic`: Messages ↔ Gemini `GenerateContent`. A conversion, not a passthrough. */
  async messages(env, account, body, headers, extras) {
    const acc = await ensureProject(env, await refreshAntigravity(env, account))
    const {
      anthropicToGeminiRequest,
      geminiResponseToAnthropic,
      geminiSseToAnthropicStream,
      InvalidGeminiReasoningEffortError,
    } = await import("../proxy/gemini_anthropic")

    const reqBody = body as Record<string, unknown>
    // The route already rewrote `model` to the bare upstream id — used as-is,
    // never re-split: a namespaced upstream id ("org/model") would lose its
    // first segment. The client-visible id it sent rides along in a header so
    // responses echo it back.
    const upstreamModel = String(reqBody.model ?? "")
    const displayModel = headers.get("x-kano-raw-model")?.trim() || `antigravity/${upstreamModel}`

    let converted
    try {
      converted = anthropicToGeminiRequest(reqBody)
    } catch (e) {
      if (e instanceof InvalidGeminiReasoningEffortError) {
        return Response.json(
          {
            type: "error",
            error: { type: "invalid_request_error", message: "invalid reasoning_effort" },
          },
          { status: 400 },
        )
      }
      throw e
    }

    const metadata = reqBody.metadata as { user_id?: unknown } | undefined
    const envelope = await buildAntigravityEnvelope({
      model: upstreamModel,
      projectId: storedProject(acc.credential),
      request: converted.request as unknown as Record<string, unknown>,
      sessionId: typeof metadata?.user_id === "string" ? metadata.user_id : undefined,
    })

    const stream = !!reqBody.stream
    const result = await postWithFallback(
      env,
      acc.credential.access_token,
      stream ? STREAM_PATH : GENERATE_PATH,
      envelope,
      { stream, signal: extras?.signal },
    )
    if (!result.response.ok) return errorResponse(result)

    if (stream) {
      if (!result.response.body) return errorResponse({ response: result.response, body: "" })
      return new Response(
        geminiSseToAnthropicStream(result.response.body, displayModel, {
          thinkingMode: converted.thinkingMode,
        }),
        {
          status: 200,
          headers: {
            "content-type": "text/event-stream; charset=utf-8",
            "cache-control": "no-cache",
          },
        },
      )
    }
    const json = await result.response.json()
    return Response.json(
      geminiResponseToAnthropic(json, displayModel, { thinkingMode: converted.thinkingMode }),
    )
  },

  /**
   * `POST /anthropic/v1/messages/count_tokens` — a real upstream count, not an
   * estimate. `v1internal:countTokens` takes the same envelope minus `model`
   * and `project` (CLIProxyAPI `antigravity_executor_tokens.go`) and answers
   * `{totalTokens}`.
   */
  async countTokens(env, account, body, headers, extras) {
    void headers
    const acc = await ensureProject(env, await refreshAntigravity(env, account))
    const { anthropicToGeminiRequest, InvalidGeminiReasoningEffortError } = await import(
      "../proxy/gemini_anthropic"
    )

    const reqBody = body as Record<string, unknown>
    let converted
    try {
      converted = anthropicToGeminiRequest(reqBody)
    } catch (e) {
      // Same 400 as `messages`: a malformed client field is the client's
      // error, never a 502 "upstream error" for a call that was never made.
      if (e instanceof InvalidGeminiReasoningEffortError) {
        return Response.json(
          {
            type: "error",
            error: { type: "invalid_request_error", message: "invalid reasoning_effort" },
          },
          { status: 400 },
        )
      }
      throw e
    }
    const envelope = await buildAntigravityEnvelope({
      model: String(reqBody.model ?? ""),
      projectId: storedProject(acc.credential),
      request: converted.request as unknown as Record<string, unknown>,
    })
    delete envelope.model
    delete envelope.project

    const result = await postWithFallback(
      env,
      acc.credential.access_token,
      COUNT_TOKENS_PATH,
      envelope,
      { stream: false, signal: extras?.signal },
    )
    if (!result.response.ok) return errorResponse(result)
    const json = (await result.response.json()) as { totalTokens?: unknown }
    const total = json.totalTokens
    // Clients budget context on this number. A 200 whose `totalTokens` is
    // missing or malformed must surface as an upstream error, never as a
    // plausible-but-fabricated `input_tokens: 0`.
    if (typeof total !== "number" || !Number.isFinite(total) || total < 0) {
      return Response.json(
        {
          type: "error",
          error: { type: "api_error", message: "antigravity countTokens returned no totalTokens" },
        },
        { status: 502 },
      )
    }
    return Response.json({ input_tokens: total })
  },

  /**
   * Antigravity publishes **no** percentage-based quota window: `loadCodeAssist`
   * reports the tier and, on paid tiers, a Google One AI credit balance with no
   * total and no reset time. Deriving a utilisation percentage from that would
   * be an invented number, so the windows list stays empty and the tier/credit
   * facts go into the account metadata instead (docs/providers.md
   * § Antigravity) — the UI prints the balance verbatim. Limit handling rides
   * entirely on the 429 classifier; `credits_remaining` gates nothing.
   */
  async fetchUsage(env, account) {
    const acc = await refreshAntigravity(env, account)
    const windows: UsageWindow[] = []
    try {
      const loaded = await loadCodeAssist(env, acc.credential.access_token)
      return {
        windows,
        account: {
          email: acc.credential.email ?? null,
          plan_type: loaded.paidTierId ?? loaded.tierId,
          project_id: loaded.projectId || storedProject(acc.credential) || null,
          // `!== null` and not a truthiness check: a balance of 0 is a fact
          // worth printing, not a missing one.
          ...(loaded.credits !== null ? { credits_remaining: loaded.credits } : {}),
        },
      }
    } catch (e) {
      return {
        windows,
        account: { email: acc.credential.email ?? null },
        stale: true,
        error: e instanceof Error ? e.message : "loadCodeAssist failed",
      }
    }
  },
}
