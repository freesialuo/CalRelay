CREATE TABLE admin_sessions (
    digest TEXT PRIMARY KEY NOT NULL,
    identity TEXT NOT NULL,
    policy TEXT NOT NULL,
    csrf_digest TEXT NOT NULL,
    nonce_digest TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL CHECK (expires_at > created_at),
    revoked_at INTEGER
);
CREATE TABLE admin_challenge_uses (
    digest TEXT PRIMARY KEY NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE INDEX admin_sessions_expiry ON admin_sessions(expires_at);
CREATE INDEX admin_challenge_expiry ON admin_challenge_uses(expires_at);
