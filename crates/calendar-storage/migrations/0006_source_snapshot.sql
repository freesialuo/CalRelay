-- Old checkpoints may come from deltas; they do not prove a complete cached snapshot.
-- Re-fetch once after upgrade before accepting conditional 304 for such sources.
ALTER TABLE sources ADD COLUMN snapshot_revision INTEGER
    CHECK (snapshot_revision IS NULL OR (snapshot_revision > 0 AND snapshot_revision <= revision));
