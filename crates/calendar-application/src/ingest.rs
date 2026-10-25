//! Protected, source-scoped machine writes. Authorization and mutation are one repository call.
use crate::{BatchError, EventV1};
use calendar_domain::validate_event;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestCondition {
    Absent,
    Revision(i64),
}

#[derive(Debug, Clone)]
pub enum IngestOperation {
    Upsert {
        calendar_id: String,
        event: Box<EventV1>,
    },
    Delete,
}

#[derive(Debug, Clone)]
pub struct IngestRequest {
    pub source_id: String,
    pub uid: String,
    pub key: String,
    pub condition: IngestCondition,
    pub operation: IngestOperation,
}
impl IngestRequest {
    pub fn validate(&self) -> Result<(), BatchError> {
        if self.source_id.is_empty()
            || self.source_id.len() > 256
            || self.uid.is_empty()
            || self.uid.len() > 1024
            || self.key.is_empty()
            || self.key.len() > 128
            || !self.key.bytes().all(|b| b.is_ascii_graphic())
            || matches!(self.condition, IngestCondition::Revision(n) if n < 1)
            || matches!(
                (&self.operation, self.condition),
                (IngestOperation::Delete, IngestCondition::Absent)
            )
        {
            return Err(BatchError::Shape("invalid ingest request"));
        }
        if let IngestOperation::Upsert { calendar_id, event } = &self.operation {
            if calendar_id.is_empty() || calendar_id.len() > 256 || event.external_uid != self.uid {
                return Err(BatchError::Shape("invalid calendar or UID mismatch"));
            }
            validate_event(event).map_err(|errors| BatchError::InvalidEvent { errors })?;
        }
        Ok(())
    }
}

/// Expired entries may be reused. Capacity exhaustion rejects new writes rather than evicting
/// unexpired keys. UTC rollback cannot prune unexpired records early.
#[derive(Debug, Clone, Copy)]
pub struct IngestPolicy {
    pub retention_ms: i64,
    pub max_records: i64,
}
impl Default for IngestPolicy {
    fn default() -> Self {
        Self {
            retention_ms: 86_400_000,
            max_records: 100_000,
        }
    }
}
impl IngestPolicy {
    pub fn valid(self) -> bool {
        (60_000..=604_800_000).contains(&self.retention_ms)
            && (1..=1_000_000).contains(&self.max_records)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestResult {
    pub event_revision: i64,
    pub source_revision: i64,
    pub deleted: bool,
    pub created: bool,
    pub replayed: bool,
    pub expires_at: i64,
    pub request_id: String,
}
