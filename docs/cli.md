# CLI providers (kano-proxy CLI + reverse tunnel for local LLMs)

**Status: implemented (v4.4.0; CLI 2.0 + `/cli/v1` + agent setup in v4.13.0 / cli-v2.0.0).** This document remains the contract for the subsystem.

**Scope discipline: no cut-down MVP.** Operator decision 2026-08-30: this ships as specified — device auth with rotating refresh tokens, the full TUI, multi-provider `start`, the `/cli` page, the distribution pipeline — not a reduced first pass. This document is the contract; the only sanctioned deferrals are the ones marked *future work* inline (currently: credit-based flow control). If implementation reality forces a change, this doc changes first (docs-first rule in `CLAUDE.md`), never the scope silently.

Lets a user expose an LLM server running on their own machine (Ollama, LM Studio, vLLM, llama.cpp — anything OpenAI- or Anthropic-compatible) as a kano-proxy provider **without a public address**: the official `kano-proxy` CLI dials **out** to the server over WebSocket, and the server routes provider traffic back down that connection. This is a product feature for all users, so it assumes nothing installed on the user's machine beyond the CLI itself — no cloudflared, no ngrok, no port forwarding.

Terminology: this subsystem is the **CLI provider** feature and its transport is the **CLI tunnel** (wire namespace `/cli/v1/*`, the CLI's own — § Server routes). *Agent* in this codebase means a coding agent (Claude Code, the Codex CLI) that `kano-proxy agent setup` configures, never the tunnel client; the wire namespace used to be `/agent/v1` and was renamed with CLI 2.0.

```text
kano-proxy CLI ──wss (outbound dial)──▶ server /cli/v1/connect ──▶ tunnel registry
                                                                        ▲ holds the socket (hibernatable)
request path:
client ─▶ server (auth, routing, conversion) ─▶ tunnel registry ─▶ WS frames ─▶ CLI ─▶ http://localhost:11434/v1
                                              ◀────── response streamed frame-by-frame ──────◀
```

Everything smart stays in the server: client auth, pool, failover, format conversion. The registry is a byte pipe with a request multiplexer; the CLI is a byte pipe with a dialer.

## Why a built-in tunnel and not a public-URL shim

A thin alternative exists — CLI auto-downloads `cloudflared`, opens a quick tunnel, registers the `trycloudflare.com` URL as an ordinary custom provider. Rejected for the public product: the URL is world-reachable (attack surface guarded only by a shim token), rotates on every restart, rides a no-SLA free service, and makes us ship and verify a third-party binary. The built-in tunnel costs a real subsystem but gives: zero public URL, auth inside our own token system, one dependency-free binary, and the registry lives in the same process as everything else that needs it (— a persistent per-provider socket **is** coordination state).

## Concepts — a first-class provider kind, not a custom-provider variant

**Operator decision 2026-08-30: CLI providers are their own kind**, not `custom_providers` rows with a transport flag. Folding them into custom was rejected because everything about the fit was a contortion — a sentinel `base_url` that is never fetched, a rejected `count_tokens_url`, a placeholder API key, and above all the `models_mode` auto/manual dichotomy, which exists only because a custom provider's catalog must be *pulled* from a URL that may be unreachable. A CLI provider's catalog is *pushed* by the agent (below), so that dichotomy simply does not apply to it.

Three entities:

- **CLI device** — one machine that ran `kano-proxy login`: a name, a rotating refresh token, a revocation switch. Shown on the web UI's Access page. A device belongs to one user, can serve any of that user's CLI providers, and can mint API keys that die with it.
- **CLI provider** — one local endpoint registered by `kano-proxy tunnel add`: slug, wire format, and the tunnel-reported model list. On the routing surface it behaves like any provider: model ids are `<slug>/<model>`, it can be a group target, it participates in bench/failover.
- **Agent setup** (CLI 2.0) — a coding agent on the device pointed at the proxy by `kano-proxy agent setup`: an API key the device minted plus a model group named after the agent, and a restore record on the device (§ CLI, § Keys and groups). Not a table of its own: the key is an `api_keys` row carrying the device id, the group an ordinary model group.

## Data model

Three new tables (`0015_cli_providers.sql`; full column notes in [database.md](./database.md); `custom_providers` is untouched):

| Table | Columns (essentials) |
|---|---|
| `cli_devices` | `id`, `user_id`, `name` (1–64 chars), `refresh_token_hash`, `refresh_token_prev_hash` (superseded-token theft detection — see Device auth below), `last_seen_at`, `created_at` — revoking deletes the row, so every stored device is live |
| `cli_login_requests` | `id`, `device_name`, `code_hash`, `user_id` (NULL until approved), `expires_at`, `approved_at`, `used_at`, `attempts`, `created_at` |
| `cli_providers` | `id`, `user_id`, `device_id` (nullable — informational "registered from"), `slug`, `name`, `format` (`openai` \| `anthropic`), `models_json` (last agent report), `models_updated_at`, `model_filter_json` (nullable expose-whitelist), `sort_order`, `created_at`, `updated_at` |

- **Slug rules:** identical to custom providers (lowercase 2–32, `^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$`, same reserved list), **immutable**, and unique per user **across both** `custom_providers` and `cli_providers` — both create paths check the other table, because both kinds resolve from the same `<slug>/<model>` position.
- **Format immutable** after creation, same as custom (delete-and-recreate).
- **Pool internals:** each CLI provider still gets one `upstream_accounts` row (`provider = slug`, empty credential). This is an implementation detail, not a user-facing "account": it is where the existing bench/unpause/routing-facts machinery keeps its state, so `facts.ts` / `feedback.ts` / group targets need no parallel bookkeeping. The row is created and deleted with the provider and never shown as an account on the CLI page; its one UI appearance is as the Groups picker's pin handle, exactly like a custom endpoint's key row ([admin-ui.md](./admin-ui.md) § Groups page).
- **Caps:** CLI providers count into the same 20-per-user provider budget as custom providers (shared cap across both tables, and the cross-table slug guard rides inside each INSERT so racing creates cannot both land); ≤ 20 devices per user — a revoked device is deleted, so it neither stays listed nor spends the quota.
- **The target URL is not stored server-side.** Where the local server lives (`http://localhost:11434/v1`) and its optional local API key are CLI state on the device — the server has no business knowing them and could not use them anyway.

## Device auth — login once, rotate forever

No permanent secrets on disk. The CLI holds a **rotating refresh token**; everything it does against the server uses a short-lived **access token** minted from it.

**Login flow (`kano-proxy login`)** — authorize-then-paste, the same shape as the Grok flow already documented in [auth.md](./auth.md), so it works over SSH and on headless boxes (the browser can be on any machine; nothing depends on a localhost callback):

1. CLI: `POST /cli/v1/login/start` `{device_name}` → `{request_id, verify_url, expires_at}`. Unauthenticated, per-IP rate-limited, fails closed without touching other state.
2. CLI opens `verify_url` in the browser (and always prints it, for SSH). The page is the web UI's **authorize view** (session — Google OIDC): it shows the device name and a confirm button; on confirm it displays a one-time code `XXXX-XXXX` (8 chars base32, stored hashed, bound to the request).
3. User pastes the code into the CLI: `POST /cli/v1/login/complete` `{request_id, code}` → `{device_id, refresh_token, access_token, expires_in}`. Code: single-use, 10-minute TTL, dead after 5 wrong attempts.

**Tokens:**

- **Access token:** stateless, HMAC-signed with a dedicated `CLI_TOKEN_SECRET` (same signing pattern as the session cookie), claims `{user_id, device_id, exp}`, TTL **1 h**. Verified without a database read.
- **Refresh token:** random 32 bytes, stored hashed on the device row, **rotates on every use**: `POST /cli/v1/token` `{refresh_token}` → new refresh + new access. Presenting a superseded refresh token is treated as theft: the device is revoked, i.e. deleted (family revocation). Detection needs one generation of history, so the row keeps the previous token's hash too (`refresh_token_prev_hash`) — a presented token matching *that* column is the superseded one and revokes the device; a token matching neither column is a plain `401` (garbage proves nothing about this device). The rotation write is a conditional `UPDATE … WHERE refresh_token_hash = ?` so two concurrent presentations of the same token cannot both win. The CLI persists the new refresh token to its state file *before* discarding the old one, and serializes refreshes across its own processes with a lock file beside the state file.
- **Revocation:** the web UI's Access page revokes a device by deleting its row, and `kano-proxy logout` does the same through `DELETE /cli/v1/device` — nothing of a revoked device is kept; the API keys it minted are deleted by the foreign-key cascade (§ Keys and groups), and its providers stay (showing no device) until removed. Effect: next refresh fails (≤ 1 h), next connect fails (immediately — connect does one device-row check), and live sockets die at access-token expiry because the registry schedules a close at `exp` with code `4003 token_expired`. The CLI treats `4003` as "refresh, then reconnect" — invisible when the device is healthy, terminal when it is revoked.

## The tunnel registry

- **Identity:** the registry is keyed by `cli_providers.id` — row ids are unique and immutable, and never resolve cross-user because every lookup ahead of it is already user-scoped.
- **One live socket.** A second successful connect replaces the first (old socket closed with code `4001 replaced`) — laptop-resume reconnects are self-healing instead of "address in use".
- **Idle sockets:** a connected-but-quiet agent costs one open socket and a heartbeat; the server answers `ping` with `pong` without waking any request path, so an idle connected agent costs nothing but the socket. The only scheduled work is the token-expiry alarm above. In-flight request state lives in memory only: if the server restarts mid-request the socket drops, the CLI aborts its local requests, and the in-flight proxied requests fail with a fault marker (below).

## Wire protocol (v1)

Established on `GET /cli/v1/connect/:providerId` (WebSocket upgrade, `Authorization: Bearer <access token>`). The **server** verifies the token signature and expiry, checks the device row still exists (revoking deletes it), and checks the provider row belongs to the token's user, then hands the upgrade to the registry with the verified facts (user id, provider id, slug, token `exp`) — the registry never validates tokens itself. On accept it sends `{"t":"hello","proto":1,"slug":"<slug>"}`; a CLI that receives a `proto` it does not speak must disconnect and tell the user to upgrade.

Control frames are JSON text; body bytes are binary frames `[u32 request id][u8 kind][chunk]` (kind `0` = request body, `1` = response body) — binary framing avoids base64 inflation on SSE chunks.

| Direction | Frame | Meaning |
|---|---|---|
| server → CLI | `{"t":"req","id",method,"path",headers}` | Open request `id`. `path` is a bare suffix (e.g. `/chat/completions`) — the CLI prepends its configured target base. |
| server → CLI | binary kind 0 ×N, then `{"t":"req_end","id"}` | Request body (absent for GET). |
| CLI → server | `{"t":"res","id",status,headers}` | Local server answered; headers reduced to `content-type` (same reduction discipline as the codex relay). |
| CLI → server | binary kind 1 ×N, then `{"t":"res_end","id"}` | Response body, streamed chunk-by-chunk as received — the CLI never buffers a whole response, per the streaming guardrail. |
| CLI → server | `{"t":"res_err","id","reason"}` | Local failure: `connect_refused`, `timeout`, `aborted`. |
| CLI → server | `{"t":"models","models":[…]}` | Agent-reported catalog (below). Sent after hello and whenever the local list changes. |
| either | `{"t":"cancel","id"}` | Abort: the server sends it when the end client disconnects (the cancellation propagates through the registry); the CLI aborts its local fetch. CLI sends it if its local abort raced a partial response. |

**Bounds (all enforced by the server, and the byte/path bounds again in the CLI — both ends distrust the other):**

- Path allowlist by provider format — `openai`: `/chat/completions`, `/models`, `/audio/transcriptions`; `anthropic`: `/v1/messages`, `/v1/messages/count_tokens`, `/v1/models`. Anything else: protocol fault, request refused. The CLI additionally only ever joins these suffixes onto its one configured target base — it is structurally not a general proxy.
- Binary chunk ≤ 1 MiB per frame, enforced on **receive** too — an inbound frame past the bound is a protocol fault on that request, refused before anything is enqueued (the platform frame limit is 32 MiB; small frames keep memory flat and interleave fairly across multiplexed requests).
- Request body ≤ 512 MiB per request (`too_large` fault): `ws.send` exposes no drain signal, so a body arriving faster than the socket empties would queue unboundedly in memory — the hard cap is the honest request-side bound, the mirror of the response-side buffer below, until credit-based flow control (future work) covers both directions.
- In-flight concurrency ≤ 4 per provider; excess is refused with fault `busy` so group failover can take the next target instead of queueing.
- Per-request server-side response buffer ≤ 8 MiB: if the end client reads slower than the CLI sends and the gap exceeds the cap, the server cancels the request (`too_large` fault). This is an honest bound in lieu of credit-based flow control — SSE chunks are small so real streams never hit it; a credit scheme is future work if large non-stream bodies ever matter.
- First `res` frame within **120 s** of `req_end`, else fault `timeout` — generous because a cold local 70B's first token is legitimately slow. No inter-chunk timeout beyond the platform's own.

## Model catalog — agent-reported, never pulled

There is no `models_mode` on a CLI provider. The CLI owns the truth:

- On connect (right after `hello`) the CLI fetches `GET <target>/models` locally and pushes the list as a `models` frame; while connected it re-checks every 5 minutes and pushes only on change. The server persists the report to `cli_providers.models_json` + `models_updated_at` — pull a new model in Ollama and it appears on the proxy within one cycle, no re-registration. A failed persistence closes the socket retryably (the CLI already marked the list as sent): the reconnect re-reports from a clean slate, so a transient database error cannot leave the catalog stale until the next local change.
- Report bounds mirror the custom manual-list rules: ≤ 100 entries, each trimmed to 1–128 chars, no whitespace (`/` allowed).
- `model_filter_json`, when set (chosen in `kano-proxy tunnel add`'s picker), is an expose-**whitelist applied at read time** over the reported list — the report itself is always stored whole, so clearing the filter never loses data. Default is no filter: expose everything, follow the local server.
- Catalog reads (`catalog/models.ts`) append one section per CLI provider straight from the stored report — no fetch, no cache, no fabrication. An offline agent keeps showing its last real report with its timestamp; an agent that has never connected shows an empty section.
- CLI slugs count as **live providers** everywhere custom slugs do: the usage summary's live-provider scoping, the Logs filter, and the Groups picker tabs.

## Failover semantics — the tri-state guard, again

Same shape as the codex relay's marker contract, for the same reason: an infrastructure fault must degrade the **route**, never poison **account** state.

| Marker on the registry's response | Meaning | Routing feedback |
|---|---|---|
| `x-tunnel-upstream: 1` | The local server answered; status passes through untouched. | Normal upstream semantics — yes, a local 429/401 benches per the standard penalty table; documented, not special-cased. |
| `x-tunnel-fault: <reason>`, status 502 | Tunnel-level failure: `offline`, `busy`, `timeout`, `replaced`, `too_large`, `protocol`. | `offline` benches the provider's internal account row **60 s** (so a group under traffic is not paying a registry round-trip per request while a laptop is closed); every other fault is a request-local skip to the next candidate, **no bench**. |
| neither | Never reached the tunnel. | 502, no bench. |

**Reconnect clears the bench:** when the CLI reconnects, the server nulls `bench_until`/`bench_reason` on the provider's internal account row (same write as manual unpause) — opening the laptop restores service on the next request, no operator action. One accepted race: a request that faulted `offline` in the instant around the reconnect can write its 60 s bench *after* the clear; the exposure is bounded by that single cooldown (the bench self-expires, and the next reconnect clears again), which is not worth a connection-generation protocol on the fault path.

**Routing integration:** `routing/candidates` gains a third prefix branch — builtin → custom slug → CLI slug — resolving to an adapter whose injected transport goes to the registry instead of the network. Format semantics (`openai` near-passthrough / `anthropic` native passthrough, conversion paths, `reasoning_effort` handling) are inherited from the custom adapters unchanged; only the transport differs. `count_tokens` on an openai-format CLI provider answers the local stub sentinel, same as custom-openai without a `count_tokens_url`.

## CLI — `kano-proxy`

The product's official CLI. Lives at `apps/cli` as a Cargo project. **Rust — operator decision 2026-08-30**, and the docs rationale the stack rule requires for a new language: the CLI ships to end users' machines, where a truly static, runtime-free single binary matters more than repo language uniformity — `deno compile` binaries are 80–100 MB self-extracting runtimes, a release-built Rust binary is a few MB, cross-compiles cleanly (`aarch64`/`x86_64-apple-darwin`, `x86_64`/`aarch64-unknown-linux-musl` fully static, `x86_64-pc-windows-msvc`), and the async-WS + streaming-HTTP shape is bog-standard in the ecosystem. Crates, pinned in `Cargo.lock`: `tokio`, `tokio-tungstenite` (rustls, no OpenSSL linkage), `reqwest` (streaming, rustls), `clap` (derive), `ratatui` + `crossterm` (interactive screens — chosen over a prompt library because the TUI surface will grow), `serde`/`serde_json`, `anyhow`, `futures-util` (stream plumbing the WS stack already pulls), `toml_edit` (for `kano-proxy agent`: edits the Codex CLI's `config.toml` while keeping every comment and key the user already has — a serialize-from-struct rewrite would flatten the file), and for `kano-proxy update` only: `sha2` (checksum verify), `flate2` + `tar`, `zip` (release-archive extraction). The server side is Rust too, so the protocol has one definition per side and no second implementation. `apps/cli/target/` goes into `.gitignore`.

**Two jobs, two namespaces (CLI 2.0, operator decision 2026-10-11).** The CLI started as the tunnel client alone, with its commands flat at the top level. 2.0 adds a second job — writing the Kano Proxy connection into coding agents on the same machine — and both jobs will keep growing (the docs site already documents six agents). Flat verbs for two unrelated features read as one muddle, so each job owns a subcommand namespace: **`tunnel`** for the local-LLM tunnel and **`agent`** for coding-agent setup. The device-level commands stay top-level. The 1.x flat tunnel commands (`kano-proxy add`, `start`, …) are **gone, not aliased**: nobody was on them yet, and a stub that only prints the new name is a second spelling to document forever. The wire namespace moved with it (§ Server routes), so a 1.x binary is refused by a 2.0 server with one clear message rather than half-working.

```text
kano-proxy login                      sign this device in (again, when the server revoked the device)
kano-proxy logout                     restore every agent this device set up, revoke the device
kano-proxy status                     device auth + tunnel connection state + agent state
kano-proxy update                     self-update
kano-proxy tunnel add|attach|remove|list|start
kano-proxy agent setup|restore|list <claude-code|codex>
```

### Command surface

Interactive **ratatui** screens are the default for `login`, `tunnel add` and `agent setup`; every command also runs non-interactively with `--no-tui` + flags (docker/CI/scripts — same code path, different input source). Global flags: `--state <path>`, `--no-tui`, `--help`, `--version`.

**Navigation (CLI 2.4, operator decision 2026-10-12): editing flows work like an editor, not a wizard.** `agent setup` and `tunnel add` open on a **root page** listing every item with its value — for Claude Code each slot's targets, for Codex the model, effort and targets, for `tunnel add` slug, API, target, local key and models. Enter opens an item's page; there Space toggles and Enter **applies the change to the draft** and returns to the root page, Esc returns without it. **Nothing reaches the server or a config file until Tab on the root page**, which saves the whole draft at once; Tab does nothing anywhere else. Rows the draft has changed are marked `* … (changed)`, and the line under the list says whether the draft differs from what is set up now (`No changes`, `N unsaved changes — Tab saves`, or `Not set up on this machine yet`). **Esc twice on the root page exits**: when saving would change something — the draft differs from the config and group in place, or nothing is set up yet — it first asks, the way nano asks before closing a modified buffer, `Save changes? Y Yes · N No · Esc Cancel`, listing the changes; Y saves, N exits without saving, Esc goes back to editing. With nothing to save it exits at once. After the first Esc every footer says "Press Esc again to exit". `login` stays a three-step wizard (Esc steps back, Esc twice exits); `agent restore` ends on one screen with its whole plan, Enter restores, Esc cancels. Quitting is not an error: the CLI prints `Cancelled — nothing was changed.` (or, for a `logout` stopped part-way, which agents were already restored) and exits `130`; Ctrl-C quits at once from anywhere.

**`kano-proxy login`** — sign this device in (once per machine).

- TUI: prompts base URL + device name → calls `login/start` → opens the browser → prompts for the code shown on the authorize page → completes login, persists device credentials. The base URL must be `https://` except for loopback hosts — cleartext to a routable origin would hand the whole rotating token family to an on-path observer.
- **Default server (operator decision 2026-09-22): the hosted edition, `https://kano-proxy.yuufeng.com`**, compiled into the CLI as `DEFAULT_BASE_URL`. The Server prompt is pre-filled with it (or with the state file's `base_url` when one exists, or `--base-url`), so a hosted-edition user presses Enter; a self-hoster types their own origin, exactly as `gh` defaults to github.com. The web UI's install card shows `kano-proxy login --base-url <this instance's origin>` so every instance teaches its own address (§ Web UI).
- **The authorize URL is rendered inside the code screen itself**, not only printed before it. Every ratatui screen runs on the terminal's alternate screen, so anything printed to stdout/stderr before the code prompt is hidden while the prompt is up — which is exactly when the user needs the URL. The screen shows the URL above the input; the CLI also prints it before opening the browser (for logs and `--no-tui`), and claims to have opened a browser only when it plausibly could (a spawned `xdg-open` with no display is not a browser).
- `--no-tui`, two phases sharing the pending request via the state file:
  ```console
  $ kano-proxy login --no-tui --base-url https://proxy.example.com --device-name box1
    open https://proxy.example.com/cli/authorize?request=…  then run:
    kano-proxy login --no-tui --auth-code XXXX-XXXX
  $ kano-proxy login --no-tui --auth-code XXXX-XXXX
    ✓ device "box1" signed in
  ```
- **Re-running while signed in** first asks the server (one token rotation). A device the server still accepts is refused with a hint to run `kano-proxy logout` first — sign out deliberately, not by accident. A device the server rejects (revoked on the Access page, or by refresh-token reuse detection) is the case the command exists for: it says so, clears the local credentials, tunnel list and agent records (their keys died with the device), and signs in afresh. Every other command that meets that rejection says the same thing — `this device was revoked or signed out on the server — run kano-proxy login again` — instead of a bare 401, and `tunnel start` exits with it rather than retrying (§ Runtime behavior). The command was `init` until CLI 2.1; `login` is what it does, and `logout` already existed.

**`kano-proxy logout`** — the reverse of `login`. In order: restores every agent this device set up (`agent restore`'s default field-level restore, one agent after another — a revoked device's keys are dead, so a config left pointing at one would be a half-broken state the CLI refuses to leave behind), then `DELETE /cli/v1/device` (the device revokes itself — the whole refresh-token family dies, and the keys it created go with it, § Keys and groups), then clears the state file's credentials, pending login and tunnel-provider list. `base_url` and the backups directory stay. An agent whose config drifted from what the CLI wrote stops the TUI with the same confirmation `agent restore` uses; `--no-tui` needs `--force` to proceed past drift. `--remove-group` is passed through to the restores. A server that already refused the device (revoked from the web UI) does not block the local cleanup: the restores and the state wipe still run, and the command says the server side was already gone.

**`kano-proxy tunnel add`** — register one local endpoint as a CLI provider.

- TUI screen: slug (default: sanitized hostname, uniquified), API type `openai`/`anthropic` — each row says what it is and which local servers speak it, above a one-line "which HTTP API does the local server speak?" — then the target base URL, **on a screen written for the API just chosen**: its shape rule, its default, and copy-pasteable examples for the usual local servers (`openai`: the `/v1` base, e.g. Ollama `http://localhost:11434/v1`, LM Studio `http://localhost:1234/v1`, vLLM `http://localhost:8000/v1`, llama.cpp `http://localhost:8080/v1`, the CLI appends `/chat/completions`; `anthropic`: the origin only, e.g. Ollama `http://localhost:11434`, llama.cpp `http://localhost:8080`, the CLI appends `/v1/messages` — it appends only allowlisted suffixes and never guesses a prefix). The user is never asked to work out whether `/v1` belongs on the end. Then the optional local API key. It then probes the model list (`openai`: `GET <target>/models`; `anthropic`: `GET <target>/v1/models`, per `api.rs`): on success, a searchable model list opens whose **first row is "All models (follow local server)"** — the default, which stores **no filter**, so models added locally later appear automatically; selecting a subset stores an expose-whitelist instead. On probe failure it asks for model ids by hand (stored as the initial report; the tunnel's first connect overwrites it with truth) — a target that is not running yet must not block registration.
- `--no-tui`: `--slug <s> --format openai|anthropic --target <url> [--target-key <k>] [--expose m1,m2,…]` (`--expose` omitted = no filter; models probed on first `start`). Prefer passing the local key as the `KANO_PROXY_TARGET_KEY` environment variable — argv leaks into shell history and the process list; the flag stays for controlled environments. The interactive screen collects it masked.
- Server call: `POST /cli/v1/providers` (Bearer access token) → row created, appended to local state. Multiple providers per device is the normal case.

**`kano-proxy tunnel attach <slug> --target <url> [--target-key <k>]`** — serve an **existing** provider from this device: records where the local server lives here for a row another machine registered (the server already authorizes any of the user's devices; the last `start` to connect wins the socket). Flags-only on purpose — it is the replacement-device path, usually scripted.

**`kano-proxy tunnel remove <slug>`** — `DELETE /cli/v1/providers/:id` (closes any live socket) + removes it from local state. `--local-only` skips the server call (e.g. already deleted in the web UI).

**`kano-proxy tunnel list`** — table of this device's registered providers: slug, format, target, connection state, model count, last report time (state read-through to the server, so it reflects the registry's truth).

**`kano-proxy tunnel start`** — the long-running tunnel process: refreshes the access token, opens **one WS per registered provider**, speaks protocol v1, forwards to each provider's target. `--concurrency <1–4>` lowers the in-flight cap below the server's 4 (a raise is refused). Foreground, logs to stderr; daemonize with your own launchd/systemd unit if you want a service (explicit over clever — the CLI does not self-daemonize).

**`kano-proxy agent setup <claude-code|codex>`** — point a coding agent on this machine at Kano Proxy. The docs site's agent pages describe the same configuration by hand; this command performs it, and keeps what it did so `agent restore` can undo it exactly. What it does, in order:

1. Requires a signed-in device (it mints the credential below, so there is nothing to do without one).
2. `GET /cli/v1/models` — the user's live catalog, the same list the Models page shows. **Nothing is invented**: an account with no connected Claude Code subscription has no `claude-code/` ids, and the command says so and stops rather than writing a model name that would only `400`.
3. Builds or updates the agent's **model group** (§ Keys and groups): one group per agent per user, slug `claude-code` / `codex-cli`, display name "Claude Code" / "Codex CLI", looked up by slug so a rerun — on this device or another — **updates** the existing group instead of creating a second one. The group's model **names are the bare upstream ids** (`claude-opus-5`, `gpt-5.6-sol`), each targeting the same id on its provider (`claude-code/claude-opus-5`). This is the whole point of going through a group rather than writing `provider/model` ids into the agent: Claude Code decides its behavior from the model id string — the identity line in its system prompt, which features it enables, how its `/model` picker and its Opus/Sonnet/Haiku slots resolve — and only a real Claude id makes those checks pass; the Codex CLI resolves a model's context window, effort levels and instructions from its bundled catalog, which knows `gpt-5.6-sol` and not `codex/gpt-5.6-sol`. A group endpoint serves those real names and echoes them back in every response, so the agent sees exactly the model it expects while the proxy routes it wherever the group says. Every id of the agent's provider is mirrored into the group (up to the 20-models-per-group cap; the picked names below are placed first), so switching models inside the agent keeps working. **Targets, not fallbacks (CLI 2.5, operator decision 2026-10-12):** each picked name carries the targets the user picked, **in order, from any provider** (`claude-code/…`, `codex/…`, `antigravity/…`, a custom or CLI provider) — the first answers, the rest take over when it is benched or out of quota: ordered failover, exactly as the Groups page would configure it, and editable there afterwards without rerunning the CLI. The agent's own model is not pinned as the first target; a name can be served entirely by another provider, and the agent still sees the name.
4. Mints the agent's **API key**, named `<agent>@<device name>` (`claude-code@my-mac`) via `POST /cli/v1/keys`. A rerun revokes the key it minted last time first (its id is in the state file) and mints a fresh one — a plaintext key exists only inside the agent's config file, never in the state file, so there is nothing to reuse.
5. Writes the agent's config (below), recording for every field it touches the value that was there before (or that it was absent), and taking a **full backup** of the file into `~/.config/kano-proxy/backups/<agent>/<timestamp>-<filename>` (mode `0600`) before the first write ever made to that file. A rerun keeps the original record and the original backup: `restore` returns to before the CLI ever touched the file, not to before the last rerun.
6. Verifies with `GET /g/<slug>/anthropic/v1/models` (or `…/openai/v1/models`) using the new key: the group endpoint must list the names it just wrote. **Never a model request** — verification is a catalog read, which costs nothing upstream.

   **Claude Code** — merges an `env` block into `~/.claude/settings.json` (`%USERPROFILE%\.claude\settings.json` on Windows), the user-scope file only (a project `.claude/settings.json` may be committed, and a key must never land in one). The file is read as JSON and only these keys are set; everything else in the file — permissions, hooks, the `model` Claude Code itself writes when the user picks a default — is kept byte-for-byte in meaning (the JSON is re-serialized, pretty-printed, two-space indent, which is what Claude Code writes too). `ANTHROPIC_BASE_URL` = `<base_url>/g/claude-code/anthropic`; `ANTHROPIC_AUTH_TOKEN` = the key; `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY` = `"1"`, so Claude Code reads the group endpoint's `GET /v1/models` at startup and lists every name the group serves in its `/model` picker, not only the slots; and one `ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU,FABLE}_MODEL` per **slot** that has targets — the name the slot is served under, written out explicitly even when it matches Claude Code's own family name, because Claude Code's built-in defaults can resolve to a dated id (`claude-opus-5-20260301`) that the group does not define. **`ANTHROPIC_MODEL` is never written (CLI 2.5):** it overrides the default the user saves with `/model` on every launch, which is exactly what made a fourth model unpickable; a value already in the file is left alone. A **slot** (CLI 2.5, operator decision 2026-10-12) is an ordered list of `provider/model` targets picked from the whole catalog — the first answers, the rest take over in order — and the name it is served under is **derived, never asked**: the first target's own id when it is a `claude-code` model; else this account's `claude-code` id of the slot's family (`opus`, `sonnet`, `haiku`, `fable`), so a slot routed to `antigravity` or `codex` first still reports a real Claude id; else the first target's upstream id as-is (no `claude-code` account at all). Two slots deriving the same name keep the first slot's targets. A slot with no targets is left to Claude Code's own default and nothing is written for it (the Fable slot on an account without a Fable model, say). The Haiku slot is also where Claude Code runs its background work (session titles and the like), which is why it should stay on a cheap model rather than carry Fable. The root page holds one row per slot — its targets and, in parentheses, the name Claude Code will see — and **a rerun starts from what is in place**: the targets the group holds for the name the config file has for that slot, else the family default (the account's Claude model of that family, `claude-code`'s first, any provider's otherwise, none when there is none). Enter opens a searchable list of every id in the catalog where Space toggles and the order toggled on is the order tried, each row numbered `[1]`, `[2]`… and the resulting list spelled out under it; `--no-tui`: `--opus <targets> [--sonnet <targets>] [--haiku <targets>] [--fable <targets>]`, each a comma-separated list of `provider/model` ids in the order tried (a bare id means `claude-code/<id>`), at least one slot given. `ANTHROPIC_API_KEY` is left alone if present — the docs page documents both and the proxy accepts both; the CLI writes one.

   **Codex CLI** — edits `~/.codex/config.toml` (`%USERPROFILE%\.codex\config.toml`) with `toml_edit`, so comments, ordering and every other key survive: top-level `model` (the picked bare id), `model_provider = "kano"`, `model_reasoning_effort` (picked: `low|medium|high|xhigh`, default `high`), `model_catalog_json = "kano-models.json"`, and the `[model_providers.kano]` table (`name = "Kano Proxy"`, `base_url = "<base_url>/g/codex-cli/openai/v1"`, `experimental_bearer_token = "<key>"`, `wire_api = "responses"`). The key is written into the file deliberately (operator decision 2026-10-11): the alternative, `env_key`, leaves the user to export a variable in every shell, which is not "set up and go"; the file is written `0600`. Then the **model catalog**: the CLI runs `codex debug models --bundled`, keeps the entries whose `visibility` is `list` **and** whose slug is one of the group's names (so the picker shows what this account can actually serve), trims `supported_reasoning_levels` to the efforts the proxy accepts (`low`, `medium`, `high`, `xhigh` — `minimal` and `ultra` are a `400`) and fixes `default_reasoning_level` into that set, and writes `~/.codex/kano-models.json` — the same transformation the docs page's `jq` / PowerShell snippets perform, now with no `jq` and one code path on every platform. A `kano-models.json` that already existed (the docs page's script wrote one) is backed up like the config file and comes back on restore; one the CLI created is deleted on restore. If no `codex` binary is on `PATH`, the catalog step is skipped with a warning and `model_catalog_json` is not written: Codex then runs on its fallback metadata, which the docs page already describes as harmless. TUI: the root page holds the model (the name Codex sees, from the `codex/` ids — Codex needs a name its bundled catalog knows, so this one is asked, not derived), the effort, and the **targets** behind that name, picked from the whole catalog in order on the same numbered list as Claude Code's slots (default: the model itself on `codex`; an emptied list falls back to that) — each starting from the config's current `model` / `model_reasoning_effort` and the group's current targets for that name; `--no-tui`: `--model <bare id> [--effort <e>] [--target <provider/model>…]` (targets in the order given, a bare id meaning `codex/<id>`; none means the model itself).

   Both: `--dry-run` performs steps 1–3's *reads* and prints every field it would write (the key shown as `<new key>`, since a dry run mints nothing) and the group it would create or update, then exits without writing, minting or changing the group.

**`kano-proxy agent restore <claude-code|codex>`** — undo `agent setup` for that agent on this device. Two depths, chosen on purpose (operator decision 2026-10-11):

- **Default — field-level.** Only the fields `setup` wrote are touched: a field that existed before gets its recorded value back, one that did not is removed; for Codex the `[model_providers.kano]` table goes, and `kano-models.json` is deleted if the CLI created it or put back from its backup if it existed before. Everything else in the file — including what the agent itself wrote since (Claude Code persists `/model` choices and permissions into the same `settings.json`) — is untouched. This is the default precisely because a whole-file restore would throw that away.
- **`--full`** — the thorough one: puts the backup taken before the CLI's first write back in place of the whole file, and deletes `kano-models.json` if the CLI created it. The user's every change since is gone with it, so: the diff is printed first and the TUI confirms; `--no-tui` needs `--force`. Before overwriting, the current file is backed up again (`<timestamp>-<filename>.pre-restore`), so even a full restore can be reversed by hand. With several backups on disk the oldest is used (the one from before the first write); `--backup <path>` names another.
- **Drift.** Either depth compares the file's current values against what `setup` recorded it wrote. A field the user edited since is listed, and the TUI asks before overwriting it; `--no-tui` refuses unless `--force`. Silently reverting a hand edit is the one thing a restore must never do.
- **Server side**, both depths: `DELETE /cli/v1/keys/:id` for the key this device minted (a 404 — already revoked from the web UI — is fine). The **group stays**: it is account-level, another device or the user's own clients may be on its endpoint. `--remove-group` deletes it, after a second TUI confirmation when it still carries models the CLI did not write.
- `--dry-run` prints the plan (fields, prior values, the backup that would be used, the key and group ids) and changes nothing. Afterwards the agent's entry leaves the state file; the backups stay on disk.

**`kano-proxy agent list`** — one row per supported agent: whether this device set it up, the group slug, the key name, when it was written, and **drift** — whether the config file's managed fields still hold what the CLI wrote (`ok`, `drifted`, or `missing` when the file is gone). Read-only; no server call.

**`kano-proxy status`** — device auth state (signed in as, token freshness), per-provider tunnel connection state from `GET /cli/v1/providers` — works from any process, and is the truth even while `tunnel start` runs elsewhere — and the `agent list` rows.

**`kano-proxy update`** — self-update: lists recent GitHub Releases, picks the newest `cli-v` one that carries this platform's archive (a release whose assets are still building, or never built, is skipped — never `releases/latest`, which is the product's), downloads it, verifies it against `SHA256SUMS`, atomically replaces its own binary. Upgrades only when that release is numerically newer than the running binary; a dev build ahead of every release is left alone. When the running binary lives inside a package manager's tree (Homebrew cellar, Scoop apps dir) it refuses and prints that manager's upgrade command instead — two owners for one binary is how installs rot.

### State file

`~/.config/kano-proxy/state.json` on macOS/Linux (`%APPDATA%\kano-proxy\state.json` on Windows), mode `0600`, plus a sibling lock file for refresh serialization. Contents: `{base_url, device_id, device_name, refresh_token, providers: [{id, slug, format, target, target_key, expose}], agents: {<agent>: {key_id, key_name, group_id, group_slug, config_path, backup_path, written_at, fields: {<field>: {before, wrote}}, catalog_path?, catalog_backup_path?}}}`. `fields` is the restore record: per managed field, the value found before the first write (`null` = absent) and the value the CLI last wrote (what drift is measured against) — with the key itself replaced by the marker `<key>`, so drift on the key field means "no proxy key where the CLI put one"; `catalog_path` is set when the CLI wrote the Codex catalog file, `catalog_backup_path` when one existed before. **No plaintext API key is ever stored here** — only the key's id and name; the plaintext lives in the agent's config file and nowhere else. **The state file must be writable at runtime** — refresh rotation persists a new token on every refresh, which is also why there is no "token via environment variable" mode: a static injected token would be superseded by its own first use. Docker/headless: mount the state directory as a writable volume and run `login`/`tunnel add` once against it.

Backups live beside it in `~/.config/kano-proxy/backups/<agent>/` (`%APPDATA%\kano-proxy\backups\` on Windows), mode `0600`, never deleted by the CLI.

### First-run walkthrough

```console
# 1. Install (any channel from § Distribution):  brew install yufeng-kano/tap/kano-proxy
$ kano-proxy login
  Server [https://kano-proxy.yuufeng.com]: ⏎   (self-hosters type their own origin)
  Device name [my-mac]: ⏎
  → approve this device at: https://kano-proxy.yuufeng.com/cli/authorize?request=…  (sign in with Google, press Approve)
  Approve this device at: https://kano-proxy.yuufeng.com/cli/authorize?request=…   ← shown on the code screen too
  Code shown in browser: XXXX-XXXX
  ✓ device "my-mac" signed in

# 2a. Point Claude Code at the proxy
$ kano-proxy agent setup claude-code
  Opus: claude-opus-5  ← antigravity/claude-opus-5-high › claude-code/claude-opus-5
  Sonnet: claude-sonnet-5  ← claude-code/claude-sonnet-5   Haiku: claude-haiku-4-5  ← claude-code/claude-haiku-4-5   Fable: claude-fable-5-1  ← claude-code/claude-fable-5-1
  ✓ group "claude-code" — 9 models   ✓ key "claude-code@my-mac"   ✓ wrote ~/.claude/settings.json
  ✓ https://kano-proxy.yuufeng.com/g/claude-code/anthropic lists claude-opus-5, claude-sonnet-5, claude-haiku-4-5 and 6 more

# 2b. Expose a local model
$ kano-proxy tunnel add
  Slug [my-mac]: ⏎   Type: openai   Target [http://localhost:11434/v1]: ⏎
  ✓ found 3 models — [All models (follow local server)] ⏎
  ✓ registered "my-mac"
$ kano-proxy tunnel start
  my-mac: connected (proto 1), 3 models reported
#    POST https://proxy.example.com/openai/v1/chat/completions  model: "my-mac/llama3.3:70b"
#    …or add "my-mac/…" as a target in a model group for failover.

# 3. Undo, when the time comes
$ kano-proxy agent restore claude-code      # fields back to before; key revoked; group kept
$ kano-proxy logout                         # restores every agent, revokes the device
```

### Runtime behavior

- Streams both directions chunk-for-chunk (no whole-body buffering, per the streaming guardrail), honors `cancel` by aborting the local request, refuses any `path` outside the allowlist, and joins paths onto exactly the one configured target base per provider — structurally not a general proxy.
- Reconnects forever on **network** failures: exponential backoff 1 s → 60 s with full jitter, per provider socket; heartbeat text `ping` every 30 s (answered by the server with `pong`). Close `4003 token_expired` → refresh and reconnect silently. HTTP 401 / failed refresh (device revoked or unregistered) does **not** retry: exit with a message to run `kano-proxy login` again. Close `4001 replaced` (another `start` took the provider over) logs and drops that provider rather than fighting its sibling.
- `SIGINT`/`SIGTERM`: close sockets with code 1000, abort in-flight local requests, exit 0.
- Exit codes: `0` clean shutdown, `1` usage/config error, `2` auth rejected, `130` cancelled by the user. Logs are one line per lifecycle event and per request (`id`, method, path, status, duration) — never bodies, never headers, never tokens ([logging.md](./logging.md) discipline applies to the CLI too). `agent` commands never print a key: a dry run shows `<new key>`, a status line shows the key's name.

## Keys and groups — what `agent setup` owns on the server

- **Keys are ordinary `api_keys` rows** ([auth.md](./auth.md) § Client API keys) with one extra fact: `device_id`, the CLI device that minted them (`NULL` for keys created in the web UI). They appear on the Access page like any other key, carry spend limits like any other key, and can be revoked there like any other key. The column is a foreign key with `ON DELETE CASCADE`: **revoking a device revokes the keys it minted** — from `kano-proxy logout`, from the Access page, or by refresh-token reuse detection — so a lost laptop needs one action, not a hunt through the key list. The web UI's revoke confirmation states how many keys go with the device. A device may only delete keys it minted itself through `/cli/v1/keys/:id`; the Access page can delete any of the user's keys.
- **Groups are ordinary model groups** ([providers.md](./providers.md) § Model groups) — account-level, not device-level: one `claude-code` and one `codex-cli` group per user, shared by every device, visible and editable on the Groups page. The CLI only ever writes the models it computes, and a rerun merges rather than replaces: CLI-computed targets first, then any other target the user stored on that name on the Groups page, and models the user added under other names stay too, all within the 20-model cap. Nothing deletes a group except `agent restore --remove-group` or the Groups page.
- Both are created through the device-token namespace (§ Server routes) with the **same validators and the same cap checks** as the session routes — the CLI is not a privileged client, just an authenticated one.
## Server routes

| Route | Auth | Purpose |
|---|---|---|
| `POST /cli/v1/login/start` | none (per-IP rate-limited) | Create login request → `{request_id, verify_url, expires_at}` |
| `POST /cli/v1/login/complete` | pairing code | Redeem approved request → device + first token pair |
| `POST /cli/v1/token` | refresh token | Rotate → new refresh + access; superseded-token reuse revokes the device |
| `DELETE /cli/v1/device` | Bearer access | The device revokes itself (`kano-proxy logout`): deletes its row, which cascades to the keys it minted. Idempotent |
| `GET /cli/v1/connect/:providerId` | Bearer access | WebSocket upgrade → tunnel registry |
| `GET /cli/v1/providers` | Bearer access | List the user's CLI providers + live connection state (read from the registry) |
| `POST /cli/v1/providers` | Bearer access | Create (slug, format, optional expose filter / initial models) |
| `DELETE /cli/v1/providers/:id` | Bearer access | Delete + force-close socket |
| `GET /cli/v1/models` | Bearer access | The user's catalog — the same `{object, data: [{id, provider, upstream, display_name, available}], providers: [{provider, count, error, cached}]}` shape as `GET /api/models` ([auth.md](./auth.md)), read through the same catalog module. What `agent setup` picks from |
| `POST /cli/v1/keys` | Bearer access | Mint an API key for this device: body `{name}` (1–64 chars, trimmed); response is the `POST /api/keys` shape with the plaintext `key` once, and `device_id` = the caller. No limit fields — limits are set on the Access page |
| `DELETE /cli/v1/keys/:id` | Bearer access | Revoke a key **this device minted**; another device's or the web UI's keys are a 404 here |
| `GET /cli/v1/model-groups` | Bearer access | The user's groups, exactly the `GET /api/model-groups` shape |
| `POST /cli/v1/model-groups` | Bearer access | Create — same body, validation, caps and response as `POST /api/model-groups` |
| `PUT /cli/v1/model-groups/:id` | Bearer access | Update — same as `PUT /api/model-groups/:id` |
| `DELETE /cli/v1/model-groups/:id` | Bearer access | Delete — same as `DELETE /api/model-groups/:id` |

`/cli/v1/*` is its own namespace beside `/api/*` (session) and the LLM surfaces: token-authenticated, permissive CORS unnecessary (no browser callers), never session-authenticated. The key and model-group routes are the session routes' handlers behind a different authenticator — one validator, one cap, one response shape — not a second implementation; the session-side management routes the web UI uses (`/api/cli/*` — devices, device rename, revoke, providers, rename, delete, login-request read/approve/deny) are listed in [auth.md](./auth.md) § CLI devices and providers.

**`/agent/v1/*` is gone (operator decision 2026-10-11).** The namespace was the one part of the subsystem not named `cli` — the tables are `cli_*`, the session routes `/api/cli/*`, the page `/cli/authorize` — and `agent` now means a coding agent. It is not aliased: every `/agent/v1/*` path answers `410 Gone` with `{"error": "this kano-proxy CLI release is no longer supported — run `kano-proxy update` (2.0 or newer)"}`, which a 1.x binary prints verbatim (`server said (410 Gone): …`) before exiting, and `kano-proxy update` fetches from GitHub, not from this server, so the upgrade path is open regardless. The 410 handler is removed with the next major core release. A reverse proxy in front of the server routes `/cli/v1/*` to it (and `/agent/*` until that handler goes) — [deployment.md](./deployment.md).

## Web UI

No dedicated page and no sidebar nav item for the CLI — its surfaces live inside the existing pages, plus the session-gated authorize view the login flow lands on:

| Surface | Content |
|---|---|
| Access page (`/access`), **Devices** tab | **CLI devices** (name, last seen, created, keys minted — row actions: rename, revoke; the revoke confirmation names how many keys it takes along). When there are no devices the card shows the install one-liners, `kano-proxy login --base-url <this instance's origin>` (the browser's `location.origin`, so every instance teaches its own address — the CLI's compiled-in default is the hosted edition) and a Releases link. Devices live here, not on Providers, because a device is a credential — a rotating token family that can mint keys — and the Access page is where the user's credentials are (operator decision 2026-10-11). |
| Access page, **Keys** tab | Keys minted by a device read `from <device name>` under their name, so the link is visible where keys are revoked. |
| Providers page (`/providers`), **All** and **CLI** tabs | **CLI providers** (slug, format, connection state chip, model count + last report time, registered-from device — row actions: rename display name, delete). No create flow — creation is `kano-proxy tunnel add`'s job; the empty state says so. |
| `/cli/authorize?request=…` | Approve view for a pending login, rendered as a **bare page outside the shell** — blank page, one centered card: device name + Approve/Deny; on approve, displays the one-time code. Session required (redirects through login like any admin page). |

`/cli` is a permanent redirect to `/providers`. The Models page and group-target pickers include CLI providers like any other provider (ids are `<slug>/<model>`); the groups `agent setup` writes are ordinary groups on the Groups page. Detailed layout rules: [admin-ui.md](./admin-ui.md) § Access page, § CLI section, § CLI authorize view.
## Distribution

The CLI is versioned and released **independently of the product**: its own SemVer line (`apps/cli/Cargo.toml`, restarted at `1.0.0` when it was decoupled from the product's 4.x), its own tag prefix `cli-vX.Y.Z`, its own workflow, released only when the CLI itself changed ([deployment.md](./deployment.md) § CLI release). It shipped in lockstep with the product from v4.4.0 to v4.5.2; that coupled every server hotfix to a five-platform rebuild and a tap/scoop bump for a binary that had not changed, and one missed `Cargo.toml` bump was enough to publish a release with no CLI assets. Wire compatibility is governed by the protocol's `proto` number, never by comparing version strings — a 1.x CLI talks to a 4.x server. The repository is public, so Release assets are directly downloadable by end users of any kano-proxy instance — they need no relationship with the repo.

**`cli-release.yml`** runs on a `cli-v…` Release publish: checks tag == `Cargo.toml`, runs `cargo test`, cross-compiles the five targets, packages `kano-proxy-<version>-<target>.tar.gz` (`.zip` for Windows), emits a `SHA256SUMS` file covering all archives, and attaches everything to the Release.

Install channels, all fed from those assets:

| Channel | Platforms | Mechanics |
|---|---|---|
| Homebrew tap | macOS, Linux | `brew install yufeng-kano/tap/kano-proxy` — formula in our own public `homebrew-tap` repo, version + checksums bumped automatically by release CI |
| Install script | macOS, Linux | `curl -fsSL https://raw.githubusercontent.com/yufeng-kano/kano-proxy/main/scripts/install-cli.sh \| sh` — detects OS/arch, downloads the newest `cli-v` release that has this platform's archive, verifies `SHA256SUMS`, installs to `~/.local/bin` (or `--dir`) |
| Scoop bucket | Windows | `scoop bucket add kano https://github.com/yufeng-kano/scoop-bucket` + `scoop install kano-proxy` — manifest in our own public bucket repo, bumped by release CI |
| `kano-proxy update` | all | self-update from the newest `cli-v` Release with assets, checksum-verified (see the command above) |

**None of these channels involves registration or third-party review** — a Homebrew *tap* and a Scoop *bucket* are just public repos we own, live the moment they exist; review queues only guard homebrew-core and Scoop's official buckets, neither of which we use. winget (Microsoft's review queue) is deliberately not offered — Scoop covers Windows. No crates.io publish: this is a product binary, not a library, and the monorepo layout gives a Cargo publish nothing but friction.

The web UI's `/cli` empty state shows the brew and script one-liners (from the message catalog, per [i18n.md](./i18n.md)) and links the Releases page — every instance distributes the same official binaries; instances never host binaries themselves.

## Cost and limits

- **Idle is ~free:** a connected-but-quiet agent is one open socket and a heartbeat; the hourly expiry check is one brief wake per provider per hour.
- **Active streams cost what any proxied stream costs**: one task and the bounded buffers below. At personal-local-LLM traffic volumes this is noise; it is the cost of owning the tunnel instead of renting trycloudflare.
- One socket per provider, ≤ 20 providers per user (shared cap with custom), ≤ 20 devices per user — no fan-in bottleneck to reason about.
- Request bodies: bounded by the same request-size realities as the rest of the proxy; a body the local server will not accept fails at the local server, passed through as its own status.

## Security notes

- No permanent secrets: access tokens live 1 h, refresh tokens rotate on every use with reuse-as-theft revocation, devices are individually revocable from the Access page and by `kano-proxy logout`, revocation reaches live sockets within the access TTL via the expiry alarm, and the API keys a device minted are deleted with it.
- Login codes and refresh tokens are stored hashed; login starts are per-IP rate-limited (10 / 10 min / IP) **atomically** — the budget check rides inside the request-row INSERT (counting recent rows by the IP's SHA-256, never the raw address), so parallel batches from one address cannot slip past a read-modify-write counter. Fails closed without touching unrelated state. Expired `cli_login_requests` rows are purged by the daily retention sweep ([logging.md](./logging.md)).
- The tunnel carries prompts/completions as opaque bytes; nothing content-shaped is logged at the server, the registry, or the CLI ([logging.md](./logging.md) rules apply unchanged).
- Cross-user isolation is inherited: every path into the registry starts from a user-scoped row lookup, and the registry key is the provider row id.
- The CLI's target confinement (one base URL per provider + path allowlist, enforced at both ends) means a compromised proxy operator still cannot use connected tunnels as general SSRF proxies into home networks.
- A device token is not a privileged credential: the key and group routes it reaches run the session routes' validators and caps, a device can only revoke keys it minted, and a plaintext key is returned once and stored nowhere on the device but inside the agent's own config file (`0600`), where the agent needs it anyway.

## Implementation order

1. Migration `0015` (`cli_devices`, `cli_login_requests`, `cli_providers`) + [database.md](./database.md) update.
2. Device auth: `login/start`, `login/complete`, `token` routes + `CLI_TOKEN_SECRET` + web authorize view + [auth.md](./auth.md) route-table update.
3. The tunnel registry and the protocol module shared as types, `connect`/`providers` routes.
4. Routing third branch in `routing/candidates`, injected-fetch reuse of the custom adapters, fault-marker handling in `routing/feedback`, catalog section — + [providers.md](./providers.md) pointer.
5. `/cli` page + authorize view + [admin-ui.md](./admin-ui.md) update.
6. **CLI 2.0 (2026-10-11):** migration `0003` (`api_keys.device_id`), the `/cli/v1` namespace with the `410` on `/agent/v1`, the key / model-group / device routes, the Access page (devices moved there from Providers), then the `tunnel` / `agent` / `logout` command tree and the `agent setup` / `restore` writers — docs-site agent pages updated to lead with the command.
6. `apps/cli` (Rust): `cargo test` wired into CI; distribution pipeline per § Distribution — `cli-release.yml` (five targets + `SHA256SUMS` on the `cli-v` Release), `scripts/install-cli.sh`, Homebrew tap + Scoop bucket automation, `kano-proxy update`.

Tests are protocol-level with in-memory socket pairs and stubbed `fetch` throughout ([testing.md](./testing.md)) — a local LLM is free, but the test suite still never assumes one is running.
