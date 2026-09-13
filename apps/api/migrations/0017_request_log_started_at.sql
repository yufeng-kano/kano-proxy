-- Preserve dispatch start independently of deferred log write time.
ALTER TABLE request_logs ADD COLUMN started_at TEXT;
