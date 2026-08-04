# Pricing & spend

Estimated USD cost per request, computed from logged token counts and a public per-model price table. Powers the Overview spend cards and per-API-key spend limits ([admin-ui.md](./admin-ui.md), [auth.md](./auth.md)).

**Estimates, not invoices.** Costs are derived from `request_logs` token fields and list prices; they exist so an operator can bound and compare usage, not to reconcile a bill. Subscription (OAuth) traffic through `claude-code` / `codex` / `grok` is priced at the same list rates — that is the "what would this have cost via API" number, which is also what makes the per-key include-OAuth toggle meaningful.

## Price table

- **Source:** LiteLLM's community-maintained table — `https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json`.
- Fetched server-side, trimmed to the four per-token rates this proxy uses (`input_cost_per_token`, `output_cost_per_token`, `cache_read_input_token_cost`, `cache_creation_input_token_cost`), and cached in the `CACHE` KV namespace for **24h** (key `pricing:litellm:v1`) — one KV write per day, per the minimal-KV rule. A per-isolate in-memory memo fronts the KV read, so steady-state request handling costs ~0 KV operations.
- Fetch failure serves the last KV copy regardless of age; with no copy at all, costs are `NULL` (unknown), never guessed. Pricing must never fail or delay a proxied request — the table refresh happens via `waitUntil`, off the critical path.

## Model matching

`request_logs.model` is `provider/upstream…`; the LiteLLM table keys are bare model ids, sometimes vendor-prefixed. Matching is normalization + a fallback chain, first hit wins:

1. Normalize: lowercase, strip a trailing bracket variant (`claude-opus-5[1m]` → `claude-opus-5`).
2. Exact match on the upstream id (everything after the first `/`).
3. The upstream id with its own leading path segments progressively stripped (`openai/gpt-4o-mini` → `gpt-4o-mini`).
4. Common LiteLLM prefix forms for the segment (`anthropic/<id>`, `openai/<id>`, `xai/<id>`, `gemini/<id>`, `openrouter/<full upstream id>`).

No match → `cost` stays `NULL` and the row is reported as unpriced. **Never fabricate a rate.**

## Cost formula

`prompt_tokens` is the **total** input count including cache reads/writes ([database.md](./database.md)), so:

```text
uncached_input = prompt_tokens − cache_read_input_tokens − cache_creation_input_tokens   (floor 0)
cost = uncached_input × input_rate
     + cache_read_input_tokens × cache_read_rate
     + cache_creation_input_tokens × cache_creation_rate
     + completion_tokens × output_rate
```

A table entry missing a cache-read or cache-creation rate falls back to the plain input rate for that component (an upstream without cache pricing bills cached input as normal input). `NULL` token fields count as 0 in the formula; a row whose token fields are **all** `NULL` (no usage reported) has `cost = NULL`.

## Storage & backfill

- `request_logs.cost` (REAL, USD, nullable) is computed **at write time** inside `logRequest` — one choke point for every logging call site. A pricing failure degrades to `NULL` cost; it never fails the log write or the request.
- Rows from before the column existed (or unpriced at write time) have `NULL` cost. `GET /api/usage/summary` re-prices those rows **at read time** with the same shared resolver, so history isn't blank. The response marks how much of the range is estimated vs unpriced.

## Per-key spend limits

`api_keys` carries three optional limit fields ([database.md](./database.md), [auth.md](./auth.md)):

| Field | Meaning |
|-------|---------|
| `spend_limit` (REAL, nullable) | USD ceiling; `NULL` = unlimited |
| `spend_limit_interval` (`daily` \| `weekly` \| `monthly` \| `total`) | Reset window: UTC day / ISO week (Mon 00:00 UTC) / 1st of month 00:00 UTC / never |
| `spend_limit_include_oauth` (0/1) | Whether builtin-provider (subscription OAuth) traffic counts toward the limit; custom-provider (BYO key) traffic always counts |

Enforcement lives in the API-key auth middleware, only when `spend_limit` is set and only for **POST** requests (chat/completions, messages, count_tokens — `GET /models` stays free so a blocked client can still see its catalog): sum `cost` over that key's rows since the window start (excluding builtin providers when `include_oauth = 0`); at or over the limit → **429** with `code: "spend_limit_exceeded"` (Anthropic surface: `rate_limit_error` type), before any upstream call. Each refusal logs one `request_logs` row (`error_code: "spend_limit_exceeded"`, no token fields, no cost). Notes:

- The check reads only stored `cost` values (one indexed `SUM`), so pre-migration `NULL`-cost rows don't count toward limits — limits act on traffic from this feature's deploy onward.
- The window sum runs behind a per-isolate 60s memo per key, so a busy key does not add a D1 aggregate to every request.
- Fail-open on infrastructure error (a D1 hiccup must not take the proxy down), fail-closed on a real over-limit read — consistent with the rate-limit guardrail in `CLAUDE.md`.
- `total` never resets but is still bounded by the `request_logs` retention sweep (90 days by default) — spend older than retention ages out of the sum. Documented limitation.
