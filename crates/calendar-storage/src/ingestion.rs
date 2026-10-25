use crate::{Storage, SyncError};
use calendar_application::{
    CommittedSourceRevision, EventRepository, IngestionError as E, SourceRepository, SourceState,
    SyncBatch,
};
fn map(error: SyncError) -> E {
    match error {
        SyncError::Conflict => E::Conflict,
        SyncError::SourceUnavailable => E::Unavailable,
        SyncError::Invalid(_)
        | SyncError::CalendarMismatch
        | SyncError::StaleSequence
        | SyncError::ReviewRequired { .. } => E::InvalidSnapshot,
        _ => E::Storage,
    }
}
type SourceRow = (i64, bool, Option<String>, Option<i64>, Option<i64>);
impl SourceRepository for Storage {
    async fn source_state(&self, source: &str) -> Result<SourceState, E> {
        let row:Option<SourceRow>=sqlx::query_as("SELECT revision,enabled,committed_checkpoint,last_verified_at,snapshot_revision FROM sources WHERE id=?").bind(source).fetch_optional(self.pool()).await.map_err(|_|E::Storage)?;
        let (revision, enabled, checkpoint, last_verified_at, snapshot) =
            row.ok_or(E::Unavailable)?;
        Ok(SourceState {
            revision,
            enabled,
            checkpoint,
            last_verified_at,
            has_snapshot: snapshot.is_some(),
        })
    }
    async fn verify_unchanged(&self, source: &str, base: i64, now: i64) -> Result<(), E> {
        let r=sqlx::query("UPDATE sources SET last_verified_at=CASE WHEN last_verified_at IS NULL OR last_verified_at<? THEN ? ELSE last_verified_at END WHERE id=? AND revision=? AND enabled=1 AND snapshot_revision IS NOT NULL")
   .bind(now).bind(now).bind(source).bind(base).execute(self.pool()).await.map_err(|_|E::Storage)?;
        if r.rows_affected() != 1 {
            return Err(E::Conflict);
        }
        Ok(())
    }
    async fn commit_snapshot(&self, batch: SyncBatch) -> Result<CommittedSourceRevision, E> {
        if !matches!(
            batch.mode,
            calendar_application::SyncMode::FullSnapshot { complete: true, .. }
        ) {
            return Err(E::InvalidSnapshot);
        }
        self.commit_sync(batch).await.map_err(map)
    }
}
