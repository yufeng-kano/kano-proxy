-- 60s server-side usage cache, shared across every device and tab.
--
-- The admin UI's localStorage TTL is per-device, so N signed-in devices meant
-- N× upstream usage calls against APIs that rate-limit and bot-wall. These
-- columns move the cache server-side (docs/providers.md § Usage cache).
--
-- `usage_fetching_at` is a single-flight lock, not data: D1 has no
-- cross-request transactions, so a conditional UPDATE on this column is used
-- as a compare-and-swap. NULL means free.

ALTER TABLE upstream_accounts ADD COLUMN usage_snapshot_json TEXT;
ALTER TABLE upstream_accounts ADD COLUMN usage_fetched_at TEXT;
ALTER TABLE upstream_accounts ADD COLUMN usage_fetching_at TEXT;
