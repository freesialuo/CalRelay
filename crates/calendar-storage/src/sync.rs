use crate::Storage;
use std::collections::HashSet;

use calendar_application::{
    BatchError, CommittedSourceRevision, DeletionPolicy, EventRepository, EventV1, SyncBatch,
    SyncMode, SyncRecord,
};
use sha2::{Digest, Sha256};
use sqlx::SqliteConnection;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Invalid(#[from] BatchError),
    #[error("source revision changed")]
    Conflict,
    #[error("source is missing or disabled")]
    SourceUnavailable,
    #[error("calendar does not belong to this source")]
    CalendarMismatch,
    #[error("source sequence decreased")]
    StaleSequence,
    #[error("revision exhausted")]
    RevisionOverflow,
    #[error("snapshot needs approval for {deletions} deletions")]
    ReviewRequired { deletions: usize },
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
}

impl EventRepository for Storage {
    type Error = SyncError;
    async fn commit_sync(&self, batch: SyncBatch) -> Result<CommittedSourceRevision, SyncError> {
        batch.validate()?;
        let revision = batch
            .base_revision
            .checked_add(1)
            .ok_or(SyncError::RevisionOverflow)?;
        // Validate, serialize and hash outside the write transaction.
        let mut prepared = Vec::with_capacity(batch.records.len());
        for record in batch.records {
            prepared.push(match record {
                SyncRecord::Upsert { calendar_id, event } => {
                    let document = serde_json::to_string(&event)?;
                    let hash = format!("{:x}", Sha256::digest(document.as_bytes()));
                    Prepared::Upsert {
                        calendar_id,
                        uid: event.external_uid,
                        sequence: event.source_sequence,
                        document,
                        hash,
                    }
                }
                SyncRecord::Delete { external_uid } => Prepared::Delete(external_uid),
            });
        }
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let source: Option<(i64, bool)> =
            sqlx::query_as("SELECT revision,enabled FROM sources WHERE id=?")
                .bind(&batch.source_id)
                .fetch_optional(&mut *tx)
                .await?;
        match source {
            None | Some((_, false)) => return Err(SyncError::SourceUnavailable),
            Some((actual, _)) if actual != batch.base_revision => return Err(SyncError::Conflict),
            _ => {}
        }
        let journal = Journal {
            source_id: &batch.source_id,
            revision,
            at: batch.verified_at,
        };
        let mut missing = Vec::new();
        if let SyncMode::FullSnapshot {
            deletion_policy, ..
        } = batch.mode
        {
            let present: HashSet<&str> = prepared.iter().map(Prepared::uid).collect();
            let current: Vec<(String,)> = sqlx::query_as(
                "SELECT external_uid FROM events WHERE source_id=? AND deleted_at IS NULL",
            )
            .bind(&batch.source_id)
            .fetch_all(&mut *tx)
            .await?;
            missing = current
                .into_iter()
                .map(|(uid,)| uid)
                .filter(|uid| !present.contains(uid.as_str()))
                .collect();
            if let DeletionPolicy::Limit(limit) = deletion_policy {
                if missing.len() > limit {
                    return Err(SyncError::ReviewRequired {
                        deletions: missing.len(),
                    });
                }
            }
        }
        let mut changed_events = 0;
        for record in prepared {
            changed_events += match record {
                Prepared::Upsert {
                    calendar_id,
                    uid,
                    sequence,
                    document,
                    hash,
                } => {
                    let owned: bool = sqlx::query_scalar(
                        "SELECT EXISTS(SELECT 1 FROM calendars WHERE id=? AND source_id=?)",
                    )
                    .bind(&calendar_id)
                    .bind(&batch.source_id)
                    .fetch_one(&mut *tx)
                    .await?;
                    if !owned {
                        return Err(SyncError::CalendarMismatch);
                    }
                    let previous = existing(&mut tx, &batch.source_id, &uid).await?;
                    if let Some(old) = &previous {
                        let event: EventV1 = serde_json::from_str(&old.document_json)?;
                        if matches!((sequence,event.source_sequence),(Some(new),Some(old)) if new<old)
                        {
                            return Err(SyncError::StaleSequence);
                        }
                        if old.deleted_at.is_none()
                            && old.calendar_id == calendar_id
                            && old.content_hash == hash
                        {
                            continue;
                        }
                    }
                    let (id, event_revision) = match previous {
                        Some(old) => {
                            let next = old
                                .revision
                                .checked_add(1)
                                .ok_or(SyncError::RevisionOverflow)?;
                            sqlx::query("UPDATE events SET calendar_id=?,revision=?,document_json=?,content_hash=?,deleted_at=NULL WHERE id=?")
                                .bind(&calendar_id).bind(next).bind(&document).bind(&hash).bind(&old.id).execute(&mut *tx).await?;
                            (old.id, next)
                        }
                        None => {
                            let id:String = sqlx::query_scalar("INSERT INTO events(id,source_id,calendar_id,external_uid,revision,document_json,content_hash) VALUES (lower(hex(randomblob(16))),?,?,?,1,?,?) RETURNING id")
                                .bind(&batch.source_id).bind(&calendar_id).bind(&uid).bind(&document).bind(&hash).fetch_one(&mut *tx).await?;
                            (id, 1)
                        }
                    };
                    journal
                        .write(&mut tx, &id, event_revision, Some(&document), "upsert")
                        .await?;
                    1
                }
                Prepared::Delete(uid) => delete(&mut tx, &journal, &uid).await?,
            };
        }
        for uid in missing {
            changed_events += delete(&mut tx, &journal, &uid).await?;
        }
        let result = sqlx::query("UPDATE sources SET revision=?,committed_checkpoint=?,last_verified_at=? WHERE id=? AND revision=? AND enabled=1")
            .bind(revision).bind(batch.proposed_checkpoint).bind(batch.verified_at).bind(&batch.source_id).bind(batch.base_revision)
            .execute(&mut *tx).await?;
        if result.rows_affected() != 1 {
            return Err(SyncError::Conflict);
        }
        tx.commit().await?;
        Ok(CommittedSourceRevision {
            source_revision: revision,
            changed_events,
        })
    }
}

enum Prepared {
    Upsert {
        calendar_id: String,
        uid: String,
        sequence: Option<u64>,
        document: String,
        hash: String,
    },
    Delete(String),
}

impl Prepared {
    fn uid(&self) -> &str {
        match self {
            Self::Upsert { uid, .. } | Self::Delete(uid) => uid,
        }
    }
}

#[derive(sqlx::FromRow)]
struct Existing {
    id: String,
    revision: i64,
    calendar_id: String,
    content_hash: String,
    document_json: String,
    deleted_at: Option<i64>,
}

async fn existing(
    connection: &mut SqliteConnection,
    source: &str,
    uid: &str,
) -> Result<Option<Existing>, SyncError> {
    Ok(sqlx::query_as("SELECT id,revision,calendar_id,content_hash,document_json,deleted_at FROM events WHERE source_id=? AND external_uid=?")
        .bind(source).bind(uid).fetch_optional(connection).await?)
}

struct Journal<'a> {
    source_id: &'a str,
    revision: i64,
    at: i64,
}

impl Journal<'_> {
    async fn write(
        &self,
        connection: &mut SqliteConnection,
        id: &str,
        event_revision: i64,
        document: Option<&str>,
        kind: &str,
    ) -> Result<(), SyncError> {
        sqlx::query("INSERT INTO event_revisions(event_id,revision,document_json,change_kind,changed_at) VALUES (?,?,?,?,?)")
            .bind(id).bind(event_revision).bind(document).bind(kind).bind(self.at).execute(&mut *connection).await?;
        sqlx::query("INSERT INTO source_changes(source_id,source_revision,event_id,event_revision,change_kind) VALUES (?,?,?,?,?)")
            .bind(self.source_id).bind(self.revision).bind(id).bind(event_revision).bind(kind).execute(connection).await?;
        Ok(())
    }
}

async fn delete(
    connection: &mut SqliteConnection,
    journal: &Journal<'_>,
    uid: &str,
) -> Result<usize, SyncError> {
    let Some(old) = existing(connection, journal.source_id, uid).await? else {
        return Ok(0);
    };
    if old.deleted_at.is_some() {
        return Ok(0);
    }
    let revision = old
        .revision
        .checked_add(1)
        .ok_or(SyncError::RevisionOverflow)?;
    sqlx::query("UPDATE events SET revision=?,deleted_at=? WHERE id=?")
        .bind(revision)
        .bind(journal.at)
        .bind(&old.id)
        .execute(&mut *connection)
        .await?;
    journal
        .write(connection, &old.id, revision, None, "delete")
        .await?;
    Ok(1)
}
