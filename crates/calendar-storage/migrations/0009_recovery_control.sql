-- Normal databases are unaffected; isolated recovery activates this durable policy.
CREATE TABLE recovery_control (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    paused INTEGER NOT NULL CHECK (paused IN (0, 1)),
    cutoff INTEGER
) STRICT;
INSERT INTO recovery_control VALUES (1, 0, NULL);

CREATE TRIGGER recovery_lease_guard BEFORE UPDATE ON delivery_jobs
WHEN NEW.state = 'leased' AND EXISTS (
    SELECT 1 FROM recovery_control WHERE paused = 1 OR NEW.due_at < cutoff
)
BEGIN SELECT RAISE(ABORT, 'recovery delivery guard'); END;
CREATE TRIGGER recovery_insert_lease_guard BEFORE INSERT ON delivery_jobs
WHEN NEW.state = 'leased' AND EXISTS (
    SELECT 1 FROM recovery_control WHERE paused = 1 OR NEW.due_at < cutoff
)
BEGIN SELECT RAISE(ABORT, 'recovery delivery guard'); END;

-- Replanning must not resurrect reminders predating the recovery cutoff.
CREATE TRIGGER recovery_insert_cutoff AFTER INSERT ON delivery_jobs
WHEN NEW.state IN ('pending', 'retry_wait') AND EXISTS (
    SELECT 1 FROM recovery_control WHERE NEW.due_at < cutoff
)
BEGIN UPDATE delivery_jobs SET state = 'expired' WHERE id = NEW.id; END;
CREATE TRIGGER recovery_update_cutoff AFTER UPDATE ON delivery_jobs
WHEN NEW.state IN ('pending', 'retry_wait') AND EXISTS (
    SELECT 1 FROM recovery_control WHERE NEW.due_at < cutoff
)
BEGIN UPDATE delivery_jobs SET state = 'expired' WHERE id = NEW.id; END;
