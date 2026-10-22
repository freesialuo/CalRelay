//! Application contracts. No database or network dependencies.

use std::{collections::HashSet, future::Future};

pub use calendar_domain::EventV1;
use calendar_domain::{validate_event, ValidationError};

pub const MAX_BATCH_RECORDS: usize = 10_000;

#[derive(Debug, Clone, Copy)]
pub enum DeletionPolicy {
    Limit(usize),
    /// Only a trusted caller that has obtained administrator approval may use this.
    Approved,
}

#[derive(Debug, Clone, Copy)]
pub enum SyncMode {
    Delta,
    FullSnapshot {
        complete: bool,
        deletion_policy: DeletionPolicy,
    },
}

#[derive(Debug, Clone)]
pub enum SyncRecord {
    Upsert {
        calendar_id: String,
        event: Box<EventV1>,
    },
    Delete {
        external_uid: String,
    },
}

impl SyncRecord {
    pub fn external_uid(&self) -> &str {
        match self {
            Self::Upsert { event, .. } => &event.external_uid,
            Self::Delete { external_uid } => external_uid,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SyncBatch {
    /// Injected by the trusted source context, never taken from an event document.
    pub source_id: String,
    pub base_revision: i64,
    pub mode: SyncMode,
    pub records: Vec<SyncRecord>,
    pub proposed_checkpoint: Option<String>,
    pub verified_at: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum BatchError {
    #[error("invalid sync batch: {0}")]
    Shape(&'static str),
    #[error("event failed domain validation")]
    InvalidEvent { errors: Vec<ValidationError> },
}

impl SyncBatch {
    /// Pure validation before acquiring a database write lock.
    pub fn validate(&self) -> Result<(), BatchError> {
        if self.source_id.is_empty()
            || self.base_revision < 0
            || self.records.len() > MAX_BATCH_RECORDS
        {
            return Err(BatchError::Shape("invalid source, revision or batch size"));
        }
        if matches!(
            self.mode,
            SyncMode::FullSnapshot {
                complete: false,
                ..
            }
        ) {
            return Err(BatchError::Shape("incomplete snapshot"));
        }
        let mut seen = HashSet::with_capacity(self.records.len());
        for record in &self.records {
            if record.external_uid().is_empty() || !seen.insert(record.external_uid()) {
                return Err(BatchError::Shape("empty or duplicate UID"));
            }
            match record {
                SyncRecord::Upsert { calendar_id, event } => {
                    if calendar_id.is_empty() {
                        return Err(BatchError::Shape("empty calendar"));
                    }
                    validate_event(event).map_err(|errors| BatchError::InvalidEvent { errors })?;
                }
                SyncRecord::Delete { .. } if !matches!(self.mode, SyncMode::Delta) => {
                    return Err(BatchError::Shape("snapshot contains explicit deletion"));
                }
                SyncRecord::Delete { .. } => {}
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommittedSourceRevision {
    pub source_revision: i64,
    pub changed_events: usize,
}

pub trait EventRepository {
    type Error: std::error::Error + Send + Sync + 'static;
    fn commit_sync(
        &self,
        batch: SyncBatch,
    ) -> impl Future<Output = Result<CommittedSourceRevision, Self::Error>> + Send;
}
