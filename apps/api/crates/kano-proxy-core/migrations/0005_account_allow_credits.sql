-- Per-account opt-in to credit spillover (docs/providers.md § Codex "Credit spillover"):
-- when true and the account's stored `credits` read usable, an exhausted usage window no
-- longer makes it unusable for routing. Spends paid credits, so it defaults off and only
-- `PATCH /api/providers/:provider/accounts/:id` writes it, for an adapter that supports it.
ALTER TABLE upstream_accounts ADD COLUMN allow_credits BOOLEAN NOT NULL DEFAULT FALSE;
