-- Splits the callable bare id off `model_groups.name` into its own table:
-- `model_groups.name` becomes a free-text display name (still unique per
-- user), and `model_group_aliases` holds 1..N callable aliases per group,
-- unique per user across ALL of that user's groups (docs/providers.md §
-- Model groups "Display name" / "Aliases"). Mirrors model_groups' own
-- ON DELETE CASCADE shape (0008_model_groups.sql).
--
-- Every pre-existing group's then-`name` was, until now, itself the one
-- callable id — seed it as that group's first alias so nothing callable
-- breaks. `model_groups.name` is left untouched (it becomes the display
-- name as-is, which is a reasonable display name for a group nobody has
-- renamed yet).

CREATE TABLE model_group_aliases (
  id TEXT PRIMARY KEY NOT NULL,
  user_id TEXT NOT NULL,
  group_id TEXT NOT NULL,
  alias TEXT NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
  FOREIGN KEY (group_id) REFERENCES model_groups(id) ON DELETE CASCADE,
  UNIQUE (user_id, alias)
);

CREATE INDEX model_group_aliases_group_id_idx ON model_group_aliases(group_id);

INSERT INTO model_group_aliases (id, user_id, group_id, alias, created_at)
SELECT lower(hex(randomblob(16))), user_id, id, name, created_at
FROM model_groups;
