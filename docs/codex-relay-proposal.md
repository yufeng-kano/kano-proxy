# Proposal: codex egress relay

**Status: proposed, not approved, not implemented.** This document exists so the decision can be made with real numbers instead of guesses. Nothing here is built. If it is rejected, the outcome is that `codex/*` models stay listed but unusable, and that is a legitimate choice — see [Option 0](#option-0-do-nothing).

This crosses the **Cloudflare-only** platform rule in `CLAUDE.md`, which is exactly why it needs sign-off before any code.

## The problem

Every chatgpt.com endpoint returns `403` + an HTML challenge page when called from a Cloudflare Worker. Codex chat does not work on this deploy today.

The cause is a header rule, verified 2026-08-03 — not TLS fingerprinting and not IP reputation, both of which were earlier theories in this repo that measurement disproved. Full evidence table in [providers.md](./providers.md#the-chatgptcom-wall-measured-2026-08-03). The short version:

- chatgpt.com `403`s if and only if `CF-Worker` or `CF-Connecting-IP` is present. The value is irrelevant — empty string, arbitrary domain, arbitrary IP all trigger it.
- Cloudflare injects both **after** the Worker's JS returns, so no code change on our side removes them.
- `cloudflare:sockets` cannot sidestep the HTTP layer: outbound TCP to Cloudflare IP ranges is blocked, and chatgpt.com resolves inside those ranges.

So the only fix is egress that is not a Cloudflare Worker.

## What a relay has to do

Almost nothing, which is the appealing part. Measured from a datacenter IP:

| Request | Result |
|---|---|
| Clean request (what a relay re-originates) | `401` ✅ passed |
| Same, but forwarding `CF-Worker` + `CF-Connecting-IP` | `403` ❌ blocked |
| `X-Forwarded-For` + `Via: 1.1 google` only | `401` ✅ passed |

The third row matters: the headers a relay platform adds on its own (`X-Forwarded-For`, `Via`) are harmless. Only the two CF headers are.

And because a relay **re-originates** the request rather than forwarding headers verbatim, the CF headers simply never exist on the new request. There is no "stripping" step to get wrong — build a clean request and the problem is gone.

## Cost, from this deploy's actual traffic

Estimated from `request_logs` over 7 days (2,662 agentic requests across the working providers, as a stand-in for what codex would carry — codex's own 32 requests are all `403` with zero tokens, so they cannot be used).

| Direction | Volume |
|---|---|
| **Upload** (relay → chatgpt.com, i.e. prompt) | **~3,363 MB/month** |
| Download (relay → Worker, i.e. completion) | ~27 MB/month |
| **Total relay egress** | **~3.4 GB/month** |

**Upload is ~124× download.** That is the shape of agentic traffic: every turn re-sends the whole conversation. Any estimate that only counts response tokens is wrong by two orders of magnitude.

**Prompt caching does not reduce this.** 94% of prompt tokens are cache-reads upstream. That saves money *at the provider*, but the bytes still cross the wire on every request, so they still count as relay egress. Cache hits cut token billing, not bandwidth.

Against GCP's 1 GB/month free egress that is 3.3× over, costing **~$0.28/month** at $0.12/GB. Ten times the traffic would still be ~$3/month. **Egress is not a real constraint here.**

Caveats on the estimate: it is a proxy measurement from other providers, ~4 bytes/token is rough (±50% for CJK and JSON tool arguments), and real codex usage could be higher. None of that changes the conclusion at this order of magnitude.

**The likelier cost trap is CPU, not bandwidth.** Forwarding is I/O-bound and uses almost no CPU, but under CPU-always-allocated billing the whole time spent waiting on an upstream SSE stream is billed. Long agentic turns run for minutes. Use request-based billing.

## Platform options

Three candidates are disqualified before comparison because they run on Cloudflare — picking one would reproduce the exact block we are trying to escape:

| Platform | Resolves to | Verdict |
|---|---|---|
| **Vercel Edge** | `64.29.17.131` | ❌ Edge runtime runs on Cloudflare's network |
| **Cloudflare Workers** | `104.18.13.15` | ❌ obviously |
| Deno Deploy | `34.120.54.55` (GCP) | ✅ eligible |
| Netlify | `13.215.239.219` (AWS) | ✅ eligible |
| Cloud Run | `216.239.34.53` (Google) | ✅ eligible |

Vercel is the trap worth naming explicitly: it looks like a different vendor and is not, for this purpose.

Of the eligible ones:

| | Free tier | Idle cost | Streaming | Notes |
|---|---|---|---|---|
| **Cloud Run** (recommended) | 2M req, 180k vCPU-s, 1 GB egress | **$0** (scales to zero) | Yes, needs enabling | Plain HTTP container; no event model to adapt to |
| AWS Lambda Function URL | 1M req, 400k GB-s, **+100 GiB streaming** | $0 | Requires `RESPONSE_STREAM` invoke mode | Largest free tier; 6 MB response cap applies unless streaming mode is on — that cap would silently kill SSE |
| Deno Deploy | Generous | $0 | Native `ReadableStream` | Smallest amount of code; streaming is first-class |
| Netlify | Generous | $0 | Via Deno Deploy | An extra abstraction layer over Deno for no gain here |

**Recommendation: Cloud Run.** Our traffic is ~11k requests/month against a 2M allowance — under 1%. It is a plain HTTP container, so the relay is a few lines with no platform-specific event model. Lambda's free tier is larger but we are nowhere near either limit, so that advantage is theoretical.

Fly.io is **not** an option: its permanent free tier ended in 2024, and it now starts around $1.94/month. An earlier revision of `providers.md` listed it — that was wrong and has been corrected.

## Non-negotiable implementation constraints

1. **The relay must be a streaming byte pipe.** Pipe the upstream body straight through. Never `await response.text()`, never set `Content-Length`. Buffering the stream violates the streaming rule in `CLAUDE.md` and breaks agentic tool loops. This is the single most common defect in published relay snippets, so it needs a test, not just care.
2. **Raise the request timeout.** Cloud Run defaults to 300s and long agentic turns exceed that; the connection is then closed with a `504` mid-stream. Set it to 3600s.
3. **Request-based billing**, per the CPU note above.
4. **The relay must authenticate.** Otherwise it is an open proxy to anyone who finds the URL. Cloud Run IAM or a shared secret from Cloudflare secrets, checked on every request.
5. **Credentials stay in Cloudflare.** The Worker passes the upstream OAuth token per request; the relay stores nothing at rest. Its role is forwarding, not custody — this keeps tokens' home in D1 + Cloudflare secrets and keeps the relay out of scope for credential compromise.
6. **Keep the relay's hostname off Cloudflare DNS** and scoped to this API path.

## Risks

- **A second egress identity.** The relay's IP is what the upstream sees and may rate-limit independently of everything else. Pool-wide behavior could change in ways the current bench logic does not model.
- **Added latency.** An extra hop, plausibly cross-region (free Cloud Run regions are US). This costs first-token latency, which agentic loops feel more than one-shot chat does.
- **A second thing to operate.** Deploys, logs, and failures now live in two places. The codex path becomes only as available as the relay.
- **The rule could change.** This whole approach exists to route around one vendor's header rule. If OpenAI blocks by other means later, the relay may stop helping. Conversely if they drop the rule, the relay becomes dead weight.
- **Platform-rule precedent.** Approving this weakens "Cloudflare only" as a principle. Worth deciding deliberately rather than by drift.

## Options

### Option 0: do nothing

`codex/*` models stay in the catalog (the list works — it uses the mirror fallback) but every request fails. Zero new infrastructure, zero new operational surface, and the platform rule stays intact.

This is a reasonable choice if codex is not a provider you actually need. Everything else in the proxy is unaffected. The main cost is that the model list advertises models that cannot be called, which is misleading — if we take this option we should consider hiding `codex/*` from the catalog until it works.

### Option 1: Cloud Run relay (recommended)

As specified above.

### Option 2: relay on a VM

A GCP `e2-micro` is always-free in `us-west1` / `us-central1` / `us-east1`. Strictly worse than Cloud Run for this: always-on instead of scale-to-zero, and an OS to patch and monitor. Only preferable if we later need something a container-per-request model cannot do.

## If approved, the first step is a 5-line spike

Before writing any relay logic, deploy a minimal service that makes one request to chatgpt.com with a **deliberately fake token** and confirm it returns `401` JSON rather than `403` HTML.

This is free, takes minutes, and closes the last unverified assumption: **no candidate platform's egress has actually been tested.** The measurements above come from a GitHub Actions runner (Azure). The inference that other clouds behave the same is well-founded — the block is a header rule, not an IP rule — but it remains an inference, and this session has already produced two confident-but-wrong diagnoses that measurement overturned. Verify before building.

Use a fake token for the spike. Never test with real upstream traffic — see the cost-safety rule in `CLAUDE.md`.
