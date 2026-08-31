# CLI providers (kano-proxy CLI + reverse tunnel for local LLMs)

**Status: implemented (v4.4.0).** This document remains the contract for the subsystem.

**Scope discipline: no cut-down MVP.** Operator decision 2026-08-30: this ships as specified — device auth with rotating refresh tokens, the full TUI, multi-provider `start`, the `/cli` page, the distribution pipeline — not a reduced first pass. This document is the contract; the only sanctioned deferrals are the ones marked *future work* inline (currently: credit-based flow control). If implementation reality forces a change, this doc changes first (docs-first rule in `CLAUDE.md`), never the scope silently.

Lets a user expose an LLM server running on their own machine (Ollama, LM Studio, vLLM, llama.cpp — anything OpenAI- or Anthropic-compatible) as a kano-proxy provider **without a public address**: the official `kano-proxy` CLI dials **out** to the Worker over WebSocket, and the Worker routes provider traffic back down that connection. This is a product feature for all users, so it assumes nothing installed on the user's machine beyond the CLI itself — no cloudflared, no ngrok, no port forwarding.

Terminology: this subsystem is the **CLI provider** feature and its transport is the **agent tunnel** (wire namespace `/agent/v1/*`). "Relay" in this repo always means the Codex egress relay on Cloud Run ([codex-relay.md](./codex-relay.md)), which is unrelated (opposite direction: that is Worker egress; this is user ingress).

```text
kano-proxy CLI ──wss (outbound dial)──▶ Worker /agent/v1/connect ──▶ AgentTunnel DO
                                                                        ▲ holds the socket (hibernatable)
request path:
client ─▶ Worker (auth, routing, conversion) ─▶ AgentTunnel DO ─▶ WS frames ─▶ CLI ─▶ http://localhost:11434/v1
                                              ◀────── response streamed frame-by-frame ──────◀
```

Everything smart stays in the Worker, same doctrine as the codex relay: client auth, pool, failover, format conversion. The DO is a byte pipe with a request multiplexer; the CLI is a byte pipe with a dialer.

## Why a built-in tunnel and not a public-URL shim

A thin alternative exists — CLI auto-downloads `cloudflared`, opens a quick tunnel, registers the `trycloudflare.com` URL as an ordinary custom provider. Rejected for the public product: the URL is world-reachable (attack surface guarded only by a shim token), rotates on every restart, rides a no-SLA free service, and makes us ship and verify a third-party binary. The built-in tunnel costs a real subsystem but gives: zero public URL, auth inside our own token system, one dependency-free binary, all on Cloudflare (the Durable Object satisfies the stack rule's "only if coordination requires" — a persistent per-provider socket **is** coordination state).

## Concepts — a first-class provider kind, not a custom-provider variant

**Operator decision 2026-08-30: CLI providers are their own kind**, not `custom_providers` rows with a transport flag. Folding them into custom was rejected because everything about the fit was a contortion — a sentinel `base_url` that is never fetched, a rejected `count_tokens_url`, a placeholder API key, and above all the `models_mode` auto/manual dichotomy, which exists only because a custom provider's catalog must be *pulled* from a URL that may be unreachable. A CLI provider's catalog is *pushed* by the agent (below), so that dichotomy simply does not apply to it.

Two entities:

- **CLI device** — one machine that ran `kano-proxy init`: a name, a rotating refresh token, a revocation switch. Shown on the web UI's CLI page. A device belongs to one user and can serve any of that user's CLI providers.
- **CLI provider** — one local endpoint registered by `kano-proxy add`: slug, wire format, and the agent-reported model list. On the routing surface it behaves like any provider: model ids are `<slug>/<model>`, it can be a group target, it participates in bench/failover.

## Data model

Three new tables (`0015_cli_providers.sql`; full column notes in [database.md](./database.md); `custom_providers` is untouched):

| Table | Columns (essentials) |
|---|---|
| `cli_devices` | `id`, `user_id`, `name` (1–64 chars), `refresh_token_hash`, `refresh_token_prev_hash` (superseded-token theft detection — see Device auth below), `last_seen_at`, `created_at`, `revoked_at` |
| `cli_login_requests` | `id`, `device_name`, `code_hash`, `user_id` (NULL until approved), `expires_at`, `approved_at`, `used_at`, `attempts`, `created_at` |
| `cli_providers` | `id`, `user_id`, `device_id` (nullable — informational "registered from"), `slug`, `name`, `format` (`openai` \| `anthropic`), `models_json` (last agent report), `models_updated_at`, `model_filter_json` (nullable expose-whitelist), `sort_order`, `created_at`, `updated_at` |

- **Slug rules:** identical to custom providers (lowercase 2–32, `^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$`, same reserved list), **immutable**, and unique per user **across both** `custom_providers` and `cli_providers` — both create paths check the other table, because both kinds resolve from the same `<slug>/<model>` position.
- **Format immutable** after creation, same as custom (delete-and-recreate).
- **Pool internals:** each CLI provider still gets one `upstream_accounts` row (`provider = slug`, empty credential). This is an implementation detail, not a user-facing "account": it is where the existing bench/unpause/routing-facts machinery keeps its state, so `facts.ts` / `feedback.ts` / group targets need no parallel bookkeeping. The row is created and deleted with the provider and never shown as an account in the UI.
- **Caps:** CLI providers count into the same 20-per-user provider budget as custom providers (shared cap across both tables); ≤ 20 devices per user.
- **The target URL is not stored server-side.** Where the local server lives (`http://localhost:11434/v1`) and its optional local API key are CLI state on the device — the server has no business knowing them and could not use them anyway.

## Device auth — login once, rotate forever

No permanent secrets on disk. The CLI holds a **rotating refresh token**; everything it does against the server uses a short-lived **access token** minted from it.

**Login flow (`kano-proxy init`)** — authorize-then-paste, the same shape as the Grok flow already documented in [auth.md](./auth.md), so it works over SSH and on headless boxes (the browser can be on any machine; nothing depends on a localhost callback):

1. CLI: `POST /agent/v1/login/start` `{device_name}` → `{request_id, verify_url, expires_at}`. Unauthenticated, per-IP rate-limited, fails closed without touching other state.
2. CLI opens `verify_url` in the browser (and always prints it, for SSH). The page is the web UI's **authorize view** (session — Google OIDC): it shows the device name and a confirm button; on confirm it displays a one-time code `XXXX-XXXX` (8 chars base32, stored hashed, bound to the request).
3. User pastes the code into the CLI: `POST /agent/v1/login/complete` `{request_id, code}` → `{device_id, refresh_token, access_token, expires_in}`. Code: single-use, 10-minute TTL, dead after 5 wrong attempts.

**Tokens:**

- **Access token:** stateless, HMAC-signed with a dedicated `CLI_TOKEN_SECRET` (same signing pattern as the session cookie), claims `{user_id, device_id, exp}`, TTL **1 h**. Verified without a D1 read.
- **Refresh token:** random 32 bytes, stored hashed on the device row, **rotates on every use**: `POST /agent/v1/token` `{refresh_token}` → new refresh + new access. Presenting a superseded refresh token is treated as theft: the device is revoked (family revocation). Detection needs one generation of history, so the row keeps the previous token's hash too (`refresh_token_prev_hash`) — a presented token matching *that* column is the superseded one and revokes the device; a token matching neither column is a plain `401` (garbage proves nothing about this device). The rotation write is a conditional `UPDATE … WHERE refresh_token_hash = ?` so two concurrent presentations of the same token cannot both win. The CLI persists the new refresh token to its state file *before* discarding the old one, and serializes refreshes across its own processes with a lock file beside the state file.
- **Revocation:** the web UI's CLI page revokes a device (`revoked_at`). Effect: next refresh fails (≤ 1 h), next connect fails (immediately — connect does one D1 device check), and live sockets die at access-token expiry because the DO schedules an **alarm** at `exp` and closes with code `4003 token_expired` (alarms wake a hibernated DO; a `setTimeout` would keep it awake and billing). The CLI treats `4003` as "refresh, then reconnect" — invisible when the device is healthy, terminal when it is revoked.

## AgentTunnel Durable Object

- **Identity:** `idFromName(cliProviderRow.id)` — row ids are unique and immutable, and never resolve cross-user because every lookup upstream of the stub is already user-scoped.
- **One live socket.** A second successful connect replaces the first (old socket closed with code `4001 replaced`) — laptop-resume reconnects are self-healing instead of "address in use".
- **Hibernation:** sockets are accepted via `state.acceptWebSocket()` (Hibernation API); heartbeat is `setWebSocketAutoResponse("ping" → "pong")`, so an idle connected agent never wakes the DO and never bills duration. The only scheduled work is the token-expiry alarm above. In-flight request state lives in memory only: if the DO is evicted mid-request the socket drops, the CLI aborts its local requests, and the in-flight proxied requests fail with a fault marker (below).
- **Wake sources:** an incoming proxied request, a real (non-heartbeat) frame from the CLI, the expiry alarm, admin status fetches.

## Wire protocol (v1)

Established on `GET /agent/v1/connect/:providerId` (WebSocket upgrade, `Authorization: Bearer <access token>`). The **Worker** verifies the token signature and expiry, checks the device is not revoked, and checks the provider row belongs to the token's user, then forwards the upgrade into the DO stub with trusted internal headers (user id, provider id, slug, token `exp`) — the DO never sees or validates tokens itself. On accept the DO sends `{"t":"hello","proto":1,"slug":"<slug>"}`; a CLI that receives a `proto` it does not speak must disconnect and tell the user to upgrade.

Control frames are JSON text; body bytes are binary frames `[u32 request id][u8 kind][chunk]` (kind `0` = request body, `1` = response body) — binary framing avoids base64 inflation on SSE chunks.

| Direction | Frame | Meaning |
|---|---|---|
| DO → CLI | `{"t":"req","id",method,"path",headers}` | Open request `id`. `path` is a bare suffix (e.g. `/chat/completions`) — the CLI prepends its configured target base. |
| DO → CLI | binary kind 0 ×N, then `{"t":"req_end","id"}` | Request body (absent for GET). |
| CLI → DO | `{"t":"res","id",status,headers}` | Local server answered; headers reduced to `content-type` (same reduction discipline as the codex relay). |
| CLI → DO | binary kind 1 ×N, then `{"t":"res_end","id"}` | Response body, streamed chunk-by-chunk as received — the CLI never buffers a whole response, per the streaming guardrail. |
| CLI → DO | `{"t":"res_err","id","reason"}` | Local failure: `connect_refused`, `timeout`, `aborted`. |
| CLI → DO | `{"t":"models","models":[…]}` | Agent-reported catalog (below). Sent after hello and whenever the local list changes. |
| either | `{"t":"cancel","id"}` | Abort: DO sends it when the end client disconnects (fetch signal propagated through the stub); the CLI aborts its local fetch. CLI sends it if its local abort raced a partial response. |

**Bounds (all enforced in the DO, and the byte/path bounds again in the CLI — both ends distrust the other, same doctrine as the codex relay's allowlist):**

- Path allowlist by provider format — `openai`: `/chat/completions`, `/models`, `/audio/transcriptions`; `anthropic`: `/v1/messages`, `/v1/messages/count_tokens`, `/v1/models`. Anything else: protocol fault, request refused. The CLI additionally only ever joins these suffixes onto its one configured target base — it is structurally not a general proxy.
- Binary chunk ≤ 1 MiB per frame (platform limit is 32 MiB; small frames keep memory flat and interleave fairly across multiplexed requests).
- In-flight concurrency ≤ 4 per provider; excess is refused with fault `busy` so group failover can take the next target instead of queueing.
- Per-request DO-side response buffer ≤ 8 MiB: if the end client reads slower than the CLI sends and the gap exceeds the cap, the DO cancels the request (`too_large` fault). This is an honest bound in lieu of credit-based flow control — SSE chunks are small so real streams never hit it; a credit scheme is future work if large non-stream bodies ever matter.
- First `res` frame within **120 s** of `req_end`, else fault `timeout` — generous because a cold local 70B's first token is legitimately slow. No inter-chunk timeout beyond the platform's own.

## Model catalog — agent-reported, never pulled

There is no `models_mode` on a CLI provider. The CLI owns the truth:

- On connect (right after `hello`) the CLI fetches `GET <target>/models` locally and pushes the list as a `models` frame; while connected it re-checks every 5 minutes and pushes only on change. The DO persists the report to `cli_providers.models_json` + `models_updated_at` (D1 write from the DO) — pull a new model in Ollama and it appears on the proxy within one cycle, no re-registration.
- Report bounds mirror the custom manual-list rules: ≤ 100 entries, each trimmed to 1–128 chars, no whitespace (`/` allowed).
- `model_filter_json`, when set (chosen in `kano-proxy add`'s picker), is an expose-**whitelist applied at read time** over the reported list — the report itself is always stored whole, so clearing the filter never loses data. Default is no filter: expose everything, follow the local server.
- Catalog reads (`catalog/models.ts`) append one section per CLI provider straight from the stored report — no fetch, no KV cache, no fabrication. An offline agent keeps showing its last real report with its timestamp; an agent that has never connected shows an empty section.

## Failover semantics — the tri-state guard, again

Same shape as the codex relay's marker contract, for the same reason: an infrastructure fault must degrade the **route**, never poison **account** state.

| Marker on the DO's response | Meaning | Routing feedback |
|---|---|---|
| `x-agent-upstream: 1` | The local server answered; status passes through untouched. | Normal upstream semantics — yes, a local 429/401 benches per the standard penalty table; documented, not special-cased. |
| `x-agent-fault: <reason>`, status 502 | Tunnel-level failure: `offline`, `busy`, `timeout`, `replaced`, `too_large`, `protocol`. | `offline` benches the provider's internal account row **60 s** (so a group under traffic is not paying a DO round-trip per request while a laptop is closed); every other fault is a request-local skip to the next candidate, **no bench**. |
| neither | Never reached the DO app (platform error). | 502, no bench. |

**Reconnect clears the bench:** when the CLI reconnects, the DO nulls `bench_until`/`bench_reason` on the provider's internal account row (same write as manual unpause) — opening the laptop restores service on the next request, no operator action.

**Routing integration:** `routing/candidates.ts` gains a third prefix branch — builtin → custom slug → CLI slug — resolving to an adapter whose injected fetch goes to the DO stub instead of the network. Format semantics (`openai` near-passthrough / `anthropic` native passthrough, conversion paths, `reasoning_effort` handling) are inherited from the custom adapters unchanged; only the transport differs. `count_tokens` on an openai-format CLI provider answers the local stub sentinel, same as custom-openai without a `count_tokens_url`.

## CLI — `kano-proxy`

The product's official CLI — named after the product, subcommand space open for whatever comes later. Lives at `apps/cli` as a Cargo project. **Rust — operator decision 2026-08-30**, and the docs rationale the stack rule requires for a new language: the CLI ships to end users' machines, where a truly static, runtime-free single binary matters more than repo language uniformity — `deno compile` binaries are 80–100 MB self-extracting runtimes, a release-built Rust binary is a few MB, cross-compiles cleanly (`aarch64`/`x86_64-apple-darwin`, `x86_64`/`aarch64-unknown-linux-musl` fully static, `x86_64-pc-windows-msvc`), and the async-WS + streaming-HTTP shape is bog-standard in the ecosystem. Crates, pinned in `Cargo.lock`: `tokio`, `tokio-tungstenite` (rustls, no OpenSSL linkage), `reqwest` (streaming, rustls), `clap` (derive), `ratatui` + `crossterm` (interactive screens — chosen over a prompt library because the TUI surface will grow), `serde`/`serde_json`, `anyhow`. The Worker/DO side stays TypeScript; Rust is confined to `apps/cli`. `apps/cli/target/` goes into `.gitignore`.

### Command surface

Interactive **ratatui** screens are the default for `init` and `add`; every command also runs non-interactively with `--no-tui` + flags (docker/CI/scripts — same code path, different input source). Global flags: `--state <path>`, `--no-tui`, `--help`, `--version`.

**`kano-proxy init`** — sign this device in (once per machine).

- TUI: prompts base URL + device name → calls `login/start` → opens the browser (URL also printed, for SSH) → prompts for the code shown on the authorize page → completes login, persists device credentials.
- `--no-tui`, two phases sharing the pending request via the state file:
  ```console
  $ kano-proxy init --no-tui --base-url https://proxy.example.com --device-name box1
    open https://proxy.example.com/cli/authorize?request=…  then run:
    kano-proxy init --no-tui --auth-code XXXX-XXXX
  $ kano-proxy init --no-tui --auth-code XXXX-XXXX
    ✓ device "box1" signed in
  ```
- Re-running on an already-initialized state file refuses with a hint (revoke/re-init deliberately, not by accident).

**`kano-proxy add`** — register one local endpoint as a CLI provider.

- TUI screen: slug (default: sanitized hostname, uniquified), API type `openai`/`anthropic`, target base URL (e.g. `http://localhost:11434/v1` — include the `/v1`; the CLI appends only allowlisted suffixes and never guesses a prefix), optional local API key. It then probes `GET <target>/models`: on success, a searchable model list opens whose **first row is "All models (follow local server)"** — the default, which stores **no filter**, so models added locally later appear automatically; selecting a subset stores an expose-whitelist instead. On probe failure it asks for model ids by hand (stored as the initial report; the agent's first connect overwrites it with truth) — a target that is not running yet must not block registration.
- `--no-tui`: `--slug <s> --format openai|anthropic --target <url> [--target-key <k>] [--expose m1,m2,…]` (`--expose` omitted = no filter; models probed on first `start`).
- Server call: `POST /agent/v1/providers` (Bearer access token) → row created, appended to local state. Multiple providers per device is the normal case.

**`kano-proxy remove <slug>`** — `DELETE /agent/v1/providers/:id` (closes any live socket) + removes it from local state. `--local-only` skips the server call (e.g. already deleted in the web UI).

**`kano-proxy list`** — table of this device's registered providers: slug, format, target, connection state, model count, last report time (state read-through to the server, so it reflects the DO's truth).

**`kano-proxy start`** — the long-running tunnel process: refreshes the access token, opens **one WS per registered provider**, speaks protocol v1, forwards to each provider's target. `--concurrency <1–4>` lowers the in-flight cap below the server's 4 (a raise is refused). Foreground, logs to stderr; daemonize with your own launchd/systemd unit if you want a service (explicit over clever — the CLI does not self-daemonize).

**`kano-proxy status`** — device auth state (signed in as, token freshness) plus per-provider connection state from `GET /agent/v1/providers` — works from any process, and is the truth even while `start` runs elsewhere.

**`kano-proxy update`** — self-update: queries the latest GitHub Release, downloads this platform's asset, verifies it against `SHA256SUMS`, atomically replaces its own binary. When the running binary lives inside a package manager's tree (Homebrew cellar, Scoop apps dir) it refuses and prints that manager's upgrade command instead — two owners for one binary is how installs rot.

### State file

`~/.config/kano-proxy/state.json` on macOS/Linux (`%APPDATA%\kano-proxy\state.json` on Windows), mode `0600`, plus a sibling lock file for refresh serialization. Contents: `{base_url, device_id, device_name, refresh_token, providers: [{id, slug, format, target, target_key, expose}]}`. **The state file must be writable at runtime** — refresh rotation persists a new token on every refresh, which is also why there is no "token via environment variable" mode: a static injected token would be superseded by its own first use. Docker/headless: mount the state directory as a writable volume and run `init`/`add` once against it.

### First-run walkthrough

```console
# 1. Install (any channel from § Distribution):  brew install yufeng-kano/tap/kano-proxy
$ kano-proxy init
  Server: https://proxy.example.com
  Device name [my-mac]: ⏎
  → opening https://proxy.example.com/cli/authorize?request=…  (sign in with Google, press Approve)
  Code shown in browser: XXXX-XXXX
  ✓ device "my-mac" signed in
$ kano-proxy add
  Slug [my-mac]: ⏎   Type: openai   Target [http://localhost:11434/v1]: ⏎
  ✓ found 3 models — [All models (follow local server)] ⏎
  ✓ registered "my-mac"
$ kano-proxy start
  my-mac: connected (proto 1), 3 models reported
# 2. Use it like any provider, from anywhere:
#    POST https://proxy.example.com/openai/v1/chat/completions  model: "my-mac/llama3.3:70b"
#    …or add "my-mac/…" as a target in a model group for failover.
```

### Runtime behavior

- Streams both directions chunk-for-chunk (no whole-body buffering, per the streaming guardrail), honors `cancel` by aborting the local request, refuses any `path` outside the allowlist, and joins paths onto exactly the one configured target base per provider — structurally not a general proxy.
- Reconnects forever on **network** failures: exponential backoff 1 s → 60 s with full jitter, per provider socket; heartbeat text `ping` every 30 s (answered by the DO auto-response without waking it). Close `4003 token_expired` → refresh and reconnect silently. HTTP 401 / failed refresh (device revoked or unregistered) does **not** retry: exit with a message to re-run `init`. Close `4001 replaced` (another `start` took the provider over) logs and drops that provider rather than fighting its sibling.
- `SIGINT`/`SIGTERM`: close sockets with code 1000, abort in-flight local requests, exit 0.
- Exit codes: `0` clean shutdown, `1` usage/config error, `2` auth rejected. Logs are one line per lifecycle event and per request (`id`, method, path, status, duration) — never bodies, never headers, never tokens ([logging.md](./logging.md) discipline applies to the CLI too).

## Server routes

| Route | Auth | Purpose |
|---|---|---|
| `POST /agent/v1/login/start` | none (per-IP rate-limited) | Create login request → `{request_id, verify_url, expires_at}` |
| `POST /agent/v1/login/complete` | pairing code | Redeem approved request → device + first token pair |
| `POST /agent/v1/token` | refresh token | Rotate → new refresh + access; superseded-token reuse revokes the device |
| `GET /agent/v1/connect/:providerId` | Bearer access | WebSocket upgrade → AgentTunnel DO |
| `GET /agent/v1/providers` | Bearer access | List the user's CLI providers + live connection state (DO read-through) |
| `POST /agent/v1/providers` | Bearer access | Create (slug, format, optional expose filter / initial models) |
| `DELETE /agent/v1/providers/:id` | Bearer access | Delete + force-close socket |

`/agent/v1/*` is its own namespace beside `/api/*` (session) and the LLM surfaces: token-authenticated, permissive CORS unnecessary (no browser callers), never session-authenticated. The session-side management routes the web UI uses (`/api/cli/*` — devices, revoke, providers, rename, delete, login-request read/approve/deny) are listed in [auth.md](./auth.md) § CLI devices and providers.

## Web UI — the CLI page

New sidebar page **`/cli`** (nav item "CLI"), plus the session-gated authorize view the login flow lands on:

| Route | Content |
|---|---|
| `/cli` | Two datasets, two cards per the house style: **Devices** (name, last seen, created — row action: revoke) and **CLI providers** (slug, format, connection state chip, model count + last report time, registered-from device — row actions: rename display name, delete). No create flows here — creation is the CLI's job; the page's empty state shows the `kano-proxy init` one-liner and a Releases link. |
| `/cli/authorize?request=…` | Approve view for a pending login: device name + Approve/Deny; on approve, displays the one-time code. Session required (redirects through login like any admin page). |

Providers page (`/providers`) does **not** list CLI providers — they live on `/cli`; the Models page and group-target pickers include them like any other provider (ids are `<slug>/<model>`). Detailed layout rules land in [admin-ui.md](./admin-ui.md) at implementation time.

## Distribution

The CLI is versioned and released **with the product**: one SemVer, one tag, one GitHub Release ([deployment.md](./deployment.md) — production deploys already happen only on Release publish). Wire compatibility is governed by the protocol's `proto` number, never by comparing version strings. The repository is public, so Release assets are directly downloadable by end users of any kano-proxy instance — they need no relationship with the repo.

**Release CI** gains a job that, on Release publish, cross-compiles the five targets, packages `kano-proxy-<version>-<target>.tar.gz` (`.zip` for Windows), emits a `SHA256SUMS` file covering all archives, and attaches everything to the Release.

Install channels, all fed from those assets:

| Channel | Platforms | Mechanics |
|---|---|---|
| Homebrew tap | macOS, Linux | `brew install yufeng-kano/tap/kano-proxy` — formula in our own public `homebrew-tap` repo, version + checksums bumped automatically by release CI |
| Install script | macOS, Linux | `curl -fsSL https://raw.githubusercontent.com/yufeng-kano/kano-proxy/main/scripts/install-cli.sh \| sh` — detects OS/arch, downloads the latest asset, verifies `SHA256SUMS`, installs to `~/.local/bin` (or `--dir`) |
| Scoop bucket | Windows | `scoop bucket add kano https://github.com/yufeng-kano/scoop-bucket` + `scoop install kano-proxy` — manifest in our own public bucket repo, bumped by release CI |
| `kano-proxy update` | all | self-update from the latest Release, checksum-verified (see the command above) |

**None of these channels involves registration or third-party review** — a Homebrew *tap* and a Scoop *bucket* are just public repos we own, live the moment they exist; review queues only guard homebrew-core and Scoop's official buckets, neither of which we use. winget (Microsoft's review queue) is deliberately not offered — Scoop covers Windows. No crates.io publish: this is a product binary, not a library, and the monorepo layout gives a Cargo publish nothing but friction.

The web UI's `/cli` empty state shows the brew and script one-liners (from the message catalog, per [i18n.md](./i18n.md)) and links the Releases page — every instance distributes the same official binaries; instances never host binaries themselves.

## Cost and limits

- **Idle is ~free:** a connected-but-quiet agent is a hibernated DO; heartbeats are auto-responses (no wake, no duration, no request billing); the hourly expiry alarm is one brief wake per provider per hour.
- **Active streams bill DO duration** (~128 MB × wall seconds against the 400k GB-s included allocation) plus per-message delivery. At personal-local-LLM traffic volumes this is noise; it is the cost of owning the tunnel instead of renting trycloudflare.
- One socket per DO, one DO per provider, ≤ 20 providers per user (shared cap with custom), ≤ 20 devices per user — no fan-in bottleneck to reason about.
- Request bodies: bounded by the same request-size realities as the rest of the proxy; a body the local server will not accept fails at the local server, passed through as its own status.

## Security notes

- No permanent secrets: access tokens live 1 h, refresh tokens rotate on every use with reuse-as-theft revocation, devices are individually revocable from `/cli`, and revocation reaches live sockets within the access TTL via the expiry alarm.
- Login codes and refresh tokens are stored hashed; login endpoints are per-IP rate-limited (KV counter, 10 starts / 10 min / IP) and fail closed without touching unrelated state. Expired `cli_login_requests` rows are purged by the daily retention sweep ([logging.md](./logging.md)).
- The tunnel carries prompts/completions as opaque bytes; nothing content-shaped is logged at the Worker, DO, or CLI ([logging.md](./logging.md) rules apply unchanged).
- Cross-user isolation is inherited: every path to the stub starts from a user-scoped row lookup, and the DO name is the provider row id.
- The CLI's target confinement (one base URL per provider + path allowlist, enforced at both ends) means a compromised proxy operator still cannot use connected agents as general SSRF proxies into home networks.

## Implementation order

1. Migration `0015` (`cli_devices`, `cli_login_requests`, `cli_providers`) + [database.md](./database.md) update.
2. Device auth: `login/start`, `login/complete`, `token` routes + `CLI_TOKEN_SECRET` + web authorize view + [auth.md](./auth.md) route-table update.
3. `AgentTunnel` DO (+ `wrangler.toml` binding and `new_sqlite_classes` migration; mirrored in gitignored `wrangler.production.toml`), protocol module shared as types, `connect`/`providers` routes.
4. Routing third branch in `candidates.ts`, injected-fetch reuse of the custom adapters, fault-marker handling in `feedback.ts`, catalog section — + [providers.md](./providers.md) pointer.
5. `/cli` page + authorize view + [admin-ui.md](./admin-ui.md) update.
6. `apps/cli` (Rust): `cargo test` wired into CI; distribution pipeline per § Distribution — release workflow (five targets + `SHA256SUMS` on the Release), `scripts/install-cli.sh`, Homebrew tap + Scoop bucket automation, `kano-proxy update`.

Tests are protocol-level with in-memory socket pairs and stubbed `fetch` throughout ([testing.md](./testing.md)) — a local LLM is free, but the test suite still never assumes one is running.
