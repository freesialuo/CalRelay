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
use calendar_storage::Storage;
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
struct Pinned<A> {
    adapter: A,
    revision: i64,
    conditional: bool,
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
        Ok(synchronize(
            storage,
            &adapter,
            &record.source_id,
            now,
            DeletionPolicy::Limit(100),
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
struct Schedule {
    config_revision: i64,
    due: u64,
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
                        (record, state)
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
                let mono = clock.monotonic_now_ms();
                if !self.schedules.contains_key(&summary.source_id)
                    && self.schedules.len() >= self.limits.max_tracked
                {
                    return Err(SourceScanError::Capacity);
                }
                let entry = self
                    .schedules
                    .entry(summary.source_id.clone())
                    .or_insert_with(|| Schedule {
                        config_revision: record.config_revision,
                        due: mono.saturating_add(remaining(
                            state.last_verified_at,
                            clock.utc_now_ms(),
                            interval,
                        )),
                        failures: 0,
                    });
                if entry.config_revision != record.config_revision {
                    *entry = Schedule {
                        config_revision: record.config_revision,
                        due: mono,
                        failures: 0,
                    };
                }
                if mono >= entry.due {
                    let Some(result) = services
                        .run_task(async |db| runner.run(db, &record, clock.utc_now_ms()).await)
                        .await
                    else {
                        break 'scan SourceRoundEnd::Stopped;
                    };
                    attempted += 1;
                    let finished = clock.monotonic_now_ms();
                    match result {
                        Ok(outcome) => {
                            entry.failures = 0;
                            entry.due = finished.saturating_add(interval);
                            progress(SourceProgress::Synchronized {
                                source_id: summary.source_id.clone(),
                                not_modified: matches!(outcome, SyncOutcome::NotModified),
                                external_degraded: self.schedules.values().any(|s| s.failures > 0),
                            });
                        }
                        Err(SourceRunError::Sync(IngestionError::Storage)) => {
                            return Err(SourceScanError::Storage)
                        }
                        Err(error) => {
                            entry.failures = entry.failures.saturating_add(1);
                            let factor = 1_u64 << entry.failures.saturating_sub(1).min(63);
                            entry.due = finished.saturating_add(
                                self.limits
                                    .backoff_ms
                                    .saturating_mul(factor)
                                    .min(self.limits.max_backoff_ms),
                            );
                            progress(SourceProgress::ExternalFailure {
                                source_id: summary.source_id.clone(),
                                error,
                            });
                        }
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
fn remaining(verified: Option<i64>, now: i64, interval: u64) -> u64 {
    match verified {
        None => 0,
        Some(time) if time > now => interval,
        Some(time) => interval.saturating_sub(now.saturating_sub(time) as u64),
    }
}
