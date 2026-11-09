-- Request edits move from the provider to the account (docs/providers.md § Request edits): each
-- account gets its own "Remove fast mode" and "Remove the Claude Code line", default off. Only
-- `PATCH /api/providers/:provider/accounts/:id` writes them. The provider's pair from 0007 is
-- copied onto every account its user owns under that provider, so an account used by its owner
-- behaves as before. A lent account changes: it now follows only its owner's switches, where
-- before the borrower's Remove fast mode also counted and the line needed both. The
-- provider_settings columns stay, unread, because a core build from before this migration reads
-- them on every request (docs/database.md § provider_settings).
ALTER TABLE upstream_accounts ADD COLUMN strip_fast_mode BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE upstream_accounts ADD COLUMN strip_claude_code_line BOOLEAN NOT NULL DEFAULT FALSE;
UPDATE upstream_accounts a
   SET strip_fast_mode = s.strip_fast_mode, strip_claude_code_line = s.strip_claude_code_line
  FROM provider_settings s
 WHERE s.user_id = a.user_id AND s.provider = a.provider
   AND (s.strip_fast_mode OR s.strip_claude_code_line);
