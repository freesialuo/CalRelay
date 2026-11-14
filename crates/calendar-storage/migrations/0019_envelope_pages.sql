-- Unfinished multi-page standard-calendar-source/v1 collections (#194). Bounded: one open
-- collection per source, at most 32 pages, 10000 operations and 32 MiB, expiring after an hour.
-- Pages are deleted when the collection commits, is replaced or expires.
CREATE TABLE envelope_pages (
    source_id TEXT NOT NULL REFERENCES sources(id),
    collection_tag TEXT NOT NULL CHECK(length(collection_tag) BETWEEN 1 AND 256),
    page_no INTEGER NOT NULL CHECK(page_no BETWEEN 1 AND 32),
    pages INTEGER NOT NULL CHECK(pages BETWEEN 2 AND 32),
    token_id TEXT NOT NULL REFERENCES api_tokens(id),
    key_digest BLOB NOT NULL CHECK(length(key_digest)=32),
    request_digest BLOB NOT NULL CHECK(length(request_digest)=32),
    batch_id TEXT NOT NULL CHECK(length(batch_id) BETWEEN 1 AND 128),
    header TEXT NOT NULL,
    operations_json TEXT NOT NULL,
    operation_count INTEGER NOT NULL CHECK(operation_count >= 0),
    bytes INTEGER NOT NULL CHECK(bytes >= 0),
    expires_at INTEGER NOT NULL,
    PRIMARY KEY(source_id, collection_tag, page_no),
    UNIQUE(token_id, key_digest)
) STRICT;
CREATE INDEX envelope_page_expiry ON envelope_pages(expires_at);
