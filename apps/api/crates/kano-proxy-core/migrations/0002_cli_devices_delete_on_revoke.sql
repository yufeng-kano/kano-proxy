-- Revoking a CLI device now deletes its row (docs/cli.md § Device auth), so the
-- revoked history goes and the column that marked it goes with it.
DELETE FROM cli_devices WHERE revoked_at IS NOT NULL;
ALTER TABLE cli_devices DROP COLUMN revoked_at;
