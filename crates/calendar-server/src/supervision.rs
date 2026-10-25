//! Supervise real futures and completed operations. Registration is not a heartbeat.
use crate::{ConfigError, Server, StartError};
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, sync::watch, task::JoinSet, time::Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WorkerKind {
    SourceSync,
    Planner,
    Delivery,
}
const REQUIRED: [WorkerKind; 3] = [
    WorkerKind::SourceSync,
    WorkerKind::Planner,
    WorkerKind::Delivery,
];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    Http,
    DatabaseProbe,
    Worker(WorkerKind),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CoreFault {
    #[error("core task exited unexpectedly: {0:?}")]
    Exited(Component),
    #[error("core task failed: {0:?}")]
    Failed(Component),
    #[error("core task panicked: {0:?}")]
    Panicked(Component),
    #[error("core heartbeat stalled: {0:?}")]
    Stalled(Component),
    #[error("core shutdown budget exceeded")]
    ShutdownTimeout,
    #[error("shutdown cancellation or storage cleanup did not complete")]
    ShutdownIncomplete,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    Completed,
    ExternalDegraded,
    RecoveryPaused,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseState {
    Unknown,
    Unavailable,
    Failed,
    Ready { recovery_paused: bool },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Starting,
    NotReady,
    Running,
    RecoveryPaused,
    Stopping,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthSnapshot {
    pub live: bool,
    pub ready: bool,
    pub phase: Phase,
    pub missing_workers: Vec<WorkerKind>,
    pub pending_workers: Vec<WorkerKind>,
    pub degraded_workers: Vec<WorkerKind>,
    pub database: DatabaseState,
    pub fault: Option<CoreFault>,
}
#[derive(Debug, Clone, Copy)]
pub struct SupervisionConfig {
    pub startup_grace: Duration,
    pub stall_timeout: Duration,
    pub probe_interval: Duration,
    pub probe_timeout: Duration,
    /// Graceful drain budget, then an equal bounded cancellation/rollback window if needed.
    pub shutdown_timeout: Duration,
}
impl Default for SupervisionConfig {
    fn default() -> Self {
        Self {
            startup_grace: Duration::from_secs(60),
            stall_timeout: Duration::from_secs(30),
            probe_interval: Duration::from_secs(1),
            probe_timeout: Duration::from_secs(1),
            shutdown_timeout: Duration::from_secs(10),
        }
    }
}
impl SupervisionConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.startup_grace.is_zero()
            || self.startup_grace > Duration::from_secs(300)
            || [
                self.stall_timeout,
                self.probe_interval,
                self.probe_timeout,
                self.shutdown_timeout,
            ]
            .iter()
            .any(|d| d.is_zero() || *d > Duration::from_secs(120))
            || self.stall_timeout < self.probe_interval + self.probe_timeout * 2
        {
            return Err(ConfigError(
                "supervision budgets (stall >= interval + 2 * probe timeout)",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Copy)]
struct Beat {
    at: Instant,
    progress: Progress,
}
struct State {
    started: Instant,
    http: Option<Instant>,
    database: DatabaseState,
    db_at: Option<Instant>,
    workers: BTreeMap<WorkerKind, Option<Beat>>,
    stopping: bool,
    fault: Option<CoreFault>,
}
pub struct Health {
    options: SupervisionConfig,
    state: Mutex<State>,
    changes: watch::Sender<()>,
}
impl Health {
    pub(crate) fn new(options: SupervisionConfig) -> Self {
        let (changes, _) = watch::channel(());
        Self {
            options,
            changes,
            state: Mutex::new(State {
                started: Instant::now(),
                http: None,
                database: DatabaseState::Unknown,
                db_at: None,
                workers: BTreeMap::new(),
                stopping: false,
                fault: None,
            }),
        }
    }
    fn read<T>(&self, f: impl FnOnce(&State) -> T) -> T {
        f(&self.state.lock().unwrap_or_else(|p| p.into_inner()))
    }
    fn update(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.state.lock().unwrap_or_else(|p| p.into_inner()));
        self.changes.send_replace(());
    }
    pub fn subscribe(&self) -> watch::Receiver<()> {
        self.changes.subscribe()
    }
    pub fn snapshot(&self) -> HealthSnapshot {
        self.snapshot_at(Instant::now())
    }
    fn fault_at(&self, s: &State, now: Instant) -> Option<CoreFault> {
        if s.fault.is_some() {
            return s.fault;
        }
        if s.database == DatabaseState::Failed {
            return Some(CoreFault::Failed(Component::DatabaseProbe));
        }
        if s.stopping {
            return None;
        }
        let expired = |at: Option<Instant>| match at {
            Some(at) => now.duration_since(at) >= self.options.stall_timeout,
            None => now.duration_since(s.started) >= self.options.startup_grace,
        };
        if expired(s.http) {
            return Some(CoreFault::Stalled(Component::Http));
        }
        for (kind, beat) in &s.workers {
            if expired(beat.map(|b| b.at)) {
                return Some(CoreFault::Stalled(Component::Worker(*kind)));
            }
        }
        None
    }
    fn snapshot_at(&self, now: Instant) -> HealthSnapshot {
        self.read(|s| {
            let fault = self.fault_at(s, now);
            let missing_workers: Vec<_> = REQUIRED
                .into_iter()
                .filter(|k| !s.workers.contains_key(k))
                .collect();
            let fresh = |at: Option<Instant>| {
                at.is_some_and(|at| now.duration_since(at) < self.options.stall_timeout)
            };
            let paused = matches!(
                s.database,
                DatabaseState::Ready {
                    recovery_paused: true
                }
            );
            let ready = fault.is_none()
                && !s.stopping
                && !paused
                && missing_workers.is_empty()
                && matches!(
                    s.database,
                    DatabaseState::Ready {
                        recovery_paused: false
                    }
                )
                && fresh(s.db_at)
                && fresh(s.http)
                && s.workers.values().all(|b| {
                    b.is_some_and(|b| fresh(Some(b.at)) && b.progress != Progress::RecoveryPaused)
                });
            let phase = if fault.is_some() {
                Phase::Failed
            } else if s.stopping {
                Phase::Stopping
            } else if paused {
                Phase::RecoveryPaused
            } else if ready {
                Phase::Running
            } else if now.duration_since(s.started) < self.options.startup_grace {
                Phase::Starting
            } else {
                Phase::NotReady
            };
            HealthSnapshot {
                live: fault.is_none(),
                ready,
                phase,
                missing_workers,
                pending_workers: s
                    .workers
                    .iter()
                    .filter_map(|(kind, beat)| beat.is_none().then_some(*kind))
                    .collect(),
                degraded_workers: s
                    .workers
                    .iter()
                    .filter_map(|(k, b)| {
                        b.filter(|b| b.progress == Progress::ExternalDegraded)
                            .map(|_| *k)
                    })
                    .collect(),
                database: s.database,
                fault,
            }
        })
    }
    pub(crate) fn stop(&self) {
        self.update(|s| s.stopping = true);
    }
    fn fail(&self, fault: CoreFault) {
        self.update(|s| {
            if s.fault.is_none() {
                s.fault = Some(fault);
            }
        });
    }
    fn database_result(&self, state: DatabaseState, now: Instant) {
        self.update(|s| {
            // An inconclusive retry cannot erase observed failure evidence.
            if s.database != DatabaseState::Failed || state != DatabaseState::Unavailable {
                s.database = state;
            }
            s.db_at = matches!(state, DatabaseState::Ready { .. }).then_some(now);
        });
    }
    fn report(&self, kind: WorkerKind, progress: Progress, now: Instant) {
        self.update(|s| {
            if !s.stopping {
                if let Some(beat) = s.workers.get_mut(&kind) {
                    *beat = Some(Beat { at: now, progress });
                }
            }
        });
    }
}
#[derive(Debug, thiserror::Error)]
#[error("core worker already registered: {0:?}")]
pub struct RegistrationError(pub WorkerKind);
#[derive(Debug, Clone, Copy)]
pub struct WorkerFailure;
pub struct WorkerContext {
    kind: WorkerKind,
    health: Arc<Health>,
    shutdown: watch::Receiver<bool>,
}
impl WorkerContext {
    /// Call only after a real bounded scan/operation, including an observed external failure.
    pub fn progress(&self, progress: Progress) {
        self.health.report(self.kind, progress, Instant::now());
    }
    pub fn recovery_paused(&self) -> Option<bool> {
        match self.health.snapshot().database {
            DatabaseState::Ready { recovery_paused } => Some(recovery_paused),
            _ => None,
        }
    }
    pub async fn cancelled(&mut self) {
        cancelled(&mut self.shutdown).await;
    }
}
pub(crate) struct Worker {
    kind: WorkerKind,
    future: Pin<Box<dyn Future<Output = Result<(), WorkerFailure>> + Send>>,
}
impl Server {
    /// Assembly interface only. The standard binary registers no production workers.
    pub fn with_worker<F, Fut>(
        mut self,
        kind: WorkerKind,
        factory: F,
    ) -> Result<Self, RegistrationError>
    where
        F: FnOnce(WorkerContext) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), WorkerFailure>> + Send + 'static,
    {
        let inserted = {
            let mut state = self
                .services
                .health
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let std::collections::btree_map::Entry::Vacant(entry) = state.workers.entry(kind) {
                entry.insert(None);
                true
            } else {
                false
            }
        };
        if !inserted {
            return Err(RegistrationError(kind));
        }
        let context = WorkerContext {
            kind,
            health: self.services.health.clone(),
            shutdown: self.services.shutdown.subscribe(),
        };
        self.workers.push(Worker {
            kind,
            future: Box::pin(async move { factory(context).await }),
        });
        Ok(self)
    }
}
async fn cancelled(receiver: &mut watch::Receiver<bool>) {
    while !*receiver.borrow_and_update() {
        if receiver.changed().await.is_err() {
            break;
        }
    }
}

/// Roll back an actual main-database write in a short transaction; never persist probe state.
/// The existing recovery policy row must exist and is never resumed or assigned a new cutoff.
async fn database_probe(storage: &calendar_storage::Storage) -> Result<bool, sqlx::Error> {
    let mut tx = storage.pool().begin_with("BEGIN IMMEDIATE").await?;
    let paused: bool = sqlx::query_scalar("SELECT paused FROM recovery_control WHERE id=1")
        .fetch_one(&mut *tx)
        .await?;
    let result = sqlx::query("UPDATE recovery_control SET paused=paused WHERE id=1")
        .execute(&mut *tx)
        .await?;
    if result.rows_affected() != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    tx.rollback().await?;
    Ok(paused)
}
fn transient_database(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::PoolTimedOut)
        || match error {
            sqlx::Error::Database(e) => e
                .code()
                .and_then(|v| v.parse::<u32>().ok())
                .is_some_and(|c| matches!(c & 0xff, 5 | 6)),
            _ => false,
        }
}
async fn probes(
    services: Arc<crate::Services>,
    address: std::net::SocketAddr,
) -> Result<(), CoreFault> {
    let options = services.health.options;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(options.probe_timeout)
        .build()
        .map_err(|_| CoreFault::Failed(Component::Http))?;
    let mut shutdown = services.shutdown.subscribe();
    loop {
        let round = async {
            match tokio::time::timeout(options.probe_timeout, database_probe(&services.storage))
                .await
            {
                Ok(Ok(paused)) => services.health.database_result(
                    DatabaseState::Ready {
                        recovery_paused: paused,
                    },
                    Instant::now(),
                ),
                result => {
                    let failed = matches!(&result, Ok(Err(error)) if !transient_database(error));
                    services.health.database_result(
                        if failed {
                            DatabaseState::Failed
                        } else {
                            DatabaseState::Unavailable
                        },
                        Instant::now(),
                    );
                }
            }

            if let Ok(response) = client
                .get(format!("http://{address}/api/v1/version"))
                .send()
                .await
            {
                if response.status() == reqwest::StatusCode::OK {
                    services.health.update(|s| s.http = Some(Instant::now()));
                }
            }
            Ok(())
        };
        tokio::select! { biased; () = cancelled(&mut shutdown) => return Ok(()), result = round => result? }
        tokio::select! { biased; () = cancelled(&mut shutdown) => return Ok(()), () = tokio::time::sleep(options.probe_interval) => {} }
    }
}

pub(crate) async fn serve(
    mut server: Server,
    listener: TcpListener,
    signal: impl Future<Output = ()> + Send + 'static,
) -> Result<(), StartError> {
    let services = server.services.clone();
    let options = services.health.options;
    let mut address = listener.local_addr()?;
    if address.ip().is_unspecified() {
        address.set_ip(if address.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        });
    }
    let router = server.router();
    let mut tasks = JoinSet::new();
    let mut identities = std::collections::HashMap::new();
    let mut http_shutdown = services.shutdown.subscribe();
    let id = tasks
        .spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move { cancelled(&mut http_shutdown).await })
                .await
                .map_err(|_| CoreFault::Failed(Component::Http))
        })
        .id();
    identities.insert(id, Component::Http);
    let probe_services = services.clone();
    let id = tasks
        .spawn(async move { probes(probe_services, address).await })
        .id();
    identities.insert(id, Component::DatabaseProbe);
    for worker in server.workers.drain(..) {
        let kind = worker.kind;
        let id = tasks
            .spawn(async move {
                worker
                    .future
                    .await
                    .map_err(|_| CoreFault::Failed(Component::Worker(kind)))
            })
            .id();
        identities.insert(id, Component::Worker(kind));
    }
    let mut shutdown = services.shutdown.subscribe();
    tokio::pin!(signal);
    // Health findings stay observable through the running HTTP server. Only an explicit
    // shutdown request or the actual HTTP future ending enters the shutdown path.
    let terminal = loop {
        tokio::select! {
            biased;
            () = &mut signal => break None,
            () = cancelled(&mut shutdown) => break None,
            result = tasks.join_next_with_id() => {
                let (component, fault) = task_result(result.expect("HTTP task remains installed"), &identities, true);
                if let Some(fault) = fault { services.health.fail(fault); }
                if component == Component::Http { break fault; }
            }
        }
    };
    services.stop();
    let mut shutdown_http_fault = None;
    let finish = async {
        while let Some(result) = tasks.join_next_with_id().await {
            let (component, fault) = task_result(result, &identities, false);
            if let Some(fault) = fault {
                services.health.fail(fault);
                if component == Component::Http {
                    shutdown_http_fault = Some(fault);
                }
            }
        }
        services.drain().await;
    };
    if tokio::time::timeout(options.shutdown_timeout, finish)
        .await
        .is_err()
    {
        services.health.fail(CoreFault::ShutdownTimeout);
        // All admitted operations participate, including futures in an embedding task
        // outside this JoinSet. The latched signal wins over a concurrently released barrier.
        services.cancel_admitted();
        tasks.abort_all();
        let cleanup = async {
            tokio::join!(tasks.shutdown(), services.drain_cancelled());
        };
        if tokio::time::timeout(options.shutdown_timeout, cleanup)
            .await
            .is_err()
        {
            // The pool has been marked closed and cancellation remains latched. Do not
            // report completed cleanup if foreign leases or noncooperative I/O still exist.
            return Err(CoreFault::ShutdownIncomplete.into());
        }
        return Err(CoreFault::ShutdownTimeout.into());
    }
    match terminal.or(shutdown_http_fault) {
        Some(fault) => Err(fault.into()),
        None => Ok(()),
    }
}

fn task_result(
    result: Result<(tokio::task::Id, Result<(), CoreFault>), tokio::task::JoinError>,
    identities: &std::collections::HashMap<tokio::task::Id, Component>,
    running: bool,
) -> (Component, Option<CoreFault>) {
    match result {
        Ok((id, result)) => {
            let component = identities[&id];
            (
                component,
                result
                    .err()
                    .or_else(|| running.then_some(CoreFault::Exited(component))),
            )
        }
        Err(error) => {
            let component = identities[&error.id()];
            (component, Some(CoreFault::Panicked(component)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn assembled() -> (Health, Instant) {
        let health = Health::new(SupervisionConfig::default());
        let now = health.read(|s| s.started);
        health.update(|s| {
            s.http = Some(now);
            s.database = DatabaseState::Ready {
                recovery_paused: false,
            };
            s.db_at = Some(now);
            for kind in REQUIRED {
                s.workers.insert(
                    kind,
                    Some(Beat {
                        at: now,
                        progress: Progress::Completed,
                    }),
                );
            }
        });
        (health, now)
    }
    #[test]
    fn monotonic_grace_and_exact_stall_boundary_do_not_mask_an_established_heartbeat() {
        let health = Health::new(SupervisionConfig::default());
        let now = health.read(|s| s.started);
        assert!(health.snapshot_at(now + Duration::from_secs(59)).live);
        assert!(!health.snapshot_at(now + Duration::from_secs(60)).live);
        let (health, now) = assembled();
        assert!(health.snapshot_at(now + Duration::from_secs(29)).live);
        assert_eq!(
            health.snapshot_at(now + Duration::from_secs(30)).fault,
            Some(CoreFault::Stalled(Component::Http))
        );
        health.update(|s| s.http = Some(now + Duration::from_secs(30)));
        assert_eq!(
            health.snapshot_at(now + Duration::from_secs(30)).fault,
            Some(CoreFault::Stalled(Component::Worker(
                WorkerKind::SourceSync
            )))
        );
    }
    #[test]
    fn missing_workers_never_become_ready_or_cause_a_restart_with_a_live_http_core() {
        let health = Health::new(SupervisionConfig::default());
        let now = health.read(|s| s.started) + Duration::from_secs(1000);
        health.update(|s| {
            s.http = Some(now);
            s.db_at = Some(now);
            s.database = DatabaseState::Ready {
                recovery_paused: false,
            };
        });
        let snapshot = health.snapshot_at(now);
        assert!(snapshot.live);
        assert!(!snapshot.ready);
        assert_eq!(snapshot.missing_workers, REQUIRED);
        health.update(|s| {
            for kind in REQUIRED {
                s.workers.insert(kind, None);
            }
        });
        assert!(
            !health.snapshot_at(now).live,
            "registration is not progress"
        );
    }
    #[test]
    fn external_degradation_is_live_and_ready_pause_is_live_but_not_ready_and_fault_is_sticky() {
        let (health, now) = assembled();
        health.report(WorkerKind::Delivery, Progress::ExternalDegraded, now);
        let snapshot = health.snapshot_at(now);
        assert!(snapshot.live && snapshot.ready);
        assert_eq!(snapshot.degraded_workers, [WorkerKind::Delivery]);
        health.update(|s| {
            s.database = DatabaseState::Ready {
                recovery_paused: true,
            }
        });
        health.report(WorkerKind::Delivery, Progress::RecoveryPaused, now);
        let snapshot = health.snapshot_at(now);
        assert!(snapshot.live && !snapshot.ready);
        assert_eq!(snapshot.phase, Phase::RecoveryPaused);
        health.fail(CoreFault::Failed(Component::Worker(WorkerKind::Planner)));
        health.report(WorkerKind::Planner, Progress::Completed, now);
        health.stop();
        assert!(!health.snapshot_at(now).live);
        let (healthy, now) = assembled();
        healthy.stop();
        let snapshot = healthy.snapshot_at(now + Duration::from_secs(1000));
        assert!(snapshot.live && !snapshot.ready);
        assert_eq!(snapshot.phase, Phase::Stopping);
    }
    #[test]
    fn a_db_failure_requires_real_success_before_a_later_timeout_can_clear_it() {
        let (health, now) = assembled();
        health.database_result(DatabaseState::Failed, now);
        assert!(!health.snapshot_at(now).live);
        health.database_result(DatabaseState::Unavailable, now);
        assert!(
            !health.snapshot_at(now).live,
            "a timeout is not evidence that the failed DB recovered"
        );
        assert_eq!(health.snapshot_at(now).database, DatabaseState::Failed);
        health.database_result(
            DatabaseState::Ready {
                recovery_paused: false,
            },
            now,
        );
        assert!(health.snapshot_at(now).live && health.snapshot_at(now).ready);
    }
    #[test]
    fn budget_configuration_fails_closed() {
        assert!(SupervisionConfig::default().validate().is_ok());
        for bad in [
            SupervisionConfig {
                startup_grace: Duration::from_secs(301),
                ..SupervisionConfig::default()
            },
            SupervisionConfig {
                stall_timeout: Duration::from_millis(1),
                ..SupervisionConfig::default()
            },
            SupervisionConfig {
                probe_timeout: Duration::ZERO,
                ..SupervisionConfig::default()
            },
        ] {
            assert!(bad.validate().is_err());
        }
    }
}
