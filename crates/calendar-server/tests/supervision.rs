use axum::{body::Body, http::Request};
use calendar_server::{Config, Server};
use std::time::Duration;
use tower::ServiceExt;
fn config(path: &std::path::Path) -> Config {
    Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        database: path.into(),
        max_request_bytes: 1024,
        max_response_bytes: 16384,
        request_timeout: Duration::from_secs(1),
    }
}
#[tokio::test]
async fn unassembled_workers_are_not_ready() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::initialize(config(&dir.path().join("health.db")))
        .await
        .unwrap();
    let response = server
        .router()
        .oneshot(
            Request::builder()
                .uri("/health/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status().as_u16(),
        503,
        "missing production workers must not be reported ready"
    );
    assert!(
        server.services.ready(),
        "business admission remains independent of health readiness"
    );
    server.services.drain().await;
}

use calendar_server::supervision::{
    Component, CoreFault, DatabaseState, Phase, Progress, SupervisionConfig, WorkerFailure,
    WorkerKind,
};
use std::sync::Arc;
fn options() -> SupervisionConfig {
    SupervisionConfig {
        startup_grace: Duration::from_secs(5),
        stall_timeout: Duration::from_secs(3),
        probe_interval: Duration::from_millis(10),
        probe_timeout: Duration::from_millis(30),
        shutdown_timeout: Duration::from_millis(500),
    }
}
async fn wait_health(
    services: &Arc<calendar_server::Services>,
    predicate: impl Fn(&calendar_server::supervision::HealthSnapshot) -> bool,
) {
    let mut changes = services.health.subscribe();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if predicate(&services.health.snapshot()) {
                break;
            }
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("observable health transition");
}
fn synthetic_workers(mut server: Server) -> Server {
    for kind in [
        WorkerKind::SourceSync,
        WorkerKind::Planner,
        WorkerKind::Delivery,
    ] {
        let services = server.services.clone();
        server = server
            .with_worker(kind, move |mut context| async move {
                // A real bounded DB operation in a synthetic worker, not a heartbeat ticker.
                let paused: bool =
                    sqlx::query_scalar("SELECT paused FROM recovery_control WHERE id=1")
                        .fetch_one(services.storage.pool())
                        .await
                        .map_err(|_| WorkerFailure)?;
                context.progress(if paused {
                    Progress::RecoveryPaused
                } else {
                    Progress::Completed
                });
                context.cancelled().await;
                Ok(())
            })
            .unwrap();
    }
    server
}

#[tokio::test]
async fn core_exit_panic_and_stall_only_expose_health_without_stopping_the_server() {
    for mode in [0, 1, 2, 3] {
        let dir = tempfile::tempdir().unwrap();
        let mut budgets = options();
        budgets.stall_timeout = Duration::from_millis(200);
        let server =
            Server::initialize_with_supervision(config(&dir.path().join("core.db")), budgets)
                .await
                .unwrap();
        let services = server.services.clone();
        let server = server
            .with_worker(WorkerKind::Planner, move |mut context| async move {
                match mode {
                    0 => Ok(()),
                    1 => Err(WorkerFailure),
                    2 => panic!("synthetic panic"),
                    _ => {
                        context.progress(Progress::Completed);
                        context.cancelled().await;
                        Ok(())
                    }
                }
            })
            .unwrap();
        let listener = server.bind().await.unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let serving = tokio::spawn(server.serve(listener, async {
            let _ = rx.await;
        }));
        let expected = match mode {
            0 => CoreFault::Exited(Component::Worker(WorkerKind::Planner)),
            1 => CoreFault::Failed(Component::Worker(WorkerKind::Planner)),
            2 => CoreFault::Panicked(Component::Worker(WorkerKind::Planner)),
            _ => CoreFault::Stalled(Component::Worker(WorkerKind::Planner)),
        };
        wait_health(&services, |s| s.fault == Some(expected)).await;
        assert!(
            services.ready(),
            "a health finding must not stop business admission or terminate the process"
        );
        assert!(!serving.is_finished());
        let response = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap()
            .get(format!("http://{address}/health/live"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 503);
        assert!(response.headers().contains_key("x-request-id"));
        tx.send(()).unwrap();
        serving.await.unwrap().unwrap();
        assert!(services.storage.pool().is_closed());
    }
}

#[tokio::test]
async fn real_db_probe_busy_recovery_and_concurrent_shutdown_keep_admission_separate() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(&dir.path().join("probe.db"));
    let server = synthetic_workers(
        Server::initialize_with_supervision(cfg.clone(), options())
            .await
            .unwrap(),
    );
    let services = server.services.clone();
    let listener = server.bind().await.unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(server.serve(listener, async {
        let _ = rx.await;
    }));
    wait_health(&services, |s| s.ready).await;
    let mut lock = services
        .storage
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .unwrap();
    sqlx::query("UPDATE recovery_control SET paused=paused WHERE id=1")
        .execute(&mut *lock)
        .await
        .unwrap();
    wait_health(&services, |s| s.database == DatabaseState::Unavailable).await;
    assert!(!services.health.snapshot().ready);
    assert!(services.health.snapshot().live);
    assert!(services.ready());
    lock.rollback().await.unwrap();
    wait_health(&services, |s| s.ready).await;
    // Several independent stop callers and the signal share one idempotent cancellation path.
    let a = services.clone();
    let b = services.clone();
    let one = tokio::spawn(async move {
        a.stop();
    });
    let two = tokio::spawn(async move {
        b.stop();
    });
    let _ = tx.send(());
    one.await.unwrap();
    two.await.unwrap();
    serving.await.unwrap().unwrap();
    assert_eq!(services.health.snapshot().phase, Phase::Stopping);
    let reopened = calendar_storage::Storage::open(&cfg.database)
        .await
        .unwrap();
    let policy: (i64, Option<i64>) =
        sqlx::query_as("SELECT paused,cutoff FROM recovery_control WHERE id=1")
            .fetch_one(reopened.pool())
            .await
            .unwrap();
    assert_eq!(policy, (0, None));
    reopened.close().await;
}

#[tokio::test]
async fn recovery_pause_is_live_not_ready_and_probes_never_resume_or_rewrite_cutoff() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("paused.db");
    for _ in 0..2 {
        let server = Server::initialize_with_supervision(config(&path), options())
            .await
            .unwrap();
        sqlx::query("UPDATE recovery_control SET paused=1,cutoff=1000 WHERE id=1")
            .execute(server.services.storage.pool())
            .await
            .unwrap();
        let server = synthetic_workers(server);
        let services = server.services.clone();
        let listener = server.bind().await.unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let serving = tokio::spawn(server.serve(listener, async {
            let _ = rx.await;
        }));
        wait_health(&services, |s| {
            s.phase == Phase::RecoveryPaused
                && s.missing_workers.is_empty()
                && s.pending_workers.is_empty()
        })
        .await;
        let snapshot = services.health.snapshot();
        assert!(snapshot.live && !snapshot.ready);
        let policy: (i64, i64) =
            sqlx::query_as("SELECT paused,cutoff FROM recovery_control WHERE id=1")
                .fetch_one(services.storage.pool())
                .await
                .unwrap();
        assert_eq!(policy, (1, 1000));
        let attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM delivery_attempts")
            .fetch_one(services.storage.pool())
            .await
            .unwrap();
        assert_eq!(attempts, 0);
        tx.send(()).unwrap();
        serving.await.unwrap().unwrap();
    }
    let db = calendar_storage::Storage::open(path).await.unwrap();
    let policy: (i64, i64) =
        sqlx::query_as("SELECT paused,cutoff FROM recovery_control WHERE id=1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(policy, (1, 1000));
    db.close().await;
}

#[tokio::test]
async fn external_503_is_observed_progress_not_a_core_failure() {
    let peer = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = peer.local_addr().unwrap();
    let peer_task = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut socket, _) = peer.accept().await.unwrap();
        let mut bytes = [0; 2048];
        let _ = socket.read(&mut bytes).await.unwrap();
        socket.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let mut server =
        Server::initialize_with_supervision(config(&dir.path().join("external.db")), options())
            .await
            .unwrap();
    for kind in [WorkerKind::SourceSync, WorkerKind::Planner] {
        let storage = server.services.clone();
        server = server
            .with_worker(kind, move |mut context| async move {
                let _: i64 = sqlx::query_scalar("SELECT count(*) FROM sources")
                    .fetch_one(storage.storage.pool())
                    .await
                    .map_err(|_| WorkerFailure)?;
                context.progress(Progress::Completed);
                context.cancelled().await;
                Ok(())
            })
            .unwrap();
    }
    server = server
        .with_worker(WorkerKind::Delivery, move |mut context| async move {
            let response = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(1))
                .build()
                .unwrap()
                .get(format!("http://{address}/synthetic"))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 503);
            context.progress(Progress::ExternalDegraded);
            context.cancelled().await;
            Ok(())
        })
        .unwrap();
    let services = server.services.clone();
    let listener = server.bind().await.unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(server.serve(listener, async {
        let _ = rx.await;
    }));
    wait_health(&services, |s| s.ready).await;
    assert_eq!(
        services.health.snapshot().degraded_workers,
        [WorkerKind::Delivery]
    );
    assert!(!serving.is_finished());
    peer_task.await.unwrap();
    tx.send(()).unwrap();
    serving.await.unwrap().unwrap();
}

#[tokio::test]
async fn shutdown_deadline_cancels_a_nonresponsive_async_worker() {
    struct Witness(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for Witness {
        fn drop(&mut self) {
            let _ = self.0.take().unwrap().send(());
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let mut budgets = options();
    budgets.shutdown_timeout = Duration::from_millis(80);
    let server =
        Server::initialize_with_supervision(config(&dir.path().join("timeout.db")), budgets)
            .await
            .unwrap();
    let services = server.services.clone();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
    let server = server
        .with_worker(WorkerKind::Delivery, move |context| async move {
            let _witness = Witness(Some(dropped_tx));
            context.progress(Progress::Completed);
            entered_tx.send(()).unwrap();
            std::future::pending().await
        })
        .unwrap();
    let listener = server.bind().await.unwrap();
    let serving = tokio::spawn(server.serve(listener, async {
        let _ = entered_rx.await;
    }));
    let error = tokio::time::timeout(Duration::from_secs(1), serving)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(matches!(
        error,
        calendar_server::StartError::Supervision(CoreFault::ShutdownTimeout)
    ));
    tokio::time::timeout(Duration::from_secs(1), dropped_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(!services.health.snapshot().live);
    services.storage.close().await;
}

#[tokio::test]
async fn stalled_worker_recovers_only_after_real_progress_without_restarting() {
    let dir = tempfile::tempdir().unwrap();
    let mut budgets = options();
    budgets.stall_timeout = Duration::from_millis(200);
    let server = Server::initialize_with_supervision(
        config(&dir.path().join("resumed-progress.db")),
        budgets,
    )
    .await
    .unwrap();
    let services = server.services.clone();
    let worker_services = services.clone();
    let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
    let server = server
        .with_worker(WorkerKind::Planner, move |mut context| async move {
            let _: i64 = sqlx::query_scalar("SELECT count(*) FROM sources")
                .fetch_one(worker_services.storage.pool())
                .await
                .map_err(|_| WorkerFailure)?;
            context.progress(Progress::Completed);
            let _ = resume_rx.await;
            let _: i64 = sqlx::query_scalar("SELECT count(*) FROM sources")
                .fetch_one(worker_services.storage.pool())
                .await
                .map_err(|_| WorkerFailure)?;
            context.progress(Progress::Completed);
            context.cancelled().await;
            Ok(())
        })
        .unwrap();
    let listener = server.bind().await.unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(server.serve(listener, async {
        let _ = rx.await;
    }));
    wait_health(&services, |s| {
        s.fault == Some(CoreFault::Stalled(Component::Worker(WorkerKind::Planner)))
    })
    .await;
    assert!(!serving.is_finished());
    resume_tx.send(()).unwrap();
    wait_health(&services, |s| s.live && s.fault.is_none()).await;
    assert!(
        !services.health.snapshot().ready,
        "other production roles are still missing"
    );
    tx.send(()).unwrap();
    serving.await.unwrap().unwrap();
}

#[tokio::test]
async fn db_failure_and_manual_repair_change_health_without_stopping_the_core() {
    let dir = tempfile::tempdir().unwrap();
    let server = synthetic_workers(
        Server::initialize_with_supervision(config(&dir.path().join("failed-probe.db")), options())
            .await
            .unwrap(),
    );
    let services = server.services.clone();
    let listener = server.bind().await.unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(server.serve(listener, async {
        let _ = rx.await;
    }));
    wait_health(&services, |s| s.ready).await;
    sqlx::query("CREATE TRIGGER fail_probe BEFORE UPDATE ON recovery_control BEGIN SELECT RAISE(ABORT,'synthetic'); END").execute(services.storage.pool()).await.unwrap();
    wait_health(&services, |s| s.database == DatabaseState::Failed).await;
    assert!(!services.health.snapshot().live);
    assert!(services.ready());
    assert!(!serving.is_finished());
    // Only the test operator removes its injected fault; the supervisor never repairs data.
    sqlx::query("DROP TRIGGER fail_probe")
        .execute(services.storage.pool())
        .await
        .unwrap();
    wait_health(&services, |s| s.ready).await;
    tx.send(()).unwrap();
    serving.await.unwrap().unwrap();
}
