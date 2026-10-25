//! Atomic management persistence. No secrets/config bodies are copied to audit.
use crate::Storage;
use calendar_application::configuration::{
    valid_id, ConfigurationCondition as Condition, ConfigurationError as Error,
    SourceConfigurationRecord as Record, SourceConfigurationV1 as Configuration,
};
use sha2::{Digest, Sha256};

impl Storage {
    pub async fn source_configuration(&self, id: &str) -> Result<Record, Error> {
        let row: Option<(String, i64, i64, Option<i64>)> = sqlx::query_as(
            "SELECT config_json,config_revision,revision,retired_at FROM sources WHERE id=? AND config_revision>0")
            .bind(id).fetch_optional(self.pool()).await.map_err(|_| Error::Storage)?;
        let (json, config_revision, source_revision, retired_at) = row.ok_or(Error::NotFound)?;
        let mut configuration: Configuration =
            serde_json::from_str(&json).map_err(|_| Error::Storage)?;
        if retired_at.is_some() {
            configuration.enabled = false;
        }
        Ok(Record {
            source_id: id.into(),
            config_revision,
            source_revision,
            retired_at,
            configuration,
        })
    }
    /// Create disabled; enabling is a separate explicit validated CAS. Legacy configs are never
    /// guessed or overwritten. Existing calendar identities may be deselected but not remapped.
    pub async fn put_source_configuration(
        &self,
        id: &str,
        condition: Condition,
        config: Configuration,
        actor: &str,
        now: i64,
    ) -> Result<Record, Error> {
        config.validate()?;
        if !valid_id(id) || actor.is_empty() || now < 0 {
            return Err(Error::Invalid);
        }
        let json = serde_json::to_string(&config).map_err(|_| Error::Invalid)?;
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|_| Error::Storage)?;
        let old: Option<(i64, i64, Option<i64>)> =
            sqlx::query_as("SELECT config_revision,revision,retired_at FROM sources WHERE id=?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| Error::Storage)?;
        let (config_revision, source_revision, action) = match (condition, old) {
            (Condition::Absent, None) if !config.enabled => (1, 1, "create"),
            (Condition::Revision(expected), Some((actual, revision, None)))
                if actual > 0 && expected == actual =>
            {
                (
                    actual.checked_add(1).ok_or(Error::Conflict)?,
                    revision.checked_add(1).ok_or(Error::Conflict)?,
                    "update",
                )
            }
            _ => return Err(Error::Conflict),
        };
        let existing: Vec<(String, String)> =
            sqlx::query_as("SELECT id,external_id FROM calendars WHERE source_id=?")
                .bind(id)
                .fetch_all(&mut *tx)
                .await
                .map_err(|_| Error::Storage)?;
        if existing.iter().any(|(old_id, ext)| {
            !config
                .calendars
                .iter()
                .any(|c| &c.id == old_id && &c.external_id == ext)
        }) {
            return Err(Error::Conflict);
        }
        if let Some(credential) = &config.credential_id {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credentials WHERE id=?)")
                    .bind(credential)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|_| Error::Storage)?;
            if !exists {
                return Err(Error::Invalid);
            }
        }
        if action == "create" {
            sqlx::query("INSERT INTO sources(id,kind,config_json,revision,enabled,config_revision,credential_id) VALUES (?,'managed-v1',?,?,?,?,?)")
                .bind(id).bind(&json).bind(source_revision).bind(config.enabled).bind(config_revision).bind(&config.credential_id)
                .execute(&mut *tx).await.map_err(|_| Error::Storage)?;
        } else {
            sqlx::query("UPDATE sources SET config_json=?,config_revision=?,revision=?,enabled=?,credential_id=?,committed_checkpoint=NULL,snapshot_revision=NULL,last_verified_at=NULL WHERE id=?")
                .bind(&json).bind(config_revision).bind(source_revision).bind(config.enabled).bind(&config.credential_id).bind(id)
                .execute(&mut *tx).await.map_err(|_| Error::Storage)?;
        }
        for c in &config.calendars {
            let owner: Option<String> =
                sqlx::query_scalar("SELECT source_id FROM calendars WHERE id=?")
                    .bind(&c.id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|_| Error::Storage)?;
            if owner.as_deref().is_some_and(|owner| owner != id) {
                return Err(Error::Invalid);
            }
            let timezone = match &c.timezone {
                Some(t) => serde_json::json!({"tzid":t}),
                None => serde_json::json!({}),
            }
            .to_string();
            sqlx::query("INSERT INTO calendars(id,source_id,external_id,timezone_policy,selected) VALUES (?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET timezone_policy=excluded.timezone_policy,selected=excluded.selected")
                .bind(&c.id).bind(id).bind(&c.external_id).bind(timezone).bind(c.selected)
                .execute(&mut *tx).await.map_err(|_| Error::Storage)?;
        }
        invalidate(&mut tx, id, "superseded").await?;
        audit(&mut tx, id, config_revision, action, actor, now).await?;
        tx.commit().await.map_err(|_| Error::Storage)?;
        // Return the committed receipt, never race a post-commit reread against another writer.
        Ok(Record {
            source_id: id.into(),
            config_revision,
            source_revision,
            retired_at: None,
            configuration: config,
        })
    }
    pub async fn retire_source_configuration(
        &self,
        id: &str,
        revision: i64,
        actor: &str,
        now: i64,
    ) -> Result<(), Error> {
        if !valid_id(id) || revision < 1 || actor.is_empty() || now < 0 {
            return Err(Error::Invalid);
        }
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|_| Error::Storage)?;
        let next = revision.checked_add(1).ok_or(Error::Conflict)?;
        let changed=sqlx::query("UPDATE sources SET retired_at=?,enabled=0,config_revision=?,revision=revision+1 WHERE id=? AND config_revision=? AND retired_at IS NULL AND revision<9223372036854775807")
            .bind(now).bind(next).bind(id).bind(revision).execute(&mut *tx).await.map_err(|_|Error::Storage)?;
        if changed.rows_affected() != 1 {
            return Err(Error::Conflict);
        }
        sqlx::query("UPDATE api_tokens SET revoked_at=COALESCE(revoked_at,?) WHERE source_id=?")
            .bind(now)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|_| Error::Storage)?;
        sqlx::query("UPDATE feed_subscriptions SET revoked_at=COALESCE(revoked_at,?),revision=revision+1 WHERE source_id=?").bind(now).bind(id)
            .execute(&mut *tx).await.map_err(|_|Error::Storage)?;
        invalidate(&mut tx, id, "cancelled").await?;
        audit(&mut tx, id, next, "retire", actor, now).await?;
        tx.commit().await.map_err(|_| Error::Storage)?;
        Ok(())
    }
}
async fn invalidate(tx: &mut sqlx::SqliteConnection, id: &str, state: &str) -> Result<(), Error> {
    // Keep leases/attempts: an already sent request may still return accepted; #24 finish
    // records that outcome and the cancellation race, retaining logical deduplication.
    sqlx::query("UPDATE delivery_jobs SET state=? WHERE state IN ('pending','retry_wait','leased') AND logical_delivery_id IN (SELECT l.id FROM logical_deliveries l JOIN events e ON e.id=l.event_id WHERE e.source_id=?)")
        .bind(state).bind(id).execute(&mut *tx).await.map_err(|_|Error::Storage)?;
    Ok(())
}
async fn audit(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
    revision: i64,
    action: &str,
    actor: &str,
    now: i64,
) -> Result<(), Error> {
    sqlx::query("INSERT INTO configuration_audit(source_id,config_revision,action,actor_digest,changed_at) VALUES(?,?,?,?,?)")
        .bind(id).bind(revision).bind(action).bind(format!("{:x}",Sha256::digest(actor.as_bytes()))).bind(now)
        .execute(tx).await.map_err(|_|Error::Storage)?;
    Ok(())
}
