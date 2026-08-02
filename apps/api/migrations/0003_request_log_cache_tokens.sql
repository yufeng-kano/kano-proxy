-- Cache token columns on request_logs (normalized token capture — see
-- docs/database.md and docs/logging.md). Range queries for the
-- GET /api/usage/summary dashboard aggregation are covered by the
-- existing request_logs_user_created_idx from 0001_init.sql.

ALTER TABLE request_logs ADD COLUMN cache_read_input_tokens INTEGER;
ALTER TABLE request_logs ADD COLUMN cache_creation_input_tokens INTEGER;
