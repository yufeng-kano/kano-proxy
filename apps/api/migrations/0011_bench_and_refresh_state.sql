ALTER TABLE upstream_accounts ADD COLUMN bench_until TEXT;
ALTER TABLE upstream_accounts ADD COLUMN bench_reason TEXT;
ALTER TABLE upstream_accounts ADD COLUMN refreshing_at TEXT;
ALTER TABLE upstream_accounts ADD COLUMN edge_strikes INTEGER NOT NULL DEFAULT 0;
ALTER TABLE upstream_accounts ADD COLUMN edge_strike_at TEXT;
ALTER TABLE request_logs ADD COLUMN upstream_status INTEGER;
