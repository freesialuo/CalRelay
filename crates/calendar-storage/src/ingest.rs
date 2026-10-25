//! One write transaction linearizes revocation, conditional event changes and retry receipts.
use crate::{security::digest, Storage};
use calendar_application::{
    IngestCondition, IngestOperation, IngestPolicy, IngestRequest, IngestResult,
};
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("authorization denied")]
    Denied,
    #[error("invalid event request")]
    Invalid,
    #[error("write or idempotency conflict")]
    Conflict,
    #[error("idempotency storage at capacity")]
    Capacity,
    #[error("ingest persistence failed")]
    Database,
}
impl From<sqlx::Error> for IngestError {
    fn from(_: sqlx::Error) -> Self {
        Self::Database
    }
}
impl Storage {
    pub async fn ingest(
        &self,
        secret: &[u8],
        request: IngestRequest,
        policy: IngestPolicy,
        clock: &impl calendar_application::Clock,
        request_id: &str,
    ) -> Result<IngestResult, IngestError> {
        request.validate().map_err(|_| IngestError::Invalid)?;
        if !policy.valid() || request_id.is_empty() || request_id.len() > 128 {
            return Err(IngestError::Invalid);
        }
        let token_hash = digest("api", secret).map_err(|_| IngestError::Denied)?;
        let (calendar, document, sequence) = match &request.operation {
            IngestOperation::Upsert { calendar_id, event } => (
                Some(calendar_id.as_str()),
                Some(serde_json::to_string(event).map_err(|_| IngestError::Invalid)?),
                event.source_sequence,
            ),
            IngestOperation::Delete => (None, None, None),
        };
        let deleting = matches!(request.operation, IngestOperation::Delete);
        let condition = match request.condition {
            IngestCondition::Absent => 0,
            IngestCondition::Revision(n) => n,
        };
        let fingerprint = serde_json::to_vec(&(
            "CalRelay/ingest/v1",
            &request.source_id,
            &request.uid,
            deleting,
            condition,
            calendar,
            &document,
        ))
        .map_err(|_| IngestError::Invalid)?;
        let hash = Sha256::digest(fingerprint).to_vec();
        let key = Sha256::digest(request.key.as_bytes()).to_vec();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        // Sample AFTER acquiring the write lock: an expired token must not survive lock contention.
        let now = clock.utc_now_ms();
        let expires_at = now
            .checked_add(policy.retention_ms)
            .ok_or(IngestError::Invalid)?;
        let token: Option<(String,i64)> = sqlx::query_as("SELECT t.id,s.revision FROM api_tokens t JOIN sources s ON s.id=t.source_id WHERE t.token_digest=? AND t.source_id=? AND t.revoked_at IS NULL AND (t.expires_at IS NULL OR t.expires_at>?) AND s.enabled=1 AND ((?=0 AND t.can_upsert=1) OR (?=1 AND t.can_delete=1))")
            .bind(token_hash).bind(&request.source_id).bind(now).bind(deleting).bind(deleting).fetch_optional(&mut *tx).await?;
        let (token_id, source_revision) = token.ok_or(IngestError::Denied)?;
        sqlx::query("DELETE FROM ingest_requests WHERE expires_at<=?")
            .bind(now)
            .execute(&mut *tx)
            .await?;
        type Receipt = (Vec<u8>, i64, i64, bool, bool, i64, String);
        let previous: Option<Receipt> = sqlx::query_as("SELECT request_digest,event_revision,source_revision,deleted,created,expires_at,request_id FROM ingest_requests WHERE token_id=? AND key_digest=?")
            .bind(&token_id).bind(&key).fetch_optional(&mut *tx).await?;
        if let Some((
            old,
            event_revision,
            source_revision,
            deleted,
            created,
            expires_at,
            request_id,
        )) = previous
        {
            if old != hash {
                return Err(IngestError::Conflict);
            }
            tx.commit().await?;
            return Ok(IngestResult {
                event_revision,
                source_revision,
                deleted,
                created,
                replayed: true,
                expires_at,
                request_id,
            });
        }
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM ingest_requests")
            .fetch_one(&mut *tx)
            .await?;
        if count >= policy.max_records {
            return Err(IngestError::Capacity);
        }
        type Existing = (String, i64, Option<i64>, String, String, String);
        let old:Option<Existing> = sqlx::query_as("SELECT id,revision,deleted_at,document_json,calendar_id,content_hash FROM events WHERE source_id=? AND external_uid=?")
            .bind(&request.source_id).bind(&request.uid).fetch_optional(&mut *tx).await?;
        match (request.condition, &old) {
            (IngestCondition::Absent, None) => {}
            (IngestCondition::Revision(n), Some((_, revision, _, _, _, _))) if n == *revision => {}
            _ => return Err(IngestError::Conflict),
        }
        if let Some(calendar) = calendar {
            let owned: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM calendars WHERE id=? AND source_id=?)",
            )
            .bind(calendar)
            .bind(&request.source_id)
            .fetch_one(&mut *tx)
            .await?;
            if !owned {
                return Err(IngestError::Invalid);
            }
        }
        if let Some((_, _, _, old_document, _, _)) = &old {
            let event: calendar_application::EventV1 =
                serde_json::from_str(old_document).map_err(|_| IngestError::Database)?;
            if matches!((sequence,event.source_sequence),(Some(new),Some(old)) if new<old) {
                return Err(IngestError::Conflict);
            }
        }
        let created = old.is_none();
        let content_hash = document
            .as_ref()
            .map(|s| format!("{:x}", Sha256::digest(s.as_bytes())));
        let unchanged = match &old {
            Some((_, _, deleted, _, old_calendar, old_hash)) => {
                if deleting {
                    deleted.is_some()
                } else {
                    deleted.is_none()
                        && Some(old_calendar.as_str()) == calendar
                        && Some(old_hash) == content_hash.as_ref()
                }
            }
            None => false,
        };
        let mut event_revision = old.as_ref().map(|o| o.1).unwrap_or(0);
        let mut committed_source_revision = source_revision;
        if !unchanged {
            event_revision = event_revision.checked_add(1).ok_or(IngestError::Conflict)?;
            committed_source_revision = source_revision
                .checked_add(1)
                .ok_or(IngestError::Conflict)?;
            let id = if let Some((id, _, _, _, _, _)) = &old {
                sqlx::query("UPDATE events SET revision=?,calendar_id=COALESCE(?,calendar_id),document_json=COALESCE(?,document_json),content_hash=COALESCE(?,content_hash),deleted_at=? WHERE id=?")
                    .bind(event_revision).bind(calendar).bind(&document).bind(&content_hash).bind(if deleting {Some(now)} else {None}).bind(id).execute(&mut *tx).await?;
                id.clone()
            } else {
                sqlx::query_scalar::<_,String>("INSERT INTO events(id,source_id,calendar_id,external_uid,revision,document_json,content_hash) VALUES(lower(hex(randomblob(16))),?,?,?,?,?,?) RETURNING id")
                    .bind(&request.source_id).bind(calendar).bind(&request.uid).bind(event_revision).bind(&document).bind(&content_hash).fetch_one(&mut *tx).await?
            };
            let kind = if deleting { "delete" } else { "upsert" };
            sqlx::query("INSERT INTO event_revisions(event_id,revision,document_json,change_kind,changed_at) VALUES(?,?,?,?,?)")
                .bind(&id).bind(event_revision).bind(&document).bind(kind).bind(now).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO source_changes(source_id,source_revision,event_id,event_revision,change_kind) VALUES(?,?,?,?,?)")
                .bind(&request.source_id).bind(committed_source_revision).bind(&id).bind(event_revision).bind(kind).execute(&mut *tx).await?;
            // A machine write must not erase the remote adapter's checkpoint or snapshot proof.
            sqlx::query("UPDATE sources SET revision=?,last_verified_at=? WHERE id=?")
                .bind(committed_source_revision)
                .bind(now)
                .bind(&request.source_id)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("INSERT INTO ingest_requests(token_id,key_digest,request_digest,event_revision,source_revision,deleted,created,expires_at,request_id) VALUES(?,?,?,?,?,?,?,?,?)")
            .bind(token_id).bind(key).bind(hash).bind(event_revision).bind(committed_source_revision).bind(deleting).bind(created).bind(expires_at).bind(request_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(IngestResult {
            event_revision,
            source_revision: committed_source_revision,
            deleted: deleting,
            created,
            replayed: false,
            expires_at,
            request_id: request_id.into(),
        })
    }
}
