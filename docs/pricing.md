# Pricing & spend

Estimated USD cost per request, computed from logged token counts and a public per-model price table. Powers the Overview spend cards and per-API-key spend limits ([admin-ui.md](./admin-ui.md), [auth.md](./auth.md)).

**Estimates, not invoices.** Costs are derived from `request_logs` token fields and list prices; they exist so an operator can bound and compare usage, not to reconcile a bill. Subscription (OAuth) traffic through `claude-code` / `codex` / `grok` is priced at the same list rates — that is the "what would this have cost via API" number, which is also what makes the per-key include-OAuth toggle meaningful.

## Price table

- **Primary source:** LiteLLM's community-maintained table — `https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json`.
- **OpenRouter source:** for rows whose provider prefix is exactly `openrouter`, the public OpenRouter catalog — `https://openrouter.ai/api/v1/models` — is authoritative. It supplies provider-specific prices that LiteLLM may not list. Its default `pricing.prompt`, `pricing.completion`, `pricing.input_cache_read`, and `pricing.input_cache_write` rates map to this proxy's input, output, cache-read, and cache-creation rates. A catalog record must supply both prompt and completion rates to be usable; partial records are unpriced, never completed with a zero rate. Models with catalog `pricing.overrides` are also left unpriced: this proxy records aggregate token counts, not the threshold/timestamp inputs needed to apply conditional rates safely. Fixed request, image, web-search, and other non-token catalog charges are likewise outside the stored token-cost formula.
- Both sources are fetched server-side, trimmed to the four per-token rates this proxy uses, source-tagged before combination, and cached in the `CACHE` KV namespace for **24h** (key `pricing:litellm:v1`) — one KV write per day, per the minimal-KV rule. A per-isolate in-memory memo fronts the KV read, so steady-state request handling costs ~0 KV operations. Only a source-tagged OpenRouter catalog entry can price an OpenRouter row; a LiteLLM `openrouter/<id>` entry, including one in a legacy combined snapshot, never can.
- Source failures serve the last source-tagged copy regardless of age. A failed OpenRouter catalog fetch still refreshes LiteLLM-only prices, but leaves OpenRouter rows unpriced when no prior catalog copy exists; with no applicable copy or source entry, costs are `NULL` (unknown), never guessed. Pricing must never fail or delay a proxied request — the table refresh happens via `waitUntil`, off the critical path.

## Model matching

`request_logs.model` is `provider/upstream…`; the LiteLLM table keys are bare model ids, sometimes vendor-prefixed. Matching is normalization + a fallback chain, first hit wins:

1. Normalize: lowercase, strip a trailing bracket variant (`claude-opus-5[1m]` → `claude-opus-5`).
2. For an `openrouter/...` row, exact match only on a source-tagged OpenRouter catalog entry for `openrouter/<full upstream id>`. This preserves the OpenRouter provider-specific rate and prevents a LiteLLM, bare/provider, or Cloudflare price from pricing an OpenRouter request.
3. For every other provider, exact match on the upstream id (everything after the first `/`).
4. The upstream id with its own leading path segments progressively stripped (`openai/gpt-4o-mini` → `gpt-4o-mini`).
5. Common LiteLLM prefix forms for the segment (`anthropic/<id>`, `openai/<id>`, `xai/<id>`, `gemini/<id>`, `openrouter/<full upstream id>`).

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
