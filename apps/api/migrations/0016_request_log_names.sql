-- Name snapshots on request_logs (docs/database.md § request_logs): the API
-- key's name and the serving account's display label as they were when the
-- row was written, so a key or account deleted later is still read by the
-- name the operator knew it by instead of a bare "removed" tag.
ALTER TABLE request_logs ADD COLUMN api_key_name TEXT;
ALTER TABLE request_logs ADD COLUMN account_label TEXT;
