//! One-time administrator approval of an over-limit snapshot deletion (#186). The approval is
//! bound to the source's configuration and source revisions at approval time; any commit
//! advances the source revision, so it can authorize at most the next commit.
use crate::Storage;
use calendar_application::{configuration::ConfigurationError as Error, IngestionError};
use sha2::{Digest, Sha256};

/// How long an unused approval stays valid.
pub const APPROVAL_TTL_MS: i64 = 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeletionApproval {
    pub config_revision: i64,
    pub source_revision: i64,
    pub expires_at: i64,
}

pub(crate) async fn approved(
    connection: &mut sqlx::SqliteConnection,
    source_id: &str,
    config_revision: i64,
    source_revision: i64,
    now: i64,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM deletion_approvals WHERE source_id=? AND config_revision=? AND source_revision=? AND expires_at>?)")
        .bind(source_id)
        .bind(config_revision)
        .bind(source_revision)
        .bind(now)
        .fetch_one(connection)
        .await
}

impl Storage {
    /// Approve the next commit of an enabled, unretired pull or push-only managed source.
    pub async fn approve_source_deletions(
        &self,
        source_id: &str,
        actor: &str,
        now: i64,
    ) -> Result<DeletionApproval, Error> {
        let expires_at = now.checked_add(APPROVAL_TTL_MS).ok_or(Error::Invalid)?;
        let mut tx = self
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|_| Error::Storage)?;
        let row: Option<(i64, i64, bool)> = sqlx::query_as("SELECT config_revision,revision,enabled=1 AND retired_at IS NULL AND json_extract(config_json,'$.enabled')=1 AND json_extract(config_json,'$.mode') IN ('pull','push-only') FROM sources WHERE id=? AND config_revision>0")
            .bind(source_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| Error::Storage)?;
        let (config_revision, source_revision, eligible) = row.ok_or(Error::NotFound)?;
        if !eligible {
            return Err(Error::Conflict);
        }
        sqlx::query("INSERT INTO deletion_approvals(source_id,config_revision,source_revision,approved_at,expires_at) VALUES(?,?,?,?,?) ON CONFLICT(source_id) DO UPDATE SET config_revision=excluded.config_revision,source_revision=excluded.source_revision,approved_at=excluded.approved_at,expires_at=excluded.expires_at")
            .bind(source_id).bind(config_revision).bind(source_revision).bind(now).bind(expires_at)
            .execute(&mut *tx).await.map_err(|_| Error::Storage)?;
        sqlx::query("INSERT INTO deletion_approval_audit(source_id,config_revision,source_revision,actor_digest,approved_at) VALUES(?,?,?,?,?)")
            .bind(source_id).bind(config_revision).bind(source_revision)
            .bind(format!("{:x}", Sha256::digest(actor.as_bytes()))).bind(now)
            .execute(&mut *tx).await.map_err(|_| Error::Storage)?;
        tx.commit().await.map_err(|_| Error::Storage)?;
        Ok(DeletionApproval {
            config_revision,
            source_revision,
            expires_at,
        })
    }
    /// Whether an unexpired approval matches exactly these revisions.
    pub async fn source_deletions_approved(
        &self,
        source_id: &str,
        config_revision: i64,
        source_revision: i64,
        now: i64,
    ) -> Result<bool, IngestionError> {
        let mut connection = self
            .pool()
            .acquire()
            .await
            .map_err(|_| IngestionError::Storage)?;
        approved(
            &mut connection,
            source_id,
            config_revision,
            source_revision,
            now,
        )
        .await
        .map_err(|_| IngestionError::Storage)
    }
}
