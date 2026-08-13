-- Routing module (docs/providers.md § Routing module): per-group and
-- per-provider-pool strategy config. `ordered` is the only accepted value
-- today for both; unknown values are rejected at write time (routes) and
-- degrade to `ordered` on read (forward compat for a future value this
-- deploy doesn't know about yet).

-- Array order stays the priority signal for `ordered`; future strategies
-- (usage-balanced, spend-aware) select by this column without a dispatch
-- change (docs/providers.md § Model groups "Future").
ALTER TABLE model_groups ADD COLUMN strategy TEXT NOT NULL DEFAULT 'ordered';

-- Pool-level counterpart of model_groups.strategy, governing direct
-- provider/model calls (docs/database.md `provider_settings`). No FK to
-- custom_providers — a row for a deleted slug is inert, mirroring how
-- upstream_accounts rows are handled. A missing row means `ordered`; rows
-- are created lazily on first PATCH, never backfilled.
CREATE TABLE provider_settings (
  user_id TEXT NOT NULL,
  provider TEXT NOT NULL,
  strategy TEXT NOT NULL DEFAULT 'ordered',
  updated_at TEXT NOT NULL,
  PRIMARY KEY (user_id, provider),
  FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);
