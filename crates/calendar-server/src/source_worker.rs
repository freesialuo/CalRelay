//! Single-instance pull scheduler. No production registration or new business configuration.
use crate::Services;
use calendar_adapters::{
    ingestion::{HttpSource, SourceFormat},
    IcsLimits,
};
use calendar_application::{
    configuration::{AdapterKind, ConfigurationError, SourceConfigurationRecord, SourceMode},
    synchronize, Clock, DeletionPolicy, IngestionError, RemoteFetcher, SourceAdapter,
    SourcePageOutcome, SourceRepository, SourceState, SyncLimits, SyncOutcome,
};
use calendar_storage::{SourceSyncOutcome, SourceSyncStatus, Storage};
use std::{collections::BTreeMap, future::Future, sync::Arc, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SourceRunError {
    #[error("source configuration is not supported by this ICS runner")]
    Unsupported,
    #[error(transparent)]
    Sync(#[from] IngestionError),
}
pub trait SourceRunner: Sync {
    fn run(
        &self,
        storage: &Storage,
        record: &SourceConfigurationRecord,
        now: i64,
    ) -> impl Future<Output = Result<SyncOutcome, SourceRunError>> + Send;
}
pub struct IcsSourceRunner<F> {
    pub fetcher: Arc<F>,
    pub ics_limits: IcsLimits,
    pub sync_limits: SyncLimits,
}
pub(crate) struct Pinned<A> {
    pub(crate) adapter: A,
    pub(crate) revision: i64,
    pub(crate) conditional: bool,
}
impl<A: SourceAdapter> SourceAdapter for Pinned<A> {
    async fn page(
        &self,
        state: &SourceState,
        cursor: Option<&str>,
        now: i64,
    ) -> Result<SourcePageOutcome, IngestionError> {
        if state.revision != self.revision {
            return Err(IngestionError::Conflict);
        }
        let mut state = state.clone();
        if !self.conditional {
            state.checkpoint = None;
            state.has_snapshot = false;
        }
        let outcome = self.adapter.page(&state, cursor, now).await?;
        if !self.conditional && matches!(outcome, SourcePageOutcome::NotModified) {
            return Err(IngestionError::UnexpectedNotModified);
        }
        Ok(outcome)
    }
}
impl<F: RemoteFetcher> SourceRunner for IcsSourceRunner<F> {
    async fn run(
        &self,
        storage: &Storage,
        record: &SourceConfigurationRecord,
        now: i64,
    ) -> Result<SyncOutcome, SourceRunError> {
        let config = &record.configuration;
        if config.validate().is_err() || !pull(record) || config.calendars.len() != 1 {
            return Err(SourceRunError::Unsupported);
        }
        let authentication = storage
            .source_authentication(config.credential_id.as_deref())
            .await?;
        let adapter = Pinned {
            revision: record.source_revision,
            conditional: config.capabilities.conditional_read,
            adapter: HttpSource {
                fetcher: self.fetcher.clone(),
                url: config.endpoint.clone().ok_or(SourceRunError::Unsupported)?,
                authentication,
                calendar_id: config.calendars[0].id.clone(),
                format: SourceFormat::Ics(self.ics_limits),
            },
        };
        // Only a separate administrator approval of exactly this revision lifts the limit.
        let deletions = if storage
            .source_deletions_approved(
                &record.source_id,
                record.config_revision,
                record.source_revision,
                now,
            )
            .await?
        {
            DeletionPolicy::Approved
        } else {
            DeletionPolicy::Limit(100)
        };
        Ok(synchronize(
            storage,
            &adapter,
            &record.source_id,
            now,
            deletions,
            self.sync_limits,
        )
        .await?)
    }
}
fn pull(record: &SourceConfigurationRecord) -> bool {
    let c = &record.configuration;
    record.retired_at.is_none()
        && c.enabled
        && c.mode == SourceMode::Pull
        && c.adapter == AdapterKind::Ics
        && c.capabilities.snapshot
}

#[derive(Clone, Copy, Debug)]
pub struct SourceScanLimits {
    max_pages: usize,
    max_attempts: usize,
    max_tracked: usize,
    budget_ms: u64,
    delay: Duration,
    backoff_ms: u64,
    max_backoff_ms: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SourceScanError {
    #[error("invalid scanner limits")]
    InvalidLimits,
    #[error("source runtime storage failed")]
    Storage,
    #[error("source scheduling capacity exceeded")]
    Capacity,
}
impl SourceScanLimits {
    pub fn new(
        max_pages: usize,
        max_attempts: usize,
        max_tracked: usize,
        budget: Duration,
        delay: Duration,
        backoff: Duration,
        max_backoff: Duration,
    ) -> Result<Self, SourceScanError> {
        if !(1..=16).contains(&max_pages)
            || !(1..=64).contains(&max_attempts)
            || !(1..=16384).contains(&max_tracked)
            || budget < Duration::from_millis(1)
            || budget > Duration::from_secs(120)
            || delay < Duration::from_millis(1)
            || delay > Duration::from_secs(60)
            || backoff < delay
            || max_backoff < backoff
            || max_backoff > Duration::from_secs(3600)
        {
            return Err(SourceScanError::InvalidLimits);
        }
        Ok(Self {
            max_pages,
            max_attempts,
            max_tracked,
            budget_ms: budget.as_millis() as u64,
            delay,
            backoff_ms: backoff.as_millis() as u64,
            max_backoff_ms: max_backoff.as_millis() as u64,
        })
    }
    /// Next schedule after one attempt; `None` for a storage failure, which is not external.
    fn plan(
        &self,
        failures: u32,
        interval: u64,
        result: &Result<SyncOutcome, SourceRunError>,
    ) -> Option<Plan> {
        let error = match result {
            Ok(outcome) => {
                return Some(Plan {
                    outcome: match outcome {
                        SyncOutcome::NotModified => SourceSyncOutcome::NotModified,
                        SyncOutcome::Committed(_) => SourceSyncOutcome::Success,
                    },
                    failures: 0,
                    due_in: interval,
                    allowed_in: self.backoff_ms,
                })
            }
            Err(SourceRunError::Unsupported) => None,
            Err(SourceRunError::Sync(error)) => Some(*error),
        };
        let outcome = match error {
            None => SourceSyncOutcome::Unsupported,
            Some(IngestionError::Storage) => return None,
            Some(IngestionError::Unavailable) => SourceSyncOutcome::Unavailable,
            Some(IngestionError::Conflict) => SourceSyncOutcome::Conflict,
            Some(IngestionError::Adapter) => SourceSyncOutcome::Adapter,
            Some(IngestionError::InvalidSnapshot) => SourceSyncOutcome::InvalidSnapshot,
            Some(IngestionError::Budget) => SourceSyncOutcome::Budget,
            Some(IngestionError::Timeout) => SourceSyncOutcome::Timeout,
            Some(IngestionError::UnexpectedNotModified) => SourceSyncOutcome::UnexpectedNotModified,
            Some(IngestionError::RetryAfter { .. }) => SourceSyncOutcome::RetryAfter,
            Some(IngestionError::ReviewRequired { .. }) => SourceSyncOutcome::ReviewRequired,
        };
        // Upstream Retry-After is a lower bound the local cap never shortens.
        let upstream = match error {
            Some(IngestionError::RetryAfter { retry_after_ms }) => {
                retry_after_ms.min(MAX_RETRY_AFTER_MS)
            }
            _ => 0,
        };
        let failures = failures.saturating_add(1);
        Some(Plan {
            outcome,
            failures,
            due_in: self.backoff(failures).max(upstream),
            allowed_in: self.backoff_ms.max(upstream),
        })
    }
    /// Local exponential backoff after `failures` consecutive failures, capped.
    fn backoff(&self, failures: u32) -> u64 {
        let factor = 1_u64 << failures.saturating_sub(1).min(63);
        self.backoff_ms
            .saturating_mul(factor)
            .min(self.max_backoff_ms)
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum SourceProgress {
    /// Completed real bounded discovery, including an empty page. Never emitted during waits.
    Scanned { external_degraded: bool },
    Synchronized {
        source_id: String,
        not_modified: bool,
        external_degraded: bool,
    },
    ExternalFailure {
        source_id: String,
        error: SourceRunError,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceRoundEnd {
    Complete,
    PageLimit,
    AttemptLimit,
    Deadline,
    Stopped,
}
#[derive(Debug, PartialEq, Eq)]
pub struct SourceRound {
    pub pages: usize,
    pub attempted: usize,
    pub end: SourceRoundEnd,
}
/// Schedule after one attempt, relative to its completion; also what is persisted.
#[derive(Clone, Copy)]
struct Plan {
    outcome: SourceSyncOutcome,
    failures: u32,
    due_in: u64,
    allowed_in: u64,
}
/// Longest honoured upstream wait: the largest configurable refresh interval (7 days).
const MAX_RETRY_AFTER_MS: u64 = 604_800_000;
struct Schedule {
    config_revision: i64,
    due: u64,
    /// Earliest manual refresh: the upstream Retry-After and a local floor between requests.
    not_before: u64,
    failures: u32,
}
pub struct SourceScanner {
    limits: SourceScanLimits,
    after: Option<String>,
    schedules: BTreeMap<String, Schedule>,
}
impl SourceScanner {
    pub fn new(limits: SourceScanLimits) -> Self {
        Self {
            limits,
            after: None,
            schedules: BTreeMap::new(),
        }
    }
    /// The budget fences new admissions, not an already-admitted bounded sync/commit.
    pub async fn round<R: SourceRunner, C: Clock + ?Sized>(
        &mut self,
        services: &Services,
        runner: &R,
        clock: &C,
        mut progress: impl FnMut(SourceProgress),
    ) -> Result<SourceRound, SourceScanError> {
        let started = clock.monotonic_now_ms();
        let mut pages = 0;
        let mut attempted = 0;
        let end = 'scan: loop {
            if !services.ready() {
                break SourceRoundEnd::Stopped;
            }
            if pages >= self.limits.max_pages {
                break SourceRoundEnd::PageLimit;
            }
            if clock.monotonic_now_ms().saturating_sub(started) >= self.limits.budget_ms {
                break SourceRoundEnd::Deadline;
            }
            let Some(page) = services
                .run_task(async |db| db.list_source_configurations(self.after.as_deref()).await)
                .await
            else {
                break SourceRoundEnd::Stopped;
            };
            let page = page.map_err(|_| SourceScanError::Storage)?;
            pages += 1;
            progress(SourceProgress::Scanned {
                external_degraded: self.schedules.values().any(|s| s.failures > 0),
            });
            for summary in page.items {
                if !services.ready() {
                    break 'scan SourceRoundEnd::Stopped;
                }
                if attempted >= self.limits.max_attempts {
                    break 'scan SourceRoundEnd::AttemptLimit;
                }
                if clock.monotonic_now_ms().saturating_sub(started) >= self.limits.budget_ms {
                    break 'scan SourceRoundEnd::Deadline;
                }
                if !summary.enabled || summary.retired_at.is_some() {
                    self.schedules.remove(&summary.source_id);
                    self.after = Some(summary.source_id);
                    continue;
                }
                let Some(loaded) = services
                    .run_task(async |db| {
                        let record = db.source_configuration(&summary.source_id).await;
                        let state = db.source_state(&summary.source_id).await;
                        let refresh = db.source_refresh_request(&summary.source_id).await;
                        let status = db.source_sync_status(&summary.source_id).await;
                        (record, state, refresh, status)
                    })
                    .await
                else {
                    break 'scan SourceRoundEnd::Stopped;
                };
                let record = match loaded.0 {
                    Ok(record) => record,
                    Err(ConfigurationError::NotFound) => {
                        self.after = Some(summary.source_id);
                        continue;
                    }
                    Err(_) => return Err(SourceScanError::Storage),
                };
                let state = match loaded.1 {
                    Ok(state) => state,
                    Err(IngestionError::Unavailable) => {
                        self.after = Some(summary.source_id);
                        continue;
                    }
                    Err(_) => return Err(SourceScanError::Storage),
                };
                let refresh = loaded.2.map_err(|_| SourceScanError::Storage)?;
                let status = loaded.3.map_err(|_| SourceScanError::Storage)?;
                if !pull(&record) {
                    self.schedules.remove(&summary.source_id);
                    self.after = Some(summary.source_id);
                    continue;
                }
                let interval = record
                    .configuration
                    .refresh_interval_ms
                    .filter(|n| (60_000..=604_800_000).contains(n))
                    .ok_or(SourceScanError::Storage)?;
                let mut mono = clock.monotonic_now_ms();
                if mono.saturating_sub(started) >= self.limits.budget_ms {
                    break 'scan SourceRoundEnd::Deadline;
                }
                if !self.schedules.contains_key(&summary.source_id)
                    && self.schedules.len() >= self.limits.max_tracked
                {
                    let ids: Vec<_> = self.schedules.keys().cloned().collect();
                    let Some(active) = services
                        .run_task(async |db| db.active_pull_source_ids(&ids).await)
                        .await
                    else {
                        break 'scan SourceRoundEnd::Stopped;
                    };
                    let active: std::collections::BTreeSet<_> = active
                        .map_err(|_| SourceScanError::Storage)?
                        .into_iter()
                        .collect();
                    self.schedules.retain(|id, _| active.contains(id));
                    progress(SourceProgress::Scanned {
                        external_degraded: self.schedules.values().any(|s| s.failures > 0),
                    });
                    mono = clock.monotonic_now_ms();
                    if mono.saturating_sub(started) >= self.limits.budget_ms {
                        break 'scan SourceRoundEnd::Deadline;
                    }
                    if self.schedules.len() >= self.limits.max_tracked {
                        return Err(SourceScanError::Capacity);
                    }
                }
                let limits = self.limits;
                let entry = self
                    .schedules
                    .entry(summary.source_id.clone())
                    .or_insert_with(|| {
                        let now = clock.utc_now_ms();
                        let mut schedule = Schedule {
                            config_revision: record.config_revision,
                            due: mono.saturating_add(remaining(
                                state.last_verified_at,
                                now,
                                interval,
                            )),
                            not_before: 0,
                            failures: 0,
                        };
                        // A restart restores the persisted wait of this configuration revision.
                        if let Some(s) =
                            status.filter(|s| s.config_revision == record.config_revision)
                        {
                            let floor = left(s.attempted_at, s.next_allowed_at, now);
                            let local = match s.consecutive_failures {
                                0 => 0,
                                n => left(
                                    s.attempted_at,
                                    s.attempted_at.saturating_add(limits.backoff(n) as i64),
                                    now,
                                ),
                            };
                            schedule.failures = s.consecutive_failures;
                            schedule.not_before = mono.saturating_add(floor);
                            schedule.due = schedule.due.max(mono.saturating_add(floor.max(local)));
                        }
                        schedule
                    });
                if entry.config_revision != record.config_revision {
                    *entry = Schedule {
                        config_revision: record.config_revision,
                        due: mono,
                        not_before: 0,
                        failures: 0,
                    };
                }
                // A manual refresh skips the period and local backoff, never `not_before`. One
                // made against a superseded configuration is dropped.
                let stale = refresh.filter(|r| r.config_revision != record.config_revision);
                let manual = refresh.filter(|r| r.config_revision == record.config_revision);
                let run = mono >= entry.due || (manual.is_some() && mono >= entry.not_before);
                if let Some(stale) = stale {
                    let Some(cleared) = services
                        .run_task(async |db| {
                            db.clear_source_refresh(&summary.source_id, stale.sequence)
                                .await
                        })
                        .await
                    else {
                        break 'scan SourceRoundEnd::Stopped;
                    };
                    cleared.map_err(|_| SourceScanError::Storage)?;
                }
                if run {
                    let failures = entry.failures;
                    let id = &summary.source_id;
                    let Some(admission) = services
                        .run_task(async |db| {
                            if clock.monotonic_now_ms().saturating_sub(started) >= limits.budget_ms
                            {
                                return None;
                            }
                            let result = runner.run(db, &record, clock.utc_now_ms()).await;
                            let finished = clock.monotonic_now_ms();
                            // Planned and persisted in the same admission as the sync, so an
                            // outcome completing during shutdown is still recorded.
                            let plan = limits.plan(failures, interval, &result);
                            let recorded = match plan {
                                Some(plan) => {
                                    let now = clock.utc_now_ms();
                                    let status = SourceSyncStatus {
                                        config_revision: record.config_revision,
                                        attempted_at: now,
                                        outcome: plan.outcome,
                                        consecutive_failures: plan.failures,
                                        next_allowed_at: now.saturating_add(plan.allowed_in as i64),
                                    };
                                    db.record_source_sync_status(id, status).await
                                }
                                None => Err(IngestionError::Storage),
                            };
                            Some((result, finished, plan, recorded))
                        })
                        .await
                    else {
                        break 'scan SourceRoundEnd::Stopped;
                    };
                    let Some((result, finished, plan, recorded)) = admission else {
                        break 'scan SourceRoundEnd::Deadline;
                    };
                    attempted += 1;
                    let (Some(plan), Ok(())) = (plan, recorded) else {
                        return Err(SourceScanError::Storage);
                    };
                    entry.failures = plan.failures;
                    entry.due = finished.saturating_add(plan.due_in);
                    entry.not_before = finished.saturating_add(plan.allowed_in);
                    progress(match result {
                        Ok(outcome) => SourceProgress::Synchronized {
                            source_id: summary.source_id.clone(),
                            not_modified: matches!(outcome, SyncOutcome::NotModified),
                            external_degraded: self.schedules.values().any(|s| s.failures > 0),
                        },
                        Err(error) => SourceProgress::ExternalFailure {
                            source_id: summary.source_id.clone(),
                            error,
                        },
                    });
                    // Cleared only once the sync has an outcome, so a request that was not
                    // admitted this round is still served; a newer one keeps its sequence.
                    if let Some(served) = manual {
                        let Some(cleared) = services
                            .run_task(async |db| {
                                db.clear_source_refresh(&summary.source_id, served.sequence)
                                    .await
                            })
                            .await
                        else {
                            break 'scan SourceRoundEnd::Stopped;
                        };
                        cleared.map_err(|_| SourceScanError::Storage)?;
                    }
                }
                self.after = Some(summary.source_id);
            }
            self.after = page.next_after;
            if self.after.is_none() {
                break SourceRoundEnd::Complete;
            }
        };
        Ok(SourceRound {
            pages,
            attempted,
            end,
        })
    }
    pub async fn run<R: SourceRunner, C: Clock + ?Sized>(
        &mut self,
        services: &Services,
        runner: &R,
        clock: &C,
        mut progress: impl FnMut(SourceProgress),
    ) -> Result<(), SourceScanError> {
        let mut shutdown = services.shutdown.subscribe();
        loop {
            let round = self.round(services, runner, clock, &mut progress).await?;
            if round.end == SourceRoundEnd::Stopped || *shutdown.borrow_and_update() {
                return Ok(());
            }
            tokio::select! { ()=tokio::time::sleep(self.limits.delay)=>{}, _=shutdown.changed()=>{} }
        }
    }
}
/// Milliseconds left of the persisted wait `start..end` at UTC `now`. A clock behind `start` waits
/// the whole span; the span is capped at the longest honoured wait so nothing stalls forever.
fn left(start: i64, end: i64, now: i64) -> u64 {
    let span = u64::try_from(end.saturating_sub(start))
        .unwrap_or(0)
        .min(MAX_RETRY_AFTER_MS);
    match u64::try_from(now.saturating_sub(start)) {
        Ok(elapsed) => span.saturating_sub(elapsed),
        Err(_) => span,
    }
}
fn remaining(verified: Option<i64>, now: i64, interval: u64) -> u64 {
    match verified {
        None => 0,
        Some(time) if time > now => interval,
        Some(time) => interval.saturating_sub(now.saturating_sub(time) as u64),
    }
}
