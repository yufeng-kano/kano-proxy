-- User-defined bare-name model aliases → ordered provider/model target
-- lists (docs/providers.md § Model groups). Mirrors custom_providers'
-- ON DELETE CASCADE shape (0002_custom_providers.sql), but `name` is
-- mutable (rename is allowed — nothing else references it, unlike a
-- custom provider's slug).

CREATE TABLE model_groups (
  id TEXT PRIMARY KEY NOT NULL,
  user_id TEXT NOT NULL,
  name TEXT NOT NULL,
  targets_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
  UNIQUE (user_id, name)
);

CREATE INDEX model_groups_user_id_idx ON model_groups(user_id);

-- Preserves the alias a request was addressed to; provider/model on the row
-- still store the expanded canonical target so pricing/Overview stay exact.
ALTER TABLE request_logs ADD COLUMN group_name TEXT;
