-- Keep user-defined account names separate from the upstream identity cache.
-- `label` is refreshed on every accounts read and can be overwritten by the
-- upstream; `custom_label` is user intent and must survive that sync.

ALTER TABLE upstream_accounts ADD COLUMN custom_label TEXT;
