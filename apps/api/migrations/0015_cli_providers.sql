-- CLI providers (docs/cli.md): devices that ran `kano-proxy init`, pending
-- authorize-then-paste logins, and the local endpoints registered by
-- `kano-proxy add`. A CLI provider's pool state lives as one ordinary
-- upstream_accounts row with provider = slug (placeholder credential); the
-- local target URL and its optional API key never reach the server.

CREATE TABLE cli_devices (
  id TEXT PRIMARY KEY NOT NULL,
  user_id TEXT NOT NULL,
  name TEXT NOT NULL,
  refresh_token_hash TEXT NOT NULL,
  -- Immediately superseded token's hash: presenting a token that matches this
  -- column is reuse-as-theft and revokes the device (docs/cli.md § Device auth).
  refresh_token_prev_hash TEXT,
  last_seen_at TEXT,
  created_at TEXT NOT NULL,
  revoked_at TEXT,
  FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX cli_devices_user_id_idx ON cli_devices(user_id);
CREATE INDEX cli_devices_refresh_hash_idx ON cli_devices(refresh_token_hash);
CREATE INDEX cli_devices_refresh_prev_hash_idx ON cli_devices(refresh_token_prev_hash);

-- Unauthenticated until approve stamps the approving session's user_id onto
-- the row. Expired rows are purged by the daily retention sweep (docs/logging.md).
CREATE TABLE cli_login_requests (
  id TEXT PRIMARY KEY NOT NULL,
  device_name TEXT NOT NULL,
  -- SHA-256 of the requesting IP: the per-IP start budget is enforced as an
  -- atomic conditional INSERT counting recent rows with this hash — never the
  -- raw address (docs/cli.md § Security notes).
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
  id TEXT PRIMARY KEY NOT NULL,
  user_id TEXT NOT NULL,
  -- Informational "registered from" only — no FK, a deleted device leaves the
  -- provider intact.
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
  FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
  UNIQUE (user_id, slug)
);

CREATE INDEX cli_providers_user_id_idx ON cli_providers(user_id);
