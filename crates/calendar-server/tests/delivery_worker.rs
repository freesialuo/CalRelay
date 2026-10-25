use calendar_application::{Clock, DeliveryEnvelope, DeliveryOutcome, PushOutput};
use calendar_server::{
    delivery_worker::{
        delivery_round, run_delivery_worker, DeliveryProgress, DeliveryRoundEnd,
        DeliveryWorkerLimits,
    },
    Config, Server, Services,
};
use std::{
    sync::{
        atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::Semaphore;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Time {
    utc: AtomicI64,
    monotonic: AtomicU64,
}
impl Time {
    fn new() -> Self {
        Self {
            utc: AtomicI64::new(1000),
            monotonic: AtomicU64::new(0),
        }
    }
}
impl Clock for Time {
    fn utc_now_ms(&self) -> i64 {
        self.utc.load(Ordering::SeqCst)
    }
    fn monotonic_now_ms(&self) -> u64 {
        self.monotonic.load(Ordering::SeqCst)
    }
}
fn limits(max: usize) -> DeliveryWorkerLimits {
    DeliveryWorkerLimits::new(max, Duration::from_secs(1), Duration::from_secs(60)).unwrap()
}
async fn service(path: &std::path::Path) -> Result<Arc<Services>> {
    Ok(Server::initialize(Config {
        bind: "127.0.0.1:0".parse()?,
        database: path.into(),
        max_request_bytes: 1024,
        max_response_bytes: 16384,
        request_timeout: Duration::from_secs(1),
    })
    .await?
    .services)
}
// Synthetic configuration is confined to tests; production must supply real guarded CRUD data.
async fn seed(services: &Services) -> Result {
    sqlx::raw_sql(r#"
    INSERT INTO sources(id,kind,revision,last_verified_at) VALUES ('s','test',1,1000);
    INSERT INTO calendars(id,source_id,external_id) VALUES ('c','s','main');
    INSERT INTO events(id,source_id,calendar_id,external_uid,revision,document_json,content_hash)
      VALUES ('e','s','c','a',1,'{"schema_version":"1.0","external_uid":"a","title":"Test","status":"confirmed","start":{"kind":"utc","value":"2026-11-02T17:00:00Z"},"duration":"PT1H"}','h');
    INSERT INTO outputs(id,kind) VALUES ('o','test');
    INSERT INTO reminder_rules(id,policy_json) VALUES ('r','{"calendar_ids":["c"]}');
    INSERT INTO rule_outputs VALUES ('r','o');
    INSERT INTO planning_watermarks(scope_id,source_id,rule_id,output_id,source_revision,rule_revision,output_revision,planned_until,tzdata_version)
      VALUES ('w','s','r','o',1,1,1,20000,'2025b');
    "#).execute(services.storage.pool()).await?;
    for i in 0..3 {
        sqlx::query("INSERT INTO logical_deliveries(id,event_id,original_key,rule_id,output_id) VALUES (?,'e',?,'r','o')")
            .bind(format!("l{i}")).bind(format!("single{i}")).execute(services.storage.pool()).await?;
        sqlx::query("INSERT INTO delivery_jobs(id,logical_delivery_id,generation,event_revision,rule_revision,output_revision,due_at,state,next_attempt_at,planning_context) VALUES (?,?,1,1,1,1,1000,'pending',1000,'[null,\"2025b\"]')")
            .bind(format!("j{i}")).bind(format!("l{i}")).execute(services.storage.pool()).await?;
    }
    Ok(())
}
async fn states(services: &Services) -> Result<Vec<String>> {
    Ok(
        sqlx::query_scalar("SELECT state FROM delivery_jobs ORDER BY id")
            .fetch_all(services.storage.pool())
            .await?,
    )
}
struct Accepted;
impl PushOutput for Accepted {
    async fn deliver(&self, _: DeliveryEnvelope) -> DeliveryOutcome {
        DeliveryOutcome::Accepted
    }
}

#[tokio::test]
async fn bounded_round_and_reopen_preserve_accepted_dedup() -> Result {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db");
    let services = service(&path).await?;
    seed(&services).await?;
    let clock = Time::new();
    let mut progress = vec![];
    let first = delivery_round(&services, &Accepted, "worker", limits(1), &clock, |p| {
        progress.push(p)
    })
    .await?;
    assert_eq!(
        (first.processed, first.end),
        (1, DeliveryRoundEnd::OperationLimit)
    );
    assert_eq!(progress, [DeliveryProgress::Processed]);
    assert_eq!(states(&services).await?, ["accepted", "pending", "pending"]);
    services.drain().await;
    let services = service(&path).await?;
    let round =
        delivery_round(&services, &Accepted, "restarted", limits(4), &clock, |_| {}).await?;
    assert_eq!((round.processed, round.end), (2, DeliveryRoundEnd::Empty));
    let round =
        delivery_round(&services, &Accepted, "restarted", limits(4), &clock, |_| {}).await?;
    assert_eq!(round.processed, 0);
    let attempts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM delivery_attempts")
        .fetch_one(services.storage.pool())
        .await?;
    assert_eq!(attempts, 3);
    assert_eq!(
        states(&services).await?,
        ["accepted", "accepted", "accepted"]
    );
    services.drain().await;
    Ok(())
}

#[tokio::test]
async fn round_budget_uses_monotonic_time_and_finishes_the_current_send() -> Result {
    let dir = tempfile::tempdir()?;
    let services = service(&dir.path().join("db")).await?;
    seed(&services).await?;
    let clock = Time::new();
    let round = delivery_round(&services, &Accepted, "worker", limits(5), &clock, |_| {
        clock.utc.store(-100_000, Ordering::SeqCst);
        clock.monotonic.store(1000, Ordering::SeqCst);
    })
    .await?;
    assert_eq!(
        (round.processed, round.end),
        (1, DeliveryRoundEnd::Deadline)
    );
    assert_eq!(states(&services).await?, ["accepted", "pending", "pending"]);
    services.drain().await;
    Ok(())
}

struct GatedOutput {
    entered: Semaphore,
    release: Semaphore,
}

#[tokio::test]
async fn worker_runs_multiple_bounded_rounds_and_stops_between_claims() -> Result {
    let dir = tempfile::tempdir()?;
    let services = service(&dir.path().join("db")).await?;
    seed(&services).await?;
    let limits = DeliveryWorkerLimits::new(1, Duration::from_secs(1), Duration::from_millis(1))?;
    let mut processed = 0;
    tokio::time::timeout(
        Duration::from_secs(2),
        run_delivery_worker(&services, &Accepted, "worker", limits, &Time::new(), |p| {
            assert_eq!(p, DeliveryProgress::Processed);
            processed += 1;
            if processed == 2 {
                services.stop();
            }
        }),
    )
    .await??;
    assert_eq!(processed, 2);
    assert_eq!(
        states(&services).await?,
        ["accepted", "accepted", "pending"]
    );
    services.drain().await;
    Ok(())
}
impl PushOutput for GatedOutput {
    async fn deliver(&self, _: DeliveryEnvelope) -> DeliveryOutcome {
        self.entered.add_permits(1);
        self.release.acquire().await.unwrap().forget();
        DeliveryOutcome::Accepted
    }
}
#[tokio::test]
async fn stopping_drains_inflight_finish_without_admitting_another_job() -> Result {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db");
    let services = service(&path).await?;
    seed(&services).await?;
    let output = Arc::new(GatedOutput {
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
    });
    let worker_services = services.clone();
    let worker_output = output.clone();
    let worker = tokio::spawn(async move {
        run_delivery_worker(
            &worker_services,
            &*worker_output,
            "worker",
            limits(5),
            &Time::new(),
            |_| {},
        )
        .await
    });
    output.entered.acquire().await?.forget();
    services.stop();
    let drain_services = services.clone();
    let drain = tokio::spawn(async move { drain_services.drain().await });
    assert!(
        !drain.is_finished(),
        "drain must wait for the in-flight operation"
    );
    output.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(2), worker).await???;
    tokio::time::timeout(Duration::from_secs(2), drain).await??;
    let services = service(&path).await?;
    assert_eq!(states(&services).await?, ["accepted", "pending", "pending"]);
    services.drain().await;
    Ok(())
}

#[tokio::test]
async fn idle_stop_is_prompt_and_reports_only_the_real_empty_scan() -> Result {
    let dir = tempfile::tempdir()?;
    let services = service(&dir.path().join("db")).await?;
    let progress = Arc::new(AtomicUsize::new(0));
    let scanned = Arc::new(Semaphore::new(0));
    let worker_services = services.clone();
    let worker_progress = progress.clone();
    let worker_scanned = scanned.clone();
    let worker = tokio::spawn(async move {
        run_delivery_worker(
            &worker_services,
            &Accepted,
            "worker",
            limits(1),
            &Time::new(),
            |p| {
                assert_eq!(p, DeliveryProgress::Empty);
                worker_progress.fetch_add(1, Ordering::SeqCst);
                worker_scanned.add_permits(1);
            },
        )
        .await
    });
    scanned.acquire().await?.forget();
    services.stop();
    tokio::time::timeout(Duration::from_secs(2), worker).await???;
    assert_eq!(progress.load(Ordering::SeqCst), 1);
    services.drain().await;
    Ok(())
}

#[tokio::test]
async fn invalid_configuration_and_storage_errors_do_not_report_progress() -> Result {
    assert!(DeliveryWorkerLimits::new(0, Duration::from_secs(1), Duration::from_secs(1)).is_err());
    assert!(
        DeliveryWorkerLimits::new(257, Duration::from_secs(1), Duration::from_secs(1)).is_err()
    );
    assert!(DeliveryWorkerLimits::new(1, Duration::ZERO, Duration::from_secs(1)).is_err());
    assert!(DeliveryWorkerLimits::new(1, Duration::from_secs(1), Duration::ZERO).is_err());
    let dir = tempfile::tempdir()?;
    let services = service(&dir.path().join("db")).await?;
    services.storage.close().await;
    let result = run_delivery_worker(
        &services,
        &Accepted,
        "worker",
        limits(1),
        &Time::new(),
        |_| panic!("failed storage is not progress"),
    )
    .await;
    assert!(matches!(
        result,
        Err(calendar_application::DeliveryError::Database)
    ));
    services.drain().await;
    Ok(())
}

#[tokio::test]
async fn restore_pause_and_cutoff_survive_worker_rounds_and_restart() -> Result {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db");
    let services = service(&path).await?;
    seed(&services).await?;
    sqlx::query("UPDATE delivery_jobs SET due_at=999 WHERE id='j0'")
        .execute(services.storage.pool())
        .await?;
    let snapshot = dir.path().join("snapshot");
    calendar_storage::backup::backup(&path, &snapshot).await?;
    services.drain().await;
    let recovery = dir.path().join("recovery");
    calendar_storage::backup::restore(&snapshot, &recovery, 1000).await?;
    let path = recovery.join("database.sqlite3");
    let services = service(&path).await?;
    for _ in 0..2 {
        let round = delivery_round(
            &services,
            &Accepted,
            "worker",
            limits(3),
            &Time::new(),
            |progress| assert_eq!(progress, DeliveryProgress::RecoveryPaused),
        )
        .await?;
        assert_eq!(
            (round.processed, round.end),
            (0, DeliveryRoundEnd::RecoveryPaused)
        );
    }
    services.drain().await;
    let services = service(&path).await?;
    let control: (i64, i64) = sqlx::query_as("SELECT paused,cutoff FROM recovery_control")
        .fetch_one(services.storage.pool())
        .await?;
    assert_eq!(control, (1, 1000));
    services.drain().await;
    // Explicit offline operator resume; historical jobs remain suppressed.
    calendar_storage::backup::resume(&path).await?;
    let services = service(&path).await?;
    let clock = Time::new();
    let round = delivery_round(&services, &Accepted, "worker", limits(3), &clock, |_| {}).await?;
    assert_eq!(round.processed, 2);
    assert_eq!(
        states(&services).await?,
        ["expired", "accepted", "accepted"]
    );
    let attempts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM delivery_attempts")
        .fetch_one(services.storage.pool())
        .await?;
    assert_eq!(attempts, 2);
    services.drain().await;
    Ok(())
}

#[tokio::test]
async fn cancellation_during_send_records_acceptance_without_reviving_the_job() -> Result {
    let dir = tempfile::tempdir()?;
    let services = service(&dir.path().join("db")).await?;
    seed(&services).await?;
    let output = GatedOutput {
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
    };
    let clock = Time::new();
    let delivery = delivery_round(&services, &output, "worker", limits(1), &clock, |_| {});
    let cancel = async {
        output.entered.acquire().await.unwrap().forget();
        sqlx::query("UPDATE delivery_jobs SET state='cancelled' WHERE id='j0'")
            .execute(services.storage.pool())
            .await
            .unwrap();
        output.release.add_permits(1);
    };
    let (round, ()) = tokio::join!(delivery, cancel);
    assert_eq!(round?.processed, 1);
    assert_eq!(
        states(&services).await?,
        ["cancelled", "pending", "pending"]
    );
    let audit: (String, String) =
        sqlx::query_as("SELECT outcome,disposition FROM delivery_attempts")
            .fetch_one(services.storage.pool())
            .await?;
    assert_eq!(audit, ("accepted".into(), "cancelled".into()));
    let accepted: Option<i64> =
        sqlx::query_scalar("SELECT accepted_at FROM logical_deliveries WHERE id='l0'")
            .fetch_one(services.storage.pool())
            .await?;
    assert_eq!(accepted, Some(1000));
    services.drain().await;
    Ok(())
}

// A test-only HTTP output: the actual Bark adapter's TLS/SSRF/response matrix is tested
// in calendar-adapters::bark_tests. No loopback exception enters production adapters.
struct HttpOutput(std::net::SocketAddr);
impl PushOutput for HttpOutput {
    async fn deliver(&self, envelope: DeliveryEnvelope) -> DeliveryOutcome {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut connection = tokio::net::TcpStream::connect(self.0).await.unwrap();
        let body = serde_json::json!({"job_id": envelope.job_id}).to_string();
        let request = format!("POST /push HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", body.len(), body);
        connection.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        connection.read_to_string(&mut response).await.unwrap();
        if response.starts_with("HTTP/1.1 200") {
            DeliveryOutcome::Accepted
        } else {
            DeliveryOutcome::Retryable {
                retry_after_ms: Some(60_000),
            }
        }
    }
}

#[tokio::test]
async fn external_http_failure_is_persisted_and_other_due_jobs_continue() -> Result {
    let dir = tempfile::tempdir()?;
    let services = service(&dir.path().join("db")).await?;
    seed(&services).await?;
    let requests = Arc::new(AtomicUsize::new(0));
    let observed = requests.clone();
    let app = axum::Router::new().route(
        "/push",
        axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            let number = observed.fetch_add(1, Ordering::SeqCst);
            async move {
                assert_eq!(body["job_id"], format!("j{number}"));
                if number == 0 {
                    axum::http::StatusCode::SERVICE_UNAVAILABLE
                } else {
                    axum::http::StatusCode::OK
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let output = HttpOutput(listener.local_addr()?);
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
    });
    let clock = Time::new();
    let round = delivery_round(&services, &output, "worker", limits(3), &clock, |_| {}).await?;
    assert_eq!(
        (round.processed, round.end),
        (3, DeliveryRoundEnd::OperationLimit)
    );
    assert_eq!(
        states(&services).await?,
        ["retry_wait", "accepted", "accepted"]
    );
    let round = delivery_round(&services, &output, "worker", limits(3), &clock, |_| {}).await?;
    assert_eq!((round.processed, round.end), (0, DeliveryRoundEnd::Empty));
    assert_eq!(requests.load(Ordering::SeqCst), 3);
    let outcomes: Vec<String> =
        sqlx::query_scalar("SELECT outcome FROM delivery_attempts ORDER BY job_id")
            .fetch_all(services.storage.pool())
            .await?;
    assert_eq!(outcomes, ["retryable", "accepted", "accepted"]);
    let _ = stop.send(());
    server.await??;
    services.drain().await;
    Ok(())
}
