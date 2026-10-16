-- Benches that cover one quota scope of an account rather than all of it
-- (docs/providers.md § Routing module "Quota scopes"): Antigravity meters Gemini,
-- Argon and Claude/GPT separately, so a 429 on one must not park the others.
-- {"<scope>": {"until": "<ISO>", "reason": "429"}}; same monotonic, read-time-expiry
-- rules as bench_until, per entry.
ALTER TABLE upstream_accounts ADD COLUMN bench_scopes_json TEXT;
