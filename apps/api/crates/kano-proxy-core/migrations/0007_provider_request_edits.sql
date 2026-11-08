-- Per-provider request edits (docs/providers.md § Request edits): "Remove fast mode" and
-- "Remove the Claude Code line", one pair per (user, provider), default off. Only
-- `PATCH /api/providers/:provider` writes them; a missing row reads as both off.
ALTER TABLE provider_settings ADD COLUMN strip_fast_mode BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE provider_settings ADD COLUMN strip_claude_code_line BOOLEAN NOT NULL DEFAULT FALSE;
