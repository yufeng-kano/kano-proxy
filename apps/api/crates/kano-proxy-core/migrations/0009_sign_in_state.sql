-- What the server knows about an account's sign-in beyond its current bench
-- (docs/providers.md § Sign-in state). `refresh_failure_json` is the last failed OAuth token
-- refresh, `{at, status, refused, expires_at}`: written by the refresh single-flight, it paces the retry
-- and is what lets the Providers page say a sign-in is about to stop working.
-- `sign_in_rejected_at` is when upstream last answered 401 for the stored credential, so an
-- expired sign-in still reads expired after its bench runs out. Both are nulled by a successful
-- refresh and by a reconnect; existing rows start with neither.
ALTER TABLE upstream_accounts ADD COLUMN refresh_failure_json TEXT;
ALTER TABLE upstream_accounts ADD COLUMN sign_in_rejected_at TEXT;
