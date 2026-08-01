-- kano-proxy initial schema

CREATE TABLE users (
  id TEXT PRIMARY KEY NOT NULL,
  google_sub TEXT NOT NULL UNIQUE,
  email TEXT NOT NULL,
  name TEXT,
  picture_url TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE sessions (
  id TEXT PRIMARY KEY NOT NULL,
  user_id TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX sessions_user_id_idx ON sessions(user_id);

CREATE TABLE api_keys (
  id TEXT PRIMARY KEY NOT NULL,
  user_id TEXT NOT NULL,
  name TEXT NOT NULL,
  key_prefix TEXT NOT NULL,
  key_hash TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL,
  last_used_at TEXT,
  FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX api_keys_user_id_idx ON api_keys(user_id);

CREATE TABLE upstream_accounts (
  id TEXT PRIMARY KEY NOT NULL,
  user_id TEXT NOT NULL,
  provider TEXT NOT NULL,
  external_account_id TEXT,
  label TEXT,
  priority INTEGER NOT NULL DEFAULT 0,
  encrypted_payload TEXT NOT NULL,
  account_meta_json TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX upstream_accounts_user_provider_idx
  ON upstream_accounts(user_id, provider);

CREATE TABLE usage_snapshots (
  id TEXT PRIMARY KEY NOT NULL,
  account_id TEXT NOT NULL,
  fetched_at TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  stale INTEGER NOT NULL DEFAULT 0,
  FOREIGN KEY (account_id) REFERENCES upstream_accounts(id) ON DELETE CASCADE
);

CREATE INDEX usage_snapshots_account_id_idx ON usage_snapshots(account_id);

CREATE TABLE request_logs (
  id TEXT PRIMARY KEY NOT NULL,
  user_id TEXT NOT NULL,
  api_key_id TEXT,
  provider TEXT NOT NULL,
  model TEXT NOT NULL,
  account_id TEXT,
  status_code INTEGER NOT NULL,
  latency_ms INTEGER NOT NULL,
  prompt_tokens INTEGER,
  completion_tokens INTEGER,
  error_code TEXT,
  created_at TEXT NOT NULL
);

CREATE INDEX request_logs_user_created_idx ON request_logs(user_id, created_at);

CREATE TABLE oauth_login_states (
  id TEXT PRIMARY KEY NOT NULL,
  kind TEXT NOT NULL,
  user_id TEXT,
  provider TEXT,
  payload_json TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  created_at TEXT NOT NULL
);
