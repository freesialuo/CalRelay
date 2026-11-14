-- One pending administrator approval per source for an over-limit snapshot deletion (#186).
-- It matches only the recorded configuration and source revisions, so any commit spends it.
CREATE TABLE deletion_approvals (
    source_id TEXT PRIMARY KEY REFERENCES sources(id),
    config_revision INTEGER NOT NULL CHECK(config_revision > 0),
    source_revision INTEGER NOT NULL CHECK(source_revision >= 0),
    approved_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
) STRICT;
CREATE TABLE deletion_approval_audit (
    source_id TEXT NOT NULL REFERENCES sources(id),
    config_revision INTEGER NOT NULL CHECK(config_revision > 0),
    source_revision INTEGER NOT NULL CHECK(source_revision >= 0),
    actor_digest TEXT NOT NULL CHECK(length(actor_digest)=64),
    approved_at INTEGER NOT NULL
) STRICT;
