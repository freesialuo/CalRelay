//! Standard-calendar-source/v1 snapshot batches. Authorization, source state, receipt and commit
//! are one write transaction, so revocation, disabling or a concurrent edit cannot race a batch.
use crate::{
    security::digest,
    sync::{apply, prepare, SyncError},
    Storage,
};
use calendar_application::{Clock, DeletionPolicy, IngestPolicy, SyncBatch, SyncMode, SyncRecord};
use sha2::{Digest, Sha256};

/// More deletions than this, or emptying a non-empty source, needs separate administrator approval.
pub const SNAPSHOT_DELETION_LIMIT: usize = 100;

macro_rules! accepting_push {
    () => {
        "s.config_revision>0 AND s.enabled=1 AND s.retired_at IS NULL AND json_extract(s.config_json,'$.enabled')=1 AND json_extract(s.config_json,'$.mode')='push-only' AND json_extract(s.config_json,'$.adapter')='standard-calendar' AND json_extract(s.config_json,'$.capabilities.snapshot')=1"
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EnvelopeError {
    #[error("authorization denied")]
    Denied,
    #[error("invalid batch")]
    Invalid,
    #[error("source is not an enabled push-only snapshot source")]
    NotAccepting,
    #[error("base revision or idempotency conflict")]
    Conflict,
    #[error("snapshot deletions need administrator approval")]
    ReviewRequired,
    #[error("receipt storage at capacity")]
    Capacity,
    #[error("envelope persistence failed")]
    Database,
}
impl From<sqlx::Error> for EnvelopeError {
    fn from(_: sqlx::Error) -> Self {
        Self::Database
    }
}

/// One validated `mode=snapshot` envelope. `digest` is the server-computed SHA-256 of its JCS form.
#[derive(Debug, Clone)]
pub struct SnapshotBatch {
    pub source_id: String,
    pub key: String,
    pub digest: [u8; 32],
    pub batch_id: String,
    pub base_config_revision: i64,
    pub base_source_revision: i64,
    pub proposed_checkpoint: Option<String>,
    pub records: Vec<SyncRecord>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchReceipt {
    pub batch_id: String,
    pub source_revision: i64,
    pub changed_events: i64,
    pub replayed: bool,
}
/// What a producer needs to fill `base_*`; never event content or configuration detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushSourceState {
    pub config_revision: i64,
    pub source_revision: i64,
    pub checkpoint: Option<String>,
    pub has_snapshot: bool,
}

type SourceRow = (i64, i64, Option<String>, Option<i64>, bool);

impl Storage {
    pub async fn push_source_state(
        &self,
        secret: &[u8],
        source: &str,
        clock: &impl Clock,
    ) -> Result<PushSourceState, EnvelopeError> {
        let token_hash = digest("api", secret).map_err(|_| EnvelopeError::Denied)?;
        let mut tx = self.pool.begin().await?;
        let now = clock.utc_now_ms();
        let row: Option<SourceRow> = sqlx::query_as(concat!(
            "SELECT s.config_revision,s.revision,s.committed_checkpoint,s.snapshot_revision,",
            accepting_push!(),
            " FROM api_tokens t JOIN sources s ON s.id=t.source_id WHERE t.token_digest=? ",
            "AND t.source_id=? AND t.revoked_at IS NULL AND (t.expires_at IS NULL OR t.expires_at>?)"
        ))
        .bind(token_hash)
        .bind(source)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        let (config_revision, source_revision, checkpoint, snapshot, accepting) =
            row.ok_or(EnvelopeError::Denied)?;
        if !accepting {
            return Err(EnvelopeError::NotAccepting);
        }
        Ok(PushSourceState {
            config_revision,
            source_revision,
            checkpoint,
            has_snapshot: snapshot.is_some(),
        })
    }

    pub async fn ingest_snapshot_batch(
        &self,
        secret: &[u8],
        batch: SnapshotBatch,
        policy: IngestPolicy,
        clock: &impl Clock,
    ) -> Result<BatchReceipt, EnvelopeError> {
        if !policy.valid()
            || batch.key.is_empty()
            || batch.key.len() > 128
            || !batch.key.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(EnvelopeError::Invalid);
        }
        let token_hash = digest("api", secret).map_err(|_| EnvelopeError::Denied)?;
        let key = Sha256::digest(batch.key.as_bytes()).to_vec();
        let mut sync = SyncBatch {
            source_id: batch.source_id.clone(),
            base_revision: batch.base_source_revision,
            mode: SyncMode::FullSnapshot {
                complete: true,
                deletion_policy: DeletionPolicy::Limit(SNAPSHOT_DELETION_LIMIT),
            },
            records: batch.records,
            proposed_checkpoint: batch.proposed_checkpoint,
            verified_at: 0,
        };
        // Validate, serialize and hash before taking the write lock.
        let prepared = prepare(&mut sync).map_err(|_| EnvelopeError::Invalid)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        // Sampled after the lock: an expired token must not survive lock contention.
        let now = clock.utc_now_ms();
        let expires_at = now
            .checked_add(policy.retention_ms)
            .ok_or(EnvelopeError::Invalid)?;
        // A snapshot deletes absent events, so it needs both write permissions.
        let token: Option<(String, i64, i64, bool)> = sqlx::query_as(concat!(
            "SELECT t.id,s.config_revision,s.revision,",
            accepting_push!(),
            " FROM api_tokens t JOIN sources s ON s.id=t.source_id WHERE t.token_digest=? ",
            "AND t.source_id=? AND t.revoked_at IS NULL AND (t.expires_at IS NULL OR t.expires_at>?) ",
            "AND t.can_upsert=1 AND t.can_delete=1"
        ))
        .bind(token_hash)
        .bind(&batch.source_id)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;
        let (token_id, config_revision, source_revision, accepting) =
            token.ok_or(EnvelopeError::Denied)?;
        // Checked before any receipt: a disabled or retired source cannot replay one.
        if !accepting {
            return Err(EnvelopeError::NotAccepting);
        }
        sqlx::query("DELETE FROM envelope_receipts WHERE expires_at<=?")
            .bind(now)
            .execute(&mut *tx)
            .await?;
        let previous: Option<(Vec<u8>, String, i64, i64)> = sqlx::query_as(
            "SELECT request_digest,batch_id,source_revision,changed_events FROM envelope_receipts WHERE token_id=? AND key_digest=?",
        )
        .bind(&token_id)
        .bind(&key)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((old, batch_id, source_revision, changed_events)) = previous {
            if old != batch.digest {
                return Err(EnvelopeError::Conflict);
            }
            tx.commit().await?;
            return Ok(BatchReceipt {
                batch_id,
                source_revision,
                changed_events,
                replayed: true,
            });
        }
        let live: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM envelope_receipts)+(SELECT count(*) FROM ingest_requests WHERE expires_at>?)",
        )
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
        if live >= policy.max_records {
            return Err(EnvelopeError::Capacity);
        }
        if config_revision != batch.base_config_revision || source_revision != sync.base_revision {
            return Err(EnvelopeError::Conflict);
        }
        if prepared.is_empty() {
            let current: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM events WHERE source_id=? AND deleted_at IS NULL",
            )
            .bind(&batch.source_id)
            .fetch_one(&mut *tx)
            .await?;
            if current > 0 {
                return Err(EnvelopeError::ReviewRequired);
            }
        }
        sync.verified_at = now;
        let committed = apply(&mut tx, sync, prepared)
            .await
            .map_err(|error| match error {
                SyncError::ReviewRequired { .. } => EnvelopeError::ReviewRequired,
                SyncError::Conflict | SyncError::SourceUnavailable => EnvelopeError::Conflict,
                SyncError::Invalid(_) | SyncError::CalendarMismatch | SyncError::StaleSequence => {
                    EnvelopeError::Invalid
                }
                _ => EnvelopeError::Database,
            })?;
        let changed_events =
            i64::try_from(committed.changed_events).map_err(|_| EnvelopeError::Database)?;
        sqlx::query("INSERT INTO envelope_receipts(token_id,key_digest,request_digest,batch_id,source_revision,changed_events,expires_at) VALUES(?,?,?,?,?,?,?)")
            .bind(token_id).bind(key).bind(batch.digest.as_slice()).bind(&batch.batch_id)
            .bind(committed.source_revision).bind(changed_events).bind(expires_at)
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(BatchReceipt {
            batch_id: batch.batch_id,
            source_revision: committed.source_revision,
            changed_events,
            replayed: false,
        })
    }
}
