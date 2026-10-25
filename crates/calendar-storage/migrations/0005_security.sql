-- Secrets never include master keys or raw machine tokens.
CREATE TABLE credentials (
    id TEXT PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 256),
    kind TEXT NOT NULL CHECK (length(kind) BETWEEN 1 AND 64),
    ciphertext BLOB NOT NULL CHECK (length(ciphertext) BETWEEN 17 AND 65552),
    nonce BLOB NOT NULL CHECK (length(nonce) = 24),
    key_version INTEGER NOT NULL CHECK (key_version > 0),
    revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
    rotated_at INTEGER NOT NULL
) STRICT;
ALTER TABLE sources ADD COLUMN credential_id TEXT REFERENCES credentials(id);
-- outputs.credential_id predates credentials. Enforce its reference without rebuilding
-- a table referenced by live planning/delivery history. Reject dangling legacy handles.
CREATE TABLE security_reference_check (missing INTEGER NOT NULL CHECK (missing = 0)) STRICT;
INSERT INTO security_reference_check SELECT count(*) FROM outputs WHERE credential_id IS NOT NULL;
DROP TABLE security_reference_check;
CREATE TRIGGER output_credential_insert BEFORE INSERT ON outputs
WHEN NEW.credential_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM credentials WHERE id=NEW.credential_id)
BEGIN SELECT RAISE(ABORT, 'unknown output credential'); END;
CREATE TRIGGER output_credential_update BEFORE UPDATE OF credential_id ON outputs
WHEN NEW.credential_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM credentials WHERE id=NEW.credential_id)
BEGIN SELECT RAISE(ABORT, 'unknown output credential'); END;
CREATE TRIGGER credential_delete_restrict BEFORE DELETE ON credentials
WHEN EXISTS (SELECT 1 FROM outputs WHERE credential_id=OLD.id)
BEGIN SELECT RAISE(ABORT, 'credential is referenced'); END;
CREATE TRIGGER credential_identity_immutable BEFORE UPDATE OF id,kind ON credentials
BEGIN SELECT RAISE(ABORT, 'credential identity is immutable'); END;

CREATE TABLE api_tokens (
    id TEXT PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 256),
    token_digest BLOB NOT NULL UNIQUE CHECK (length(token_digest) = 32),
    source_id TEXT NOT NULL REFERENCES sources(id),
    can_upsert INTEGER NOT NULL CHECK (can_upsert IN (0,1)),
    can_delete INTEGER NOT NULL CHECK (can_delete IN (0,1)),
    expires_at INTEGER,
    revoked_at INTEGER,
    CHECK (can_upsert = 1 OR can_delete = 1)
) STRICT;

-- Source-wide read-only subscriptions. Fine-grained visibility is delivered by #32.
CREATE TABLE feed_subscriptions (
    id TEXT PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 256),
    public_id TEXT NOT NULL UNIQUE CHECK (length(public_id) BETWEEN 1 AND 256),
    source_id TEXT NOT NULL REFERENCES sources(id),
    token_digest BLOB NOT NULL UNIQUE CHECK (length(token_digest) = 32),
    scope_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(scope_json)),
    revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
    expires_at INTEGER,
    revoked_at INTEGER
) STRICT;
