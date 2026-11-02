-- The usage notice every signed-in user accepts (docs/auth.md § Usage notice). The two
-- columns are what the session gate reads; NULL means never accepted, which is every user
-- who existed before the notice, so they accept at their next web sign-in.
ALTER TABLE users ADD COLUMN notice_version TEXT;
ALTER TABLE users ADD COLUMN notice_accepted_at TEXT;

-- Every version a user accepted and when, so an earlier acceptance survives a newer one.
CREATE TABLE notice_acceptances (
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  version TEXT NOT NULL,
  accepted_at TEXT NOT NULL,
  PRIMARY KEY (user_id, version)
);
