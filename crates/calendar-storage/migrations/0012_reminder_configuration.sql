-- #92 is the unique owner. Preserve legacy seeds and every historical binding/delivery.
ALTER TABLE reminder_rules ADD COLUMN managed INTEGER NOT NULL DEFAULT 0 CHECK(managed IN (0,1));
ALTER TABLE reminder_rules ADD COLUMN display_name TEXT NOT NULL DEFAULT '';
ALTER TABLE reminder_rules ADD COLUMN retired_at INTEGER;
ALTER TABLE outputs ADD COLUMN managed INTEGER NOT NULL DEFAULT 0 CHECK(managed IN (0,1));
ALTER TABLE outputs ADD COLUMN retired_at INTEGER;
ALTER TABLE rule_outputs ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0,1));
CREATE TABLE rule_calendars (
    rule_id TEXT NOT NULL REFERENCES reminder_rules(id),
    calendar_id TEXT NOT NULL REFERENCES calendars(id),
    PRIMARY KEY(rule_id,calendar_id)
) STRICT;
CREATE INDEX rule_calendars_calendar ON rule_calendars(calendar_id,rule_id);
CREATE INDEX rule_outputs_output ON rule_outputs(output_id,enabled,rule_id);
CREATE TABLE reminder_configuration_audit (
    entity_kind TEXT NOT NULL CHECK(entity_kind IN ('rule','output')),
    entity_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK(generation>0),
    action TEXT NOT NULL CHECK(action IN ('create','update','retire','credential_changed')),
    actor_digest TEXT CHECK(actor_digest IS NULL OR length(actor_digest)=64),
    changed_at INTEGER NOT NULL,
    PRIMARY KEY(entity_kind,entity_id,generation)
) STRICT;
CREATE TRIGGER retired_rule_no_reactivation BEFORE UPDATE ON reminder_rules
WHEN OLD.retired_at IS NOT NULL AND (NEW.retired_at IS NOT OLD.retired_at OR NEW.enabled<>0)
BEGIN SELECT RAISE(ABORT,'rule retired'); END;
CREATE TRIGGER retired_output_no_reactivation BEFORE UPDATE ON outputs
WHEN OLD.retired_at IS NOT NULL AND (NEW.retired_at IS NOT OLD.retired_at OR NEW.enabled<>0)
BEGIN SELECT RAISE(ABORT,'output retired'); END;
CREATE TRIGGER rule_configuration_invalidate AFTER UPDATE OF generation,enabled ON reminder_rules
WHEN NEW.managed=1 AND (NEW.generation<>OLD.generation OR NEW.enabled<>OLD.enabled)
BEGIN
    UPDATE delivery_jobs SET state=CASE WHEN NEW.enabled=0 THEN 'cancelled' ELSE 'superseded' END
    WHERE state IN ('pending','retry_wait','leased') AND logical_delivery_id IN (
        SELECT id FROM logical_deliveries WHERE rule_id=NEW.id);
END;
CREATE TRIGGER output_configuration_invalidate AFTER UPDATE OF generation,enabled ON outputs
WHEN NEW.managed=1 AND (NEW.generation<>OLD.generation OR NEW.enabled<>OLD.enabled)
BEGIN
    UPDATE delivery_jobs SET state=CASE WHEN NEW.enabled=0 THEN 'cancelled' ELSE 'superseded' END
    WHERE state IN ('pending','retry_wait','leased') AND logical_delivery_id IN (
        SELECT id FROM logical_deliveries WHERE output_id=NEW.id);
END;
CREATE TRIGGER rule_binding_invalidate AFTER UPDATE OF enabled ON rule_outputs
WHEN NEW.enabled=0 AND OLD.enabled=1
BEGIN
    UPDATE delivery_jobs SET state='cancelled'
    WHERE state IN ('pending','retry_wait','leased') AND logical_delivery_id IN (
        SELECT id FROM logical_deliveries WHERE rule_id=NEW.rule_id AND output_id=NEW.output_id);
END;
-- Replacement and master-key re-encryption both invalidate previously prepared configs.
-- Runs inside the existing SecurityStore CAS transaction, never reads cleartext.
CREATE TRIGGER output_credential_configuration_revision AFTER UPDATE OF revision ON credentials
WHEN NEW.revision<>OLD.revision
BEGIN
    UPDATE outputs SET generation=generation+1
    WHERE managed=1 AND retired_at IS NULL AND credential_id=NEW.id;
    INSERT INTO reminder_configuration_audit(entity_kind,entity_id,generation,action,actor_digest,changed_at)
    SELECT 'output',id,generation,'credential_changed',NULL,NEW.rotated_at FROM outputs
    WHERE managed=1 AND retired_at IS NULL AND credential_id=NEW.id;
END;
