-- Persisted display order for user-defined custom providers.
-- Existing rows keep their created_at order, numbered densely per user.

ALTER TABLE custom_providers ADD COLUMN sort_order INTEGER NOT NULL DEFAULT 0;

WITH ranked AS (
  SELECT
    id,
    ROW_NUMBER() OVER (PARTITION BY user_id ORDER BY created_at ASC, id ASC) - 1 AS position
  FROM custom_providers
)
UPDATE custom_providers
SET sort_order = (
  SELECT position
  FROM ranked
  WHERE ranked.id = custom_providers.id
);
