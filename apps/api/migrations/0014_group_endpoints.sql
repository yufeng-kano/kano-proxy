-- v4 model groups: a group becomes a virtual endpoint (docs/providers.md §
-- Model groups). `model_groups` loses `targets_json` and gains a URL `slug`;
-- the callable ids move from `model_group_aliases` (every alias → the group's
-- one shared target list) to `model_group_models` (every model → its own
-- target list).
--
-- Seeding: every alias becomes one model carrying a copy of its group's whole
-- target list, so every (group, alias) pair that was callable before maps to
-- a (slug, model name) pair on the group's endpoint with identical routing.
-- The slug is backfilled deterministically from the row id ('g-' plus 13 id
-- chars) — valid per the slug regex, collision-free because ids are unique,
-- and renameable in the UI afterwards.

CREATE TABLE model_groups_v4 (
  id TEXT PRIMARY KEY NOT NULL,
  user_id TEXT NOT NULL,
  name TEXT NOT NULL,
  slug TEXT NOT NULL,
  strategy TEXT NOT NULL DEFAULT 'ordered',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
  UNIQUE (user_id, name),
  UNIQUE (user_id, slug)
);

INSERT INTO model_groups_v4 (id, user_id, name, slug, strategy, created_at, updated_at)
SELECT id, user_id, name, 'g-' || lower(substr(id, 6, 13)), strategy, created_at, updated_at
FROM model_groups;

CREATE TABLE model_group_models (
  id TEXT PRIMARY KEY NOT NULL,
  user_id TEXT NOT NULL,
  group_id TEXT NOT NULL,
  name TEXT NOT NULL,
  targets_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
  FOREIGN KEY (group_id) REFERENCES model_groups_v4(id) ON DELETE CASCADE,
  UNIQUE (group_id, name)
);

CREATE INDEX model_group_models_group_id_idx ON model_group_models(group_id);

INSERT INTO model_group_models (id, user_id, group_id, name, targets_json, created_at, updated_at)
SELECT lower(hex(randomblob(16))), a.user_id, a.group_id, a.alias, g.targets_json, a.created_at, a.created_at
FROM model_group_aliases a
JOIN model_groups g ON g.id = a.group_id;

DROP TABLE model_group_aliases;
DROP TABLE model_groups;

ALTER TABLE model_groups_v4 RENAME TO model_groups;

CREATE INDEX model_groups_user_id_idx ON model_groups(user_id);
