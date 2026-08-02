-- User-defined custom upstream providers (BYO endpoint + API key).
-- API keys for a custom provider live as rows in upstream_accounts with
-- provider = slug; this table only holds the provider-level config.

CREATE TABLE custom_providers (
  id TEXT PRIMARY KEY NOT NULL,
  user_id TEXT NOT NULL,
  slug TEXT NOT NULL,
  name TEXT NOT NULL,
  format TEXT NOT NULL,
  base_url TEXT NOT NULL,
  models_mode TEXT NOT NULL DEFAULT 'auto',
  manual_models_json TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
  UNIQUE (user_id, slug)
);

CREATE INDEX custom_providers_user_id_idx ON custom_providers(user_id);
