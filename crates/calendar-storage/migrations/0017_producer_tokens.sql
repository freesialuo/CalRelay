-- Producer token management (#178). Issue time is NULL for tokens created before this migration.
ALTER TABLE api_tokens ADD COLUMN issued_at INTEGER;
CREATE INDEX api_tokens_source ON api_tokens(source_id);
-- Who issued or revoked which token; never the secret or its digest.
CREATE TABLE producer_token_audit (
    token_id TEXT NOT NULL REFERENCES api_tokens(id),
    source_id TEXT NOT NULL REFERENCES sources(id),
    action TEXT NOT NULL CHECK(action IN ('issue','revoke')),
    actor_digest TEXT NOT NULL CHECK(length(actor_digest)=64),
    changed_at INTEGER NOT NULL
) STRICT;
