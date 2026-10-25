//! Concrete first-version composition. Business configuration stays in the existing ports.
use crate::{
    delivery_worker::{delivery_round, DeliveryProgress, DeliveryWorkerLimits},
    source_worker::{IcsSourceRunner, SourceProgress, SourceScanLimits, SourceScanner},
    supervision::{Progress, SupervisionConfig, WorkerFailure, WorkerKind},
    ConfigError, DeploymentConfig, Server, Services, StartError,
};
use calendar_adapters::{bark::BarkOutput, remote::HttpFetcher};
use calendar_application::{
    configuration::{PlanningConfiguration, ReminderConfigurationRepository},
    Clock, DeliveryEnvelope, DeliveryError, DeliveryOutcome, PlanningError, PlanningLimits,
    PlanningRequest, PushOutput, ReminderPlanner, RemoteFetchLimits, SyncLimits,
};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const DELAY: Duration = Duration::from_secs(1);
const ROUND: Duration = Duration::from_secs(2);
const HORIZON_MS: i64 = 30 * 86_400_000;
const EXTENSION_MS: i64 = 300_000;
const MAX_LEAD_MS: i64 = 7 * 86_400_000;

/// Production budgets allow the existing 30s global delivery deadline plus storage finish.
/// Per-output timeout remains an additional upper bound, not an extension of the global cap.
pub fn supervision_budgets() -> SupervisionConfig {
    SupervisionConfig {
        stall_timeout: Duration::from_secs(120),
        ..SupervisionConfig::default()
    }
}
/// Healthcheck and credential CLI bypass this entry point. Serving requires real security
/// bootstrap; validate presence before creating/migrating storage or opening a listener.
pub async fn initialize(config: DeploymentConfig) -> Result<Server, StartError> {
    if config.admin.is_none() || config.credentials.is_none() {
        return Err(
            ConfigError("production runtime requires admin and credentials configuration").into(),
        );
    }
    config
        .initialize_with_supervision(supervision_budgets())
        .await?
        .with_production_runtime()
}

struct RuntimeClock {
    started: Instant,
}
impl RuntimeClock {
    fn new() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}
impl Clock for RuntimeClock {
    fn utc_now_ms(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
            Err(e) => -i64::try_from(e.duration().as_millis()).unwrap_or(i64::MAX),
        }
    }
    fn monotonic_now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

struct ConfiguredBark {
    services: Arc<Services>,
    credentials: Arc<crate::credentials_bootstrap::RuntimeCredentialProvider>,
    degraded: AtomicBool,
    storage_failed: AtomicBool,
}
impl ConfiguredBark {
    fn new(services: Arc<Services>) -> Result<Self, StartError> {
        let credentials = services
            .credential_provider()
            .map_err(|_| ConfigError("runtime credential provider unavailable"))?;
        Ok(Self {
            services,
            credentials: Arc::new(credentials),
            degraded: AtomicBool::new(false),
            storage_failed: AtomicBool::new(false),
        })
    }
}
impl PushOutput for ConfiguredBark {
    async fn deliver(&self, envelope: DeliveryEnvelope) -> DeliveryOutcome {
        // Never cache a generation fence across sends or deserialize credentials into config.
        let record = match self
            .services
            .storage
            .output_for_delivery(&envelope.job_id, &envelope.output_id)
            .await
        {
            Ok(Some(record)) => record,
            Ok(None) => return DeliveryOutcome::PermanentFailure,
            Err(_) => {
                self.storage_failed.store(true, Ordering::SeqCst);
                return DeliveryOutcome::Retryable {
                    retry_after_ms: None,
                };
            }
        };
        let config = record.configuration;
        let outcome = match BarkOutput::new(
            self.credentials.clone(),
            &record.output_id,
            &config.credential_id,
            &config.origin,
            Duration::from_millis(config.request_timeout_ms),
        ) {
            Ok(output) => output.deliver(envelope).await,
            Err(_) => DeliveryOutcome::PermanentFailure,
        };
        self.degraded
            .store(outcome != DeliveryOutcome::Accepted, Ordering::SeqCst);
        outcome
    }
}

fn request(scope: PlanningConfiguration) -> PlanningRequest {
    PlanningRequest {
        source_id: scope.source_id,
        base_revision: scope.source_revision,
        rule_id: scope.rule_id,
        rule_generation: scope.rule_generation,
        output_id: scope.output_id,
        output_generation: scope.output_generation,
        horizon_ms: HORIZON_MS,
        max_lead_ms: MAX_LEAD_MS,
        extension_interval_ms: EXTENSION_MS,
        tzdata_version: calendar_domain::TZDB_VERSION.into(),
        limits: PlanningLimits {
            max_occurrences: 10_000,
            max_iterations: 100_000,
            max_elapsed_ms: 1_000,
        },
    }
}
#[derive(Default)]
struct Planner {
    cursor: Option<String>,
    pending: VecDeque<PlanningConfiguration>,
}
impl Planner {
    async fn round<C: Clock>(
        &mut self,
        services: &Services,
        clock: &C,
        mut progress: impl FnMut(Progress),
    ) -> Result<(), WorkerFailure> {
        let started = clock.monotonic_now_ms();
        if self.pending.is_empty() {
            let page = services
                .run_task(async |storage| {
                    storage
                        .planning_configurations(self.cursor.as_deref())
                        .await
                })
                .await;
            let Some(page) = page else { return Ok(()) };
            let page = page.map_err(|_| WorkerFailure)?;
            self.cursor = page.next_after;
            self.pending = page.items.into();
            progress(Progress::Completed); // actual bounded discovery, including empty page
        }
        for _ in 0..8 {
            if !services.ready()
                || clock.monotonic_now_ms().saturating_sub(started) >= ROUND.as_millis() as u64
            {
                break;
            }
            let Some(scope) = self.pending.pop_front() else {
                break;
            };
            let request = request(scope.clone());
            let result = services
                .run_task(async |storage| {
                    if !storage
                        .planning_required(&request, clock.utc_now_ms())
                        .await?
                    {
                        return Ok(());
                    }
                    if clock.monotonic_now_ms().saturating_sub(started) >= ROUND.as_millis() as u64
                    {
                        return Err(PlanningError::Busy);
                    }
                    storage.reconcile(request, clock).await
                })
                .await;
            match result {
                None => break,
                Some(Ok(())) => progress(Progress::Completed),
                Some(Err(PlanningError::Conflict)) => progress(Progress::Completed),
                Some(Err(PlanningError::Busy)) => {
                    self.pending.push_front(scope);
                    break;
                }
                Some(Err(PlanningError::Database(_))) => return Err(WorkerFailure),
                Some(Err(_)) => progress(Progress::ExternalDegraded), // bounded invalid/unsupported input; do not advance watermark
            }
        }
        Ok(())
    }
}

async fn paused(services: &Services) -> Result<bool, WorkerFailure> {
    match services.run_task(async |s| s.delivery_paused().await).await {
        None => Ok(true),
        Some(Ok(p)) => Ok(p),
        Some(Err(_)) => Err(WorkerFailure),
    }
}
async fn wait(services: &Services) {
    let mut shutdown = services.shutdown.subscribe();
    if !*shutdown.borrow_and_update() {
        tokio::select! { ()=tokio::time::sleep(DELAY)=>{}, _=shutdown.changed()=>{} }
    }
}
impl Server {
    pub fn with_production_runtime(mut self) -> Result<Self, StartError> {
        if self.admin.is_none() {
            return Err(ConfigError("production runtime requires admin bootstrap").into());
        }
        let output = ConfiguredBark::new(self.services.clone())?;
        let provider = self
            .services
            .credential_provider()
            .map_err(|_| ConfigError("runtime credential provider unavailable"))?;
        let fetcher = HttpFetcher::new(
            Arc::new(provider),
            RemoteFetchLimits {
                timeout: Duration::from_secs(20),
                ..RemoteFetchLimits::default()
            },
        )
        .map_err(|_| ConfigError("invalid source runtime limits"))?;
        let runner = IcsSourceRunner {
            fetcher: Arc::new(fetcher),
            ics_limits: self.services.ics_limits,
            sync_limits: SyncLimits::default(),
        };
        let source_limits =
            SourceScanLimits::new(1, 8, 16_384, ROUND, DELAY, DELAY, Duration::from_secs(300))
                .map_err(|_| ConfigError("invalid source runtime limits"))?;
        let delivery_limits = DeliveryWorkerLimits::new(8, ROUND, DELAY)
            .map_err(|_| ConfigError("invalid delivery runtime limits"))?;
        let services = self.services.clone();
        self = self
            .with_worker(WorkerKind::SourceSync, move |context| async move {
                let clock = RuntimeClock::new();
                let mut scanner = SourceScanner::new(source_limits);
                while services.ready() {
                    if paused(&services).await? {
                        context.progress(Progress::RecoveryPaused);
                    } else {
                        scanner
                            .round(&services, &runner, &clock, |p| {
                                context.progress(match p {
                                    SourceProgress::Scanned { external_degraded }
                                    | SourceProgress::Synchronized {
                                        external_degraded, ..
                                    } => {
                                        if external_degraded {
                                            Progress::ExternalDegraded
                                        } else {
                                            Progress::Completed
                                        }
                                    }
                                    SourceProgress::ExternalFailure { .. } => {
                                        Progress::ExternalDegraded
                                    }
                                })
                            })
                            .await
                            .map_err(|_| WorkerFailure)?;
                    }
                    wait(&services).await;
                }
                Ok(())
            })
            .map_err(|_| ConfigError("source worker already installed"))?;
        let services = self.services.clone();
        self = self
            .with_worker(WorkerKind::Planner, move |context| async move {
                let clock = RuntimeClock::new();
                let mut planner = Planner::default();
                while services.ready() {
                    if paused(&services).await? {
                        context.progress(Progress::RecoveryPaused);
                    } else {
                        planner
                            .round(&services, &clock, |p| context.progress(p))
                            .await?;
                    }
                    wait(&services).await;
                }
                Ok(())
            })
            .map_err(|_| ConfigError("planner worker already installed"))?;
        let services = self.services.clone();
        self.with_worker(WorkerKind::Delivery, move |context| async move {
            let clock = RuntimeClock::new();
            while services.ready() {
                let result = delivery_round(
                    &services,
                    &output,
                    "calrelay-delivery",
                    delivery_limits,
                    &clock,
                    |p| {
                        context.progress(match p {
                            DeliveryProgress::RecoveryPaused => Progress::RecoveryPaused,
                            _ if output.degraded.load(Ordering::SeqCst) => {
                                Progress::ExternalDegraded
                            }
                            _ => Progress::Completed,
                        })
                    },
                )
                .await;
                if output.storage_failed.load(Ordering::SeqCst) {
                    return Err(WorkerFailure);
                }
                match result {
                    Ok(_) | Err(DeliveryError::Busy | DeliveryError::StaleLease) => {}
                    Err(_) => return Err(WorkerFailure),
                }
                wait(&services).await;
            }
            Ok(())
        })
        .map_err(|_| ConfigError("delivery worker already installed").into())
    }
}

#[cfg(test)]
#[path = "../tests/common/mod.rs"]
mod test_support;
#[cfg(test)]
mod tests;
