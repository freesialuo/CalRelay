//! Full-snapshot coordination. Fetch/parse outside the atomic repository boundary.
use crate::{
    CommittedSourceRevision, DeletionPolicy, EventRepository, SyncBatch, SyncMode, SyncRecord,
    MAX_BATCH_RECORDS,
};
use std::{collections::HashSet, future::Future, time::Duration};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceState {
    pub revision: i64,
    pub enabled: bool,
    pub checkpoint: Option<String>,
    pub last_verified_at: Option<i64>,
    pub has_snapshot: bool,
}
impl SourceState {
    pub fn push_ready(&self, now: i64, max_age_ms: i64) -> bool {
        self.enabled
            && max_age_ms >= 0
            && self
                .last_verified_at
                .is_some_and(|t| t <= now && now.saturating_sub(t) <= max_age_ms)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IngestionError {
    #[error("source unavailable")]
    Unavailable,
    #[error("source revision conflict")]
    Conflict,
    #[error("source adapter failed")]
    Adapter,
    #[error("source storage failed")]
    Storage,
    #[error("incomplete or invalid snapshot")]
    InvalidSnapshot,
    #[error("source sync budget exceeded")]
    Budget,
    #[error("source sync deadline exceeded")]
    Timeout,
    #[error("304 without committed snapshot")]
    UnexpectedNotModified,
}
pub trait SourceRepository: EventRepository + Send + Sync {
    fn source_state(
        &self,
        source: &str,
    ) -> impl Future<Output = Result<SourceState, IngestionError>> + Send;
    fn verify_unchanged(
        &self,
        source: &str,
        base: i64,
        now: i64,
    ) -> impl Future<Output = Result<(), IngestionError>> + Send;
    fn commit_snapshot(
        &self,
        batch: SyncBatch,
    ) -> impl Future<Output = Result<CommittedSourceRevision, IngestionError>> + Send;
}
pub struct SourcePage {
    pub records: Vec<SyncRecord>,
    pub next: Option<String>,
    pub checkpoint: Option<String>,
    pub bytes: usize,
    /// Stable upstream collection version, required for every page of a paginated snapshot.
    pub snapshot_token: Option<String>,
}
pub enum SourcePageOutcome {
    Modified(SourcePage),
    NotModified,
}
pub trait SourceAdapter: Send + Sync {
    fn page(
        &self,
        state: &SourceState,
        cursor: Option<&str>,
        now: i64,
    ) -> impl Future<Output = Result<SourcePageOutcome, IngestionError>> + Send;
}
#[derive(Debug, Clone, Copy)]
pub struct SyncLimits {
    pub max_pages: usize,
    pub max_bytes: usize,
    pub max_records: usize,
    pub timeout: Duration,
}
impl Default for SyncLimits {
    fn default() -> Self {
        Self {
            max_pages: 32,
            max_bytes: 32 * 1024 * 1024,
            max_records: MAX_BATCH_RECORDS,
            timeout: Duration::from_secs(60),
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum SyncOutcome {
    Committed(CommittedSourceRevision),
    NotModified,
}
/// Concurrent runs use source CAS: one complete snapshot can win for each base revision.
/// A rejected old run must refetch, never replay its old snapshot against a new revision.
pub async fn synchronize<R: SourceRepository, A: SourceAdapter>(
    repository: &R,
    adapter: &A,
    source: &str,
    now: i64,
    deletions: DeletionPolicy,
    limits: SyncLimits,
) -> Result<SyncOutcome, IngestionError> {
    if source.is_empty()
        || limits.max_pages == 0
        || limits.max_pages > 128
        || limits.max_bytes == 0
        || limits.max_bytes > 64 * 1024 * 1024
        || limits.max_records == 0
        || limits.max_records > MAX_BATCH_RECORDS
        || limits.timeout.is_zero()
        || limits.timeout > Duration::from_secs(120)
    {
        return Err(IngestionError::Budget);
    }
    let deadline = tokio::time::Instant::now() + limits.timeout;
    tokio::time::timeout(limits.timeout, async {
        let state = repository.source_state(source).await?;
        if !state.enabled {
            return Err(IngestionError::Unavailable);
        }
        let mut records = Vec::new();
        let mut bytes = 0usize;
        let mut next = None;
        let mut seen = HashSet::new();
        let mut checkpoint = None;
        let mut snapshot_token = None;
        for index in 0..limits.max_pages {
            let page = adapter.page(&state, next.as_deref(), now).await?;
            if tokio::time::Instant::now() >= deadline {
                return Err(IngestionError::Timeout);
            }
            match page {
                SourcePageOutcome::NotModified => {
                    if index != 0 || !state.has_snapshot {
                        return Err(IngestionError::UnexpectedNotModified);
                    }
                    repository
                        .verify_unchanged(source, state.revision, now)
                        .await?;
                    return Ok(SyncOutcome::NotModified);
                }
                SourcePageOutcome::Modified(page) => {
                    bytes = bytes
                        .checked_add(page.bytes)
                        .ok_or(IngestionError::Budget)?;
                    if bytes > limits.max_bytes
                        || page.records.len() > limits.max_records - records.len()
                    {
                        return Err(IngestionError::Budget);
                    }
                    if index == 0 {
                        checkpoint = page.checkpoint;
                        snapshot_token = page.snapshot_token.clone();
                        if page.next.is_some() && snapshot_token.is_none() {
                            return Err(IngestionError::InvalidSnapshot);
                        }
                    } else if snapshot_token.is_none() || page.snapshot_token != snapshot_token {
                        return Err(IngestionError::InvalidSnapshot);
                    }
                    if page
                        .snapshot_token
                        .as_ref()
                        .is_some_and(|s| s.is_empty() || s.len() > 256)
                    {
                        return Err(IngestionError::InvalidSnapshot);
                    }
                    records.extend(page.records);
                    match page.next {
                        Some(cursor) => {
                            if cursor.is_empty()
                                || cursor.len() > 8192
                                || !seen.insert(cursor.clone())
                            {
                                return Err(IngestionError::InvalidSnapshot);
                            }
                            next = Some(cursor);
                        }
                        None => {
                            let batch = SyncBatch {
                                source_id: source.into(),
                                base_revision: state.revision,
                                mode: SyncMode::FullSnapshot {
                                    complete: true,
                                    deletion_policy: deletions,
                                },
                                records,
                                proposed_checkpoint: checkpoint,
                                verified_at: now,
                            };
                            batch
                                .validate()
                                .map_err(|_| IngestionError::InvalidSnapshot)?;
                            return repository
                                .commit_snapshot(batch)
                                .await
                                .map(SyncOutcome::Committed);
                        }
                    }
                }
            }
        }
        Err(IngestionError::Budget)
    })
    .await
    .map_err(|_| IngestionError::Timeout)?
}
