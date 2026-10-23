-- Preserve existing jobs and accepted history. Attempts include durable send intent.
CREATE TABLE delivery_attempts (
    id TEXT PRIMARY KEY CHECK (length(id) > 0),
    job_id TEXT NOT NULL REFERENCES delivery_jobs(id),
    attempt_no INTEGER NOT NULL CHECK (attempt_no >= 1),
    lease_owner TEXT NOT NULL CHECK (length(lease_owner) > 0),
    lease_token TEXT NOT NULL UNIQUE CHECK (length(lease_token) > 0),
    request_id TEXT NOT NULL UNIQUE CHECK (length(request_id) > 0),
    started_at INTEGER NOT NULL,
    sent_at INTEGER,
    finished_at INTEGER,
    outcome TEXT CHECK (outcome IN ('accepted','permanent_failure','retryable','unknown','not_sent')),
    disposition TEXT CHECK (disposition IN ('accepted','retry_wait','failed_permanent','expired','superseded','cancelled')),
    retry_after_ms INTEGER CHECK (retry_after_ms IS NULL OR retry_after_ms >= 0),
    UNIQUE(job_id, attempt_no),
    CHECK ((finished_at IS NULL AND outcome IS NULL AND disposition IS NULL)
        OR (finished_at IS NOT NULL AND outcome IS NOT NULL AND disposition IS NOT NULL))
) STRICT;
CREATE INDEX delivery_attempts_job ON delivery_attempts(job_id, attempt_no);
