-- Antigravity joined the builtin provider registry, and `antigravity` became a
-- reserved custom-provider slug. Reservation only blocks *new* creates: a
-- custom provider a user had already stored under that slug would silently
-- stop resolving, because `resolveCandidates` checks builtins before the
-- `custom_providers` lookup — its endpoint, its `upstream_accounts` keys and
-- any group targets addressing it would all become unreachable.
--
-- Rename those pre-existing rows (data migration, no schema change),
-- rewriting everything keyed by the slug in the same step so the renamed
-- provider keeps working end to end. Model ids change from
-- `antigravity/<model>` to `<new-slug>/<model>` — the release notes must call
-- that out to affected operators.
--
-- The target slug is `antigravity-custom`; a user who already owns it falls
-- through to `antigravity-custom-2`, then `antigravity-custom-3` (each pass
-- skips users who own that pass's target, so UNIQUE(user_id, slug) can never
-- reject the rename). Three static passes rather than a computed suffix
-- because the group-target rewrite is a string replace that must know the
-- exact replacement; a user owning all four slugs would have had to construct
-- the collision deliberately and keeps the shadowed row (documented residual).
--
-- Group targets store `"model": "antigravity/<id>"` inside targets_json.
-- Before this migration no builtin used the prefix, so every such target for
-- these users addressed the custom provider. The leading quote anchors the
-- match to the start of the JSON string value, so a mid-string
-- `…-antigravity/…` and the distinct `"antigravity-custom/` prefix are never
-- touched. `upstream_accounts` rows with provider='antigravity' can only be
-- the custom provider's keys — the builtin ships in this same release.

-- ── Pass 1: antigravity → antigravity-custom ────────────────────────────────

UPDATE model_groups
SET targets_json = replace(targets_json, '"antigravity/', '"antigravity-custom/')
WHERE user_id IN (
  SELECT user_id FROM custom_providers WHERE slug = 'antigravity'
    AND user_id NOT IN (SELECT user_id FROM custom_providers WHERE slug = 'antigravity-custom')
);

UPDATE provider_settings
SET provider = 'antigravity-custom'
WHERE provider = 'antigravity'
  AND user_id IN (
    SELECT user_id FROM custom_providers WHERE slug = 'antigravity'
      AND user_id NOT IN (SELECT user_id FROM custom_providers WHERE slug = 'antigravity-custom')
  );

UPDATE upstream_accounts
SET provider = 'antigravity-custom'
WHERE provider = 'antigravity'
  AND user_id IN (
    SELECT user_id FROM custom_providers WHERE slug = 'antigravity'
      AND user_id NOT IN (SELECT user_id FROM custom_providers WHERE slug = 'antigravity-custom')
  );

UPDATE custom_providers
SET slug = 'antigravity-custom'
WHERE slug = 'antigravity'
  AND user_id NOT IN (SELECT user_id FROM custom_providers WHERE slug = 'antigravity-custom');

-- ── Pass 2: leftovers → antigravity-custom-2 ────────────────────────────────

UPDATE model_groups
SET targets_json = replace(targets_json, '"antigravity/', '"antigravity-custom-2/')
WHERE user_id IN (
  SELECT user_id FROM custom_providers WHERE slug = 'antigravity'
    AND user_id NOT IN (SELECT user_id FROM custom_providers WHERE slug = 'antigravity-custom-2')
);

UPDATE provider_settings
SET provider = 'antigravity-custom-2'
WHERE provider = 'antigravity'
  AND user_id IN (
    SELECT user_id FROM custom_providers WHERE slug = 'antigravity'
      AND user_id NOT IN (SELECT user_id FROM custom_providers WHERE slug = 'antigravity-custom-2')
  );

UPDATE upstream_accounts
SET provider = 'antigravity-custom-2'
WHERE provider = 'antigravity'
  AND user_id IN (
    SELECT user_id FROM custom_providers WHERE slug = 'antigravity'
      AND user_id NOT IN (SELECT user_id FROM custom_providers WHERE slug = 'antigravity-custom-2')
  );

UPDATE custom_providers
SET slug = 'antigravity-custom-2'
WHERE slug = 'antigravity'
  AND user_id NOT IN (SELECT user_id FROM custom_providers WHERE slug = 'antigravity-custom-2');

-- ── Pass 3: leftovers → antigravity-custom-3 ────────────────────────────────

UPDATE model_groups
SET targets_json = replace(targets_json, '"antigravity/', '"antigravity-custom-3/')
WHERE user_id IN (
  SELECT user_id FROM custom_providers WHERE slug = 'antigravity'
    AND user_id NOT IN (SELECT user_id FROM custom_providers WHERE slug = 'antigravity-custom-3')
);

UPDATE provider_settings
SET provider = 'antigravity-custom-3'
WHERE provider = 'antigravity'
  AND user_id IN (
    SELECT user_id FROM custom_providers WHERE slug = 'antigravity'
      AND user_id NOT IN (SELECT user_id FROM custom_providers WHERE slug = 'antigravity-custom-3')
  );

UPDATE upstream_accounts
SET provider = 'antigravity-custom-3'
WHERE provider = 'antigravity'
  AND user_id IN (
    SELECT user_id FROM custom_providers WHERE slug = 'antigravity'
      AND user_id NOT IN (SELECT user_id FROM custom_providers WHERE slug = 'antigravity-custom-3')
  );

UPDATE custom_providers
SET slug = 'antigravity-custom-3'
WHERE slug = 'antigravity'
  AND user_id NOT IN (SELECT user_id FROM custom_providers WHERE slug = 'antigravity-custom-3');
