-- Configuration changes must invalidate feed representations, including SQL callers.
-- Rotation already increments revision. Do not count an explicit CAS increment twice.
CREATE TRIGGER feed_configuration_revision
AFTER UPDATE OF public_id,source_id,scope_json,expires_at ON feed_subscriptions
WHEN NEW.revision=OLD.revision AND (
    NEW.public_id IS NOT OLD.public_id OR NEW.source_id IS NOT OLD.source_id OR
    NEW.scope_json IS NOT OLD.scope_json OR NEW.expires_at IS NOT OLD.expires_at
)
BEGIN
    UPDATE feed_subscriptions SET revision=revision+1 WHERE id=NEW.id;
END;
