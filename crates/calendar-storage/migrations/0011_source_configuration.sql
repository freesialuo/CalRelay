-- Owned by #73. 0008 is reserved by the unmerged #70; do not renumber old migrations.
-- Legacy sources retain their exact config and remain unmanaged (revision 0).
ALTER TABLE sources ADD COLUMN config_revision INTEGER NOT NULL DEFAULT 0 CHECK(config_revision >= 0);
ALTER TABLE sources ADD COLUMN retired_at INTEGER;
ALTER TABLE calendars ADD COLUMN selected INTEGER NOT NULL DEFAULT 1 CHECK(selected IN (0,1));
CREATE TABLE configuration_audit (
    source_id TEXT NOT NULL REFERENCES sources(id),
    config_revision INTEGER NOT NULL CHECK(config_revision > 0),
    action TEXT NOT NULL CHECK(action IN ('create','update','retire')),
    actor_digest TEXT NOT NULL CHECK(length(actor_digest)=64),
    changed_at INTEGER NOT NULL,
    PRIMARY KEY(source_id,config_revision)
) STRICT;
CREATE TRIGGER retired_source_no_reactivation BEFORE UPDATE ON sources
WHEN OLD.retired_at IS NOT NULL AND (NEW.retired_at IS NOT OLD.retired_at OR NEW.enabled<>0)
BEGIN SELECT RAISE(ABORT,'source retired'); END;
