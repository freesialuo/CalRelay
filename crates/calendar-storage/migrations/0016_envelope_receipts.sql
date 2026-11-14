-- Idempotency receipts of standard-calendar-source/v1 batches, separate from the #30 namespace.
-- Digests and outcome counters only: no envelope, event body, bearer secret or raw key.
CREATE TABLE envelope_receipts (
    token_id TEXT NOT NULL REFERENCES api_tokens(id),
    key_digest BLOB NOT NULL CHECK (length(key_digest)=32),
    request_digest BLOB NOT NULL CHECK (length(request_digest)=32),
    batch_id TEXT NOT NULL CHECK (length(batch_id) BETWEEN 1 AND 128),
    source_revision INTEGER NOT NULL CHECK (source_revision>0),
    changed_events INTEGER NOT NULL CHECK (changed_events>=0),
    expires_at INTEGER NOT NULL,
    PRIMARY KEY(token_id,key_digest)
) STRICT;
CREATE INDEX envelope_receipt_expiry ON envelope_receipts(expires_at);
