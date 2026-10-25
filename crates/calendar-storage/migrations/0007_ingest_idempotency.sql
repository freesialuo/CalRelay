-- No event bodies or bearer secrets. Keys are hashed by the repository.
CREATE TABLE ingest_requests (
    token_id TEXT NOT NULL REFERENCES api_tokens(id),
    key_digest BLOB NOT NULL CHECK (length(key_digest)=32),
    request_digest BLOB NOT NULL CHECK (length(request_digest)=32),
    event_revision INTEGER NOT NULL CHECK (event_revision>0),
    source_revision INTEGER NOT NULL CHECK (source_revision>0),
    deleted INTEGER NOT NULL CHECK (deleted IN (0,1)),
    created INTEGER NOT NULL CHECK (created IN (0,1)),
    expires_at INTEGER NOT NULL,
    request_id TEXT NOT NULL CHECK (length(request_id) BETWEEN 1 AND 128),
    PRIMARY KEY(token_id,key_digest)
) STRICT;
CREATE INDEX ingest_expiry ON ingest_requests(expires_at);
