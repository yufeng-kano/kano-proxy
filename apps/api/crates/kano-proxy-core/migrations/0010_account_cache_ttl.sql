-- "Keep prompt cache for 1 hour" per account (docs/providers.md § Request edits): with it on,
-- every Anthropic `cache_control` marker in a request sent to this account goes out with
-- `ttl: "1h"`. Default off on every existing row, so the upgrade changes no request. Only
-- `PATCH /api/providers/:provider/accounts/:id` writes it.
ALTER TABLE upstream_accounts ADD COLUMN force_cache_ttl_1h BOOLEAN NOT NULL DEFAULT FALSE;
