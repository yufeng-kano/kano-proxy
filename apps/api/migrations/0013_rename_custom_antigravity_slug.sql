-- Antigravity joined the builtin provider registry, and `antigravity` became a
-- reserved custom-provider slug. Reservation only blocks *new* creates: a
-- custom provider a user had already stored under that slug would silently
-- stop resolving, because `resolveCandidates` checks builtins before the
-- `custom_providers` lookup — its endpoint, its `upstream_accounts` keys and
-- any group targets addressing it would all become unreachable.
--
-- Rename those pre-existing rows to `antigravity-custom` (data migration, no
-- schema change), rewriting everything keyed by the slug in the same step so
-- the renamed provider keeps working end to end. Model ids change from
-- `antigravity/<model>` to `antigravity-custom/<model>` — the release notes
-- must call that out to affected operators.
--
-- Scope guard: skip a user who somehow already owns an `antigravity-custom`
-- slug (UNIQUE(user_id, slug) would reject the rename). For such a user the
-- old provider stays shadowed by the builtin — the documented residual edge —
-- rather than the migration failing for everyone.

-- Group targets store `"model": "antigravity/<id>"` inside targets_json.
-- Before this migration no builtin used the prefix, so for these users every
-- such target addressed the custom provider. The leading quote anchors the
-- match to the start of the JSON string value.
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

-- A custom provider's API keys are ordinary upstream_accounts rows with
-- provider = slug; builtin antigravity accounts cannot exist yet (the builtin
-- ships in the same release as this migration), so every matching row here
-- belongs to the custom provider being renamed.
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
