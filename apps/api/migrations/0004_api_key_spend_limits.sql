-- Spend tracking + per-key spend limits (docs/pricing.md).
--
-- request_logs.cost: estimated USD computed at write time from the LiteLLM
-- price table; NULL = unpriced/unknown, never a guessed zero.
--
-- api_keys spend-limit fields: NULL spend_limit = unlimited. The interval and
-- include-oauth flag carry defaults so existing rows stay valid.

ALTER TABLE request_logs ADD COLUMN cost REAL;

ALTER TABLE api_keys ADD COLUMN spend_limit REAL;
ALTER TABLE api_keys ADD COLUMN spend_limit_interval TEXT NOT NULL DEFAULT 'monthly';
ALTER TABLE api_keys ADD COLUMN spend_limit_include_oauth INTEGER NOT NULL DEFAULT 1;

-- The spend-limit window sum filters on (api_key_id, created_at); the existing
-- request_logs_user_created_idx covers only (user_id, created_at).
CREATE INDEX request_logs_api_key_created_idx ON request_logs(api_key_id, created_at);
