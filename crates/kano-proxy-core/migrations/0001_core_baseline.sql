-- Core baseline for the Rust/Postgres edition: the final shape of the D1 schema after
-- apps/api/migrations/0001..0017, expressed once (docs/rust-server.md § Storage).
-- Column names, ids (TEXT), ISO-8601 text timestamps and 0/1 integer flags are kept so a
-- D1 export imports row for row and the request/accounting semantics stay identical.

CREATE TABLE users (
  id TEXT PRIMARY KEY,
  google_sub TEXT NOT NULL UNIQUE,
  email TEXT NOT NULL,
  name TEXT,
  picture_url TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE sessions (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  expires_at TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX sessions_user_id_idx ON sessions(user_id);

CREATE TABLE api_keys (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  key_prefix TEXT NOT NULL,
  key_hash TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL,
  last_used_at TEXT,
  spend_limit DOUBLE PRECISION,
  spend_limit_interval TEXT NOT NULL DEFAULT 'monthly',
  spend_limit_include_oauth INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX api_keys_user_id_idx ON api_keys(user_id);

CREATE TABLE upstream_accounts (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  provider TEXT NOT NULL,
  external_account_id TEXT,
  label TEXT,
  priority INTEGER NOT NULL DEFAULT 0,
  encrypted_payload TEXT NOT NULL,
  account_meta_json TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  custom_label TEXT,
  usage_snapshot_json TEXT,
  usage_fetched_at TEXT,
  usage_fetching_at TEXT,
  bench_until TEXT,
  bench_reason TEXT,
  refreshing_at TEXT,
  edge_strikes INTEGER NOT NULL DEFAULT 0,
  edge_strike_at TEXT
);
CREATE INDEX upstream_accounts_user_provider_idx ON upstream_accounts(user_id, provider);

-- Deprecated since core 0006 (usage lives on upstream_accounts); kept so an export imports.
CREATE TABLE usage_snapshots (
  id TEXT PRIMARY KEY,
  account_id TEXT NOT NULL REFERENCES upstream_accounts(id) ON DELETE CASCADE,
  fetched_at TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  stale INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX usage_snapshots_account_id_idx ON usage_snapshots(account_id);

CREATE TABLE request_logs (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  api_key_id TEXT,
  provider TEXT NOT NULL,
  model TEXT NOT NULL,
  account_id TEXT,
  status_code INTEGER NOT NULL,
  latency_ms BIGINT NOT NULL,
  prompt_tokens BIGINT,
  completion_tokens BIGINT,
  error_code TEXT,
  created_at TEXT NOT NULL,
  cache_read_input_tokens BIGINT,
  cache_creation_input_tokens BIGINT,
  cost DOUBLE PRECISION,
  group_name TEXT,
  upstream_status INTEGER,
  api_key_name TEXT,
  account_label TEXT,
  started_at TEXT
);
CREATE INDEX request_logs_user_created_idx ON request_logs(user_id, created_at);
CREATE INDEX request_logs_api_key_created_idx ON request_logs(api_key_id, created_at);

CREATE TABLE oauth_login_states (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  user_id TEXT,
  provider TEXT,
  payload_json TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE custom_providers (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  slug TEXT NOT NULL,
  name TEXT NOT NULL,
  format TEXT NOT NULL,
  base_url TEXT NOT NULL,
  models_mode TEXT NOT NULL DEFAULT 'auto',
  manual_models_json TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  sort_order INTEGER NOT NULL DEFAULT 0,
  count_tokens_url TEXT,
  UNIQUE (user_id, slug)
);
CREATE INDEX custom_providers_user_id_idx ON custom_providers(user_id);

CREATE TABLE model_groups (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  slug TEXT NOT NULL,
  strategy TEXT NOT NULL DEFAULT 'ordered',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE (user_id, name),
  UNIQUE (user_id, slug)
);
CREATE INDEX model_groups_user_id_idx ON model_groups(user_id);

CREATE TABLE model_group_models (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  group_id TEXT NOT NULL REFERENCES model_groups(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  targets_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE (group_id, name)
);
CREATE INDEX model_group_models_group_id_idx ON model_group_models(group_id);

CREATE TABLE provider_settings (
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  provider TEXT NOT NULL,
  strategy TEXT NOT NULL DEFAULT 'ordered',
  updated_at TEXT NOT NULL,
  PRIMARY KEY (user_id, provider)
);

CREATE TABLE cli_devices (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  refresh_token_hash TEXT NOT NULL,
  refresh_token_prev_hash TEXT,
  last_seen_at TEXT,
  created_at TEXT NOT NULL,
  revoked_at TEXT
);
CREATE INDEX cli_devices_user_id_idx ON cli_devices(user_id);
CREATE INDEX cli_devices_refresh_hash_idx ON cli_devices(refresh_token_hash);
CREATE INDEX cli_devices_refresh_prev_hash_idx ON cli_devices(refresh_token_prev_hash);

CREATE TABLE cli_login_requests (
  id TEXT PRIMARY KEY,
  device_name TEXT NOT NULL,
  ip_hash TEXT,
  code_hash TEXT,
  user_id TEXT,
  expires_at TEXT NOT NULL,
  approved_at TEXT,
  used_at TEXT,
  attempts INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL
);
CREATE INDEX cli_login_requests_ip_created_idx ON cli_login_requests(ip_hash, created_at);

CREATE TABLE cli_providers (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  device_id TEXT,
  slug TEXT NOT NULL,
  name TEXT NOT NULL,
  format TEXT NOT NULL,
  models_json TEXT,
  models_updated_at TEXT,
  model_filter_json TEXT,
  sort_order INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE (user_id, slug)
);
CREATE INDEX cli_providers_user_id_idx ON cli_providers(user_id);
