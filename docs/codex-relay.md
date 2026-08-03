# Codex egress relay (Cloud Run)

**Status: approved 2026-08-03, operator decision.** This is the one sanctioned exception to the Cloudflare-only platform rule in `CLAUDE.md`. Auth is **Cloud Run IAM** (operator's explicit choice over a shared secret). This document is the design the implementation must follow; the original cost/platform analysis it grew from is preserved in [Decision record](#decision-record).

## Why it exists

Every chatgpt.com endpoint this proxy uses returns `403` + an HTML challenge when called from a Cloudflare Worker. The cause is a header rule — chatgpt.com rejects any request carrying `CF-Worker` or `CF-Connecting-IP`, both of which Cloudflare injects into every outbound `fetch()` **after** Worker JS returns, so no Worker-side code can remove them. Full evidence table: [providers.md — "The chatgpt.com wall"](./providers.md#the-chatgptcom-wall--measured-2026-08-03).

The only fix is egress that is not a Cloudflare Worker. The relay is that egress: a stateless byte pipe on Cloud Run that re-originates requests to chatgpt.com.

```text
client → CF Worker (auth, pool, conversion) → Cloud Run relay → chatgpt.com
                                              ↑ sole purpose: egress that is not a Worker
```

Everything smart stays in the Worker: client auth, account pool, failover, format conversion, reasoning replay. The relay holds **no credentials at rest**, keeps **no state**, logs **no content**. Token refresh (`auth.openai.com`) stays direct from the Worker — OAuth binding works from Workers today, which proves only chatgpt.com is walled.

## Wire contract

The relay serves exactly one upstream, hardcoded: `https://chatgpt.com`. It is **not** a general forward proxy — even with a stolen invocation path it can only reach the codex endpoints.

### Request

| Rule | Value |
|---|---|
| URL mapping | `https://<relay>.run.app<path>?<query>` → `https://chatgpt.com<path>?<query>`, query preserved verbatim |
| Allowed methods | `GET`, `POST` |
| Allowed path prefixes | `/backend-api/codex/`, `/backend-api/wham/` (the usage alias lives under `/wham/`) |
| Health | `GET /healthz` → `200 ok` (still behind Cloud Run IAM) |

**Request headers are rebuilt from an allowlist — never copied verbatim.** This is the single most important line in the design. The Worker's own outbound fetch *to the relay* also gets stamped with `CF-Worker` / `CF-Connecting-IP` by Cloudflare, so the inbound request at Cloud Run already carries the poison. Any generic reverse-proxy behavior (nginx `proxy_pass`, Go `httputil.ReverseProxy`, Express middleware) forwards headers verbatim and reproduces the exact `403` this relay exists to escape. The relay therefore constructs the upstream request with **only** these inbound headers:

```text
authorization          # upstream ChatGPT OAuth bearer — passthrough, never stored
chatgpt-account-id
session_id
originator
user-agent
content-type
accept
accept-language
openai-beta
```

plus a forced `accept-encoding: identity` (bytes cross the wire 1:1; no decompress/reframe bookkeeping — bandwidth is not a constraint, see [cost](#cloud-run-configuration-and-cost)). Everything else — `CF-*`, `X-Forwarded-*`, `Via`, `X-Serverless-Authorization`, hop-by-hop headers — is dropped by construction.

### Streaming

- **Response body is a straight pipe.** The upstream body streams through untouched — never `await text()`, never buffered, no `Content-Length` set by hand. This is the streaming rule in `CLAUDE.md` and it is pinned by a test, not by care.
- **Request body is read fully before forwarding.** Deliberate asymmetry: request bodies are bounded JSON (Cloud Run caps HTTP/1 requests at 32 MiB; real prompts are a few MB), and sending a known `Content-Length` avoids chunked-POST ambiguity at OpenAI's edge. The streaming rule protects the SSE *response* path; it is not violated by buffering a bounded request.
- **Cancellation propagates.** Client disconnect (an agent stopped mid-run) aborts the upstream fetch via the request signal, so the upstream stops generating and Cloud Run stops billing the wait.

### Response

Status passes through untouched. Response headers are reduced to `content-type` plus a marker; cookies and upstream framing headers are dropped.

| Header | Meaning |
|---|---|
| `x-relay-upstream: 1` | This response came from chatgpt.com through the relay app. Normal upstream semantics apply, including bench on 401/403/429. |
| `x-relay-fault: <reason>` | The relay itself failed (`path`, `method`, `upstream_unreachable`). Always status **502**. |
| neither | The request never reached the relay app — Cloud Run IAM rejection (expired/invalid ID token) or a platform error. |

**Relay self-errors never use 401/402/403/429.** Those four statuses bench pool accounts in the Worker (`pool/acquire.ts` `FAILOVER_STATUS`); a relay misconfiguration must degrade the codex *route*, not poison codex *account* state.

The Worker applies a tri-state guard to every relay response:

1. `x-relay-upstream` present → treat exactly like a direct upstream response.
2. `x-relay-fault` present → return 502 to the client; log; **do not bench**.
3. Neither, status 401/403 → assume a stale ID token: mint a fresh token, retry **once**; if still marker-less, return 502 as a relay fault. Any other marker-less response → 502 relay fault.

## Auth: Cloud Run IAM

The service deploys with `--no-allow-unauthenticated`. Google Front End validates a Google-signed ID token **before the container is invoked** — unauthenticated probes never start an instance and never bill. The relay app itself contains zero auth code.

Because `Authorization` must carry the upstream ChatGPT bearer token end-to-end, the ID token rides in **`X-Serverless-Authorization`**, which Cloud Run checks (and strips) when both headers are present. This is the documented pattern for proxies whose `Authorization` is spoken for.

- **Identity:** dedicated service account `kano-relay-invoker@<gcp-project>.iam.gserviceaccount.com` with `roles/run.invoker` on this one service only. No other grants.
- **Key custody:** the SA JSON key lives in the Cloudflare secret `CODEX_RELAY_SA_KEY` (and optionally local `.dev.vars`). It is never committed, never stored in GCP beyond the key registry, never on the relay.
- **Minting (Worker side):** build a JWT — header `{alg: RS256, typ: JWT}`, claims `{iss, sub}` = SA email, `aud` = `https://oauth2.googleapis.com/token`, `target_audience` = the relay origin (`CODEX_RELAY_URL`), 1h expiry — sign with the SA private key via WebCrypto, exchange at `https://oauth2.googleapis.com/token` (`grant_type=urn:ietf:params:oauth:grant-type:jwt-bearer`) for an ID token. Cache per isolate in memory; refresh when <5 minutes of validity remain. One mint costs one round trip per isolate per hour.
- **Audience = `CODEX_RELAY_URL` origin.** The default `*.run.app` URL is the audience; attaching a custom domain would silently break minted tokens. Don't attach one — the bare `run.app` hostname also keeps the relay off Cloudflare DNS, per the original constraint.
- **Rotation:** create a new SA key → `wrangler secret put CODEX_RELAY_SA_KEY` → delete the old key in GCP. No relay redeploy needed.

## Worker configuration

| Name | Kind | Meaning |
|---|---|---|
| `CODEX_RELAY_URL` | var | Relay origin, e.g. `https://kano-codex-relay-<hash>-uc.a.run.app`. Also the ID-token audience. |
| `CODEX_RELAY_SA_KEY` | secret | Full SA JSON key for `kano-relay-invoker`. |

Both present → codex traffic (responses, models, usage) routes via the relay. Either missing → direct to chatgpt.com, which is today's behavior (403 from Workers; models fall back to the public mirrors). The open-source defaults ship with the relay off. Release CI carries `CODEX_RELAY_URL` through the generated production config from an optional GitHub repository variable; the secret persists in the Cloudflare secret store across deploys.

## Cloud Run configuration and cost

Service `kano-codex-relay`, region **us-central1** (always-free egress tier is North America, and OpenAI's compute is US — an asia-east1 relay would pay for egress to shave a hop that inference latency dominates anyway).

| Flag | Value | Why |
|---|---|---|
| `--no-allow-unauthenticated` | | IAM at the front door; junk never bills |
| `--timeout` | `3600` | Platform max. Default 300s kills long agentic turns mid-stream with a 504 |
| `--min-instances` | `0` | Scale to zero; accept ~1s cold start on first turn after idle |
| `--max-instances` | `10` | With concurrency 1 this is the concurrent-codex-stream ceiling *and* the runaway-cost fuse; requests past it get a marker-less 429 → Worker guard → non-benching 502 |
| `--concurrency` | `1` | Forced: Cloud Run rejects CPU < 1 with concurrency > 1 (measured 2026-08-03). Instance-per-stream at 0.25 vCPU bills less than shared instances at 1 vCPU |
| `--cpu` / `--memory` | `0.25` / `256Mi` (accepted) | CPU size is the only real cost lever — see below |
| billing mode | request-based (default; **no** `--no-cpu-throttling`) | See correction below |

**Billing model, corrected from the original proposal.** The proposal implied request-based billing makes SSE waits free. It does not: request-based billing charges configured vCPU + memory for **wall-clock time while ≥1 request is active on an instance** — a 10-minute agentic stream is 600 billed seconds. What request-based billing avoids is idle-between-requests time. Mitigations are structural: small vCPU (forwarding uses ~none) and scale to zero. Because the platform ties fractional CPU to `concurrency=1`, each stream runs on its own instance and billing scales per stream: ~11k req/month × ~60s active ≈ 660k stream-seconds × 0.25 vCPU ≈ 165k vCPU-s, inside the 180k vCPU-s free tier (memory: 165k GiB-s vs 360k free). The rejected alternative — 1 vCPU shared at concurrency 80 — would bill ~660k vCPU-s ≈ $10/month at the same load unless streams overlap heavily. Config, not luck, keeps this near $0.

**Egress** (unchanged from the proposal's measurement): ~3.4 GB/month against 1 GB free ≈ **$0.28/month**, of which uploads are ~124× downloads — agentic turns re-send the whole conversation every round, and prompt caching saves provider-side token billing, not bytes on the wire.

## Operations

- **Deploys are manual** (`gcloud run deploy` from `apps/relay` — commands in [deployment.md](./deployment.md#codex-egress-relay-cloud-run)). The relay is not in release CI; its change rate should be ~zero. Run `deno task test` + `deno task check` before any relay deploy.
- **Failure mode:** relay down → codex requests fail 502 with `x-relay-fault` semantics; no accounts benched; every other provider unaffected. The codex route is only as available as the relay — that is the accepted trade.
- **Logs:** Cloud Run request logs (URL, status, latency) only. The relay never logs bodies, tokens, or headers. This matches [logging.md](./logging.md).
- **The rule could change.** If OpenAI later blocks by other means (IP reputation of GCP ranges, TLS fingerprint), the relay may stop helping; if they drop the CF-header rule, the relay becomes dead weight. Either way the Worker's direct path remains the fallback by unsetting `CODEX_RELAY_URL`.

## Testing and verification

All tests run with stubbed fetch — **zero real upstream traffic** (cost-safety rule in `CLAUDE.md`).

Relay (`deno task test` in `apps/relay`):

1. Header allowlist: inbound `CF-Worker` / `CF-Connecting-IP` / `X-Serverless-Authorization` never reach the upstream request; allowlisted headers do; `accept-encoding` forced to `identity`.
2. No-buffering proof: a stub upstream that emits chunk A, then later chunk B — the client must receive A while the upstream stream is still open.
3. Path/method allowlist → `502` + `x-relay-fault`, and `/backend-api/wham/usage` is allowed.
4. Query string preserved; upstream `set-cookie` not forwarded; proxied responses carry `x-relay-upstream`.

Worker (`pnpm --filter api test`):

1. ID-token mint: JWT claims (`iss`, `aud`, `target_audience`), exchange call shape, in-memory cache hit, near-expiry refresh.
2. Tri-state guard: upstream-marker 401 passes through (bench semantics intact); fault-marker → 502 no bench; marker-less 401 → one re-mint + retry → then 502.
3. Wiring: relay configured → requests hit the relay base with `X-Serverless-Authorization` and the upstream bearer in `Authorization`; unconfigured → direct, no serverless header.

**Post-deploy spike (free, one-time per environment):** call the deployed relay with a **deliberately fake** upstream token — expect `401` JSON from OpenAI (proves egress escapes the wall: a Worker-style block would be `403` HTML). Repeat with manual `CF-Worker: test` on the request — still `401` proves the allowlist drops it. Never spike with a real token or a real prompt.

## Risks (accepted at approval)

- **A second egress identity.** chatgpt.com sees the relay's IP; rate-limit behavior may differ from what bench logic models today.
- **Added latency.** One extra hop, US region — first-token latency grows; agentic loops feel it most.
- **A second thing to operate.** Deploys, logs, and failures now live in two places.
- **Precedent.** "Cloudflare only" now has one deliberate exception; any further exception needs its own doc and sign-off, same as this one.

## Decision record

Approved 2026-08-03 with **Option 1 + IAM auth**. Options considered:

- **Option 0 — do nothing:** keep `codex/*` listed but unusable (or hide it). Rejected: codex is a provider the operator wants.
- **Option 1 — Cloud Run relay:** chosen. ~11k req/month vs 2M free; plain HTTP container, no platform event model.
- **Option 2 — GCP e2-micro VM:** strictly worse here — always-on, an OS to patch, no scale-to-zero.

Platforms disqualified because they run on Cloudflare's network and reproduce the identical block: **Vercel Edge** (the named trap — resolves to Cloudflare), Cloudflare Workers themselves. Eligible alternates: AWS Lambda Function URL (bigger free tier, but `RESPONSE_STREAM` mode required and its 6 MB buffered-response cap would silently kill SSE if misconfigured), Deno Deploy, Netlify (a layer over Deno Deploy for no gain). Fly.io is not an option — its permanent free tier ended in 2024.

Auth alternatives: shared secret (simpler, but the container wakes to reject junk and rotation is bilateral) vs **Cloud Run IAM (chosen)** — Google's edge rejects junk before billing, per-service least privilege, at the cost of SA-key custody in Cloudflare secrets and ~80 lines of JWT minting in the Worker.

Original traffic measurement (7 days of `request_logs`, 2,662 agentic requests across working providers as a codex stand-in): upload ~3,363 MB/month, download ~27 MB/month. The 32 logged codex requests were all `403` with zero tokens — the block happens at OpenAI's edge before the model, so the outage cost nothing but availability.
