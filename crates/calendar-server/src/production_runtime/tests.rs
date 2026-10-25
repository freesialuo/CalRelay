use super::*;
use calendar_application::{configuration::*, CredentialProvider, SourceRepository};
use std::sync::atomic::{AtomicI64, AtomicU64};
struct Time {
    utc: AtomicI64,
    mono: AtomicU64,
}
const NOW: i64 = 1_793_638_800_000;
impl Time {
    fn new() -> Self {
        Self {
            utc: AtomicI64::new(NOW),
            mono: AtomicU64::new(0),
        }
    }
}
impl Clock for Time {
    fn utc_now_ms(&self) -> i64 {
        self.utc.load(Ordering::SeqCst)
    }
    fn monotonic_now_ms(&self) -> u64 {
        self.mono.load(Ordering::SeqCst)
    }
}
struct Fixture {
    dir: tempfile::TempDir,
    server: Server,
}
async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let document = test_support::deployment(dir.path(), "127.0.0.1:0");
    let db = dir.path().join("db");
    let config = || {
        DeploymentConfig::from_deployment(document.to_string().as_bytes(), |key| {
            (key == "CALRELAY_DATABASE").then(|| db.to_str().unwrap().to_owned())
        })
        .unwrap()
    };
    // Same controlled KeyRing and existing encrypted security API as the local CLI.
    let server = config()
        .initialize_with_supervision(supervision_budgets())
        .await
        .unwrap();
    let keys = config().credentials.unwrap().load().unwrap();
    calendar_storage::security::SecurityStore {
        storage: &server.services.storage,
        keys: &keys,
    }
    .create_credential(
        "bark-credential",
        "bark",
        &calendar_application::SecretBytes::new(b"synthetic-runtime-bark".to_vec()),
        NOW,
    )
    .await
    .unwrap();
    Fixture { dir, server }
}
async fn configure(services: &Services) {
    let mut source: SourceConfigurationV1 = serde_json::from_str(include_str!(
        "../../../../examples/source-configuration-v1.json"
    ))
    .unwrap();
    source.mode = SourceMode::Pull;
    source.endpoint = Some("https://127.0.0.1:9/source.ics".into());
    source.refresh_interval_ms = Some(60_000);
    source.freshness_sla_ms = Some(60_000);
    source.capabilities.conditional_read = true;
    services
        .storage
        .put_source_configuration(
            "source",
            ConfigurationCondition::Absent,
            source.clone(),
            "test",
            NOW,
        )
        .await
        .unwrap();
    source.enabled = true;
    services
        .storage
        .put_source_configuration(
            "source",
            ConfigurationCondition::Revision(1),
            source,
            "test",
            NOW,
        )
        .await
        .unwrap();
    let mut output: BarkOutputConfigurationV1 = serde_json::from_str(include_str!(
        "../../../../examples/bark-output-configuration-v1.json"
    ))
    .unwrap();
    // Numeric loopback is deterministically denied by the actual production Bark adapter.
    output.origin = "https://127.0.0.1:9".into();
    services
        .storage
        .put_bark_output_configuration(
            "bark-a",
            ConfigurationCondition::Absent,
            output.clone(),
            "test",
            NOW,
        )
        .await
        .unwrap();
    output.enabled = true;
    services
        .storage
        .put_bark_output_configuration(
            "bark-a",
            ConfigurationCondition::Revision(1),
            output,
            "test",
            NOW,
        )
        .await
        .unwrap();
    let mut rule: RuleConfigurationV1 = serde_json::from_str(include_str!(
        "../../../../examples/rule-configuration-v1.json"
    ))
    .unwrap();
    rule.policy.lead_ms = 0;
    services
        .storage
        .put_rule_configuration(
            "rule",
            ConfigurationCondition::Absent,
            rule.clone(),
            "test",
            NOW,
        )
        .await
        .unwrap();
    rule.enabled = true;
    services
        .storage
        .put_rule_configuration(
            "rule",
            ConfigurationCondition::Revision(1),
            rule,
            "test",
            NOW,
        )
        .await
        .unwrap();
}
async fn sync(services: &Services) {
    use calendar_application::{EventRepository, SyncBatch, SyncMode, SyncRecord};
    let state = services.storage.source_state("source").await.unwrap();
    let event=serde_json::from_value(serde_json::json!({"schema_version":"1.0","external_uid":"one","title":"Runtime event","status":"confirmed","start":{"kind":"utc","value":"2026-11-02T17:00:00Z"}})).unwrap();
    services
        .storage
        .commit_sync(SyncBatch {
            source_id: "source".into(),
            base_revision: state.revision,
            mode: SyncMode::Delta,
            records: vec![SyncRecord::Upsert {
                calendar_id: "calendar-a".into(),
                event: Box::new(event),
            }],
            proposed_checkpoint: None,
            verified_at: NOW,
        })
        .await
        .unwrap();
}
async fn wait_ready(services: &Services, paused: bool) {
    let mut changes = services.health.subscribe();
    tokio::time::timeout(Duration::from_secs(5),async {
        loop {
            let s=services.health.snapshot();
            if s.pending_workers.is_empty() && s.missing_workers.is_empty()
                && matches!(s.database,crate::supervision::DatabaseState::Ready{recovery_paused} if recovery_paused==paused)
                && (paused || s.ready) { assert!(s.live); break; }
            changes.changed().await.unwrap();
        }
    }).await.unwrap();
}
#[tokio::test]
async fn actual_three_workers_become_ready_on_real_empty_discovery_and_stop() {
    let f = fixture().await;
    let server = f.server.with_production_runtime().unwrap();
    let services = server.services.clone();
    let listener = server.bind().await.unwrap();
    let running = tokio::spawn(server.serve(listener, std::future::pending()));
    wait_ready(&services, false).await;
    let snapshot = services.health.snapshot();
    assert!(snapshot.degraded_workers.is_empty());
    services.stop();
    tokio::time::timeout(Duration::from_secs(3), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let db = calendar_storage::Storage::open(f.dir.path().join("db"))
        .await
        .unwrap();
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM delivery_jobs")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(n, 0);
    db.close().await;
}
#[tokio::test]
async fn absent_security_is_rejected_before_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("absent");
    let full = test_support::deployment(dir.path(), "127.0.0.1:0");
    for missing in ["admin", "credentials"] {
        let mut doc = full.clone();
        doc.as_object_mut().unwrap().remove(missing);
        let config = DeploymentConfig::from_deployment(doc.to_string().as_bytes(), |k| {
            (k == "CALRELAY_DATABASE").then(|| db.to_str().unwrap().into())
        })
        .unwrap();
        assert!(initialize(config).await.is_err());
        assert!(!db.exists());
    }
}
#[tokio::test]
async fn planning_persists_window_restart_threshold_revisions_and_bark_generation_fence() {
    let f = fixture().await;
    let services = &f.server.services;
    configure(services).await;
    sync(services).await;
    let clock = Time::new();
    let mut planner = Planner::default();
    let mut progress = vec![];
    planner
        .round(services, &clock, |p| progress.push(p))
        .await
        .unwrap();
    assert!(progress.iter().all(|p| *p == Progress::Completed));
    let scopes = services
        .storage
        .planning_configurations(None)
        .await
        .unwrap();
    let req = request(scopes.items[0].clone());
    assert!(!services.storage.planning_required(&req, NOW).await.unwrap());
    assert!(services
        .storage
        .planning_required(&req, NOW + EXTENSION_MS)
        .await
        .unwrap());
    assert!(services
        .storage
        .planning_required(&req, NOW - 1)
        .await
        .unwrap());
    let mut revised = req.clone();
    revised.rule_generation += 1;
    assert!(services
        .storage
        .planning_required(&revised, NOW)
        .await
        .unwrap());
    revised = req.clone();
    revised.tzdata_version = "next".into();
    assert!(services
        .storage
        .planning_required(&revised, NOW)
        .await
        .unwrap());
    let provider = services.credential_provider().unwrap();
    assert_eq!(
        provider
            .credential("bark-credential", "bark")
            .await
            .unwrap()
            .expose(),
        b"synthetic-runtime-bark"
    );
    let dispatcher = ConfiguredBark::new(services.clone()).unwrap();
    let round = delivery_round(
        services,
        &dispatcher,
        "test",
        DeliveryWorkerLimits::new(1, ROUND, DELAY).unwrap(),
        &clock,
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(round.processed, 1);
    assert!(dispatcher.degraded.load(Ordering::SeqCst));
    let (state,accepted):(String,Option<i64>)=sqlx::query_as("SELECT j.state,l.accepted_at FROM delivery_jobs j JOIN logical_deliveries l ON l.id=j.logical_delivery_id")
        .fetch_one(services.storage.pool()).await.unwrap();
    assert_eq!(state, "failed_permanent");
    assert_eq!(accepted, None);
    // A restarted planner reads the committed watermark and creates no additional attempts/jobs.
    Planner::default()
        .round(services, &clock, |_| {})
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM delivery_jobs")
        .fetch_one(services.storage.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
    services.drain().await;
}
#[tokio::test]
async fn production_workers_respect_actual_restore_pause_and_cutoff_without_network() {
    let f = fixture().await;
    configure(&f.server.services).await;
    sync(&f.server.services).await;
    let path = f.dir.path().join("db");
    let snapshot = f.dir.path().join("snapshot");
    calendar_storage::backup::backup(&path, &snapshot)
        .await
        .unwrap();
    f.server.services.drain().await;
    let recovery = f.dir.path().join("recovery");
    calendar_storage::backup::restore(&snapshot, &recovery, NOW)
        .await
        .unwrap();
    let document = test_support::deployment(f.dir.path(), "127.0.0.1:0");
    let db = recovery.join("database.sqlite3");
    let config = DeploymentConfig::from_deployment(document.to_string().as_bytes(), |k| {
        (k == "CALRELAY_DATABASE").then(|| db.to_str().unwrap().into())
    })
    .unwrap();
    let server = initialize(config).await.unwrap();
    let services = server.services.clone();
    let listener = server.bind().await.unwrap();
    let running = tokio::spawn(server.serve(listener, std::future::pending()));
    wait_ready(&services, true).await;
    let health = services.health.snapshot();
    assert!(!health.ready);
    assert!(
        health.degraded_workers.is_empty(),
        "paused workers must not attempt a blocked endpoint"
    );
    assert!(services.ready());
    let control: (bool, i64) = sqlx::query_as("SELECT paused,cutoff FROM recovery_control")
        .fetch_one(services.storage.pool())
        .await
        .unwrap();
    assert_eq!(control, (true, NOW));
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM delivery_attempts")
        .fetch_one(services.storage.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    let verified: Option<i64> =
        sqlx::query_scalar("SELECT last_verified_at FROM sources WHERE id='source'")
            .fetch_one(services.storage.pool())
            .await
            .unwrap();
    assert_eq!(verified, Some(NOW));
    services.stop();
    tokio::time::timeout(Duration::from_secs(3), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn output_changed_after_prepare_is_not_dispatched() {
    use calendar_application::DeliveryRepository;
    let f = fixture().await;
    let s = &f.server.services;
    configure(s).await;
    sync(s).await;
    let clock = Time::new();
    Planner::default().round(s, &clock, |_| {}).await.unwrap();
    let lease = s
        .storage
        .claim("test", &s.delivery_policy, &clock)
        .await
        .unwrap()
        .unwrap();
    let envelope = s
        .storage
        .prepare(&lease, &s.delivery_policy, &clock)
        .await
        .unwrap()
        .unwrap();
    let output = s.storage.bark_output_configuration("bark-a").await.unwrap();
    let mut config = output.configuration;
    config.name = "Changed in flight".into();
    s.storage
        .put_bark_output_configuration(
            "bark-a",
            ConfigurationCondition::Revision(output.generation),
            config,
            "test",
            NOW,
        )
        .await
        .unwrap();
    assert!(s
        .storage
        .output_for_delivery(&envelope.job_id, &envelope.output_id)
        .await
        .unwrap()
        .is_none());
    let dispatcher = ConfiguredBark::new(s.clone()).unwrap();
    assert_eq!(
        dispatcher.deliver(envelope).await,
        DeliveryOutcome::PermanentFailure
    );
    assert!(
        !dispatcher.degraded.load(Ordering::SeqCst),
        "configuration invalidation is not remote failure"
    );
    s.storage
        .finish(
            &lease,
            DeliveryOutcome::PermanentFailure,
            &s.delivery_policy,
            &clock,
        )
        .await
        .unwrap();
    let accepted: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM logical_deliveries WHERE accepted_at IS NOT NULL")
            .fetch_one(s.storage.pool())
            .await
            .unwrap();
    assert_eq!(accepted, 0);
    s.drain().await;
}

#[tokio::test]
async fn planner_bounds_work_and_preserves_pending_scopes_between_rounds() {
    let f = fixture().await;
    let s = &f.server.services;
    configure(s).await;
    sync(s).await;
    let output = s.storage.bark_output_configuration("bark-a").await.unwrap();
    let mut ids = vec!["bark-a".to_owned()];
    for i in 0..8 {
        let id = format!("output-{i}");
        let mut config = output.configuration.clone();
        config.enabled = false;
        s.storage
            .put_bark_output_configuration(
                &id,
                ConfigurationCondition::Absent,
                config.clone(),
                "test",
                NOW,
            )
            .await
            .unwrap();
        config.enabled = true;
        s.storage
            .put_bark_output_configuration(
                &id,
                ConfigurationCondition::Revision(1),
                config,
                "test",
                NOW,
            )
            .await
            .unwrap();
        ids.push(id);
    }
    let rule = s.storage.rule_configuration("rule").await.unwrap();
    let mut config = rule.configuration;
    config.output_ids = ids;
    s.storage
        .put_rule_configuration(
            "rule",
            ConfigurationCondition::Revision(rule.generation),
            config,
            "test",
            NOW,
        )
        .await
        .unwrap();
    let clock = Time::new();
    let mut planner = Planner::default();
    // Discovery itself can consume the round. Its remaining page is retained without claims.
    planner
        .round(s, &clock, |_| {
            clock.mono.store(ROUND.as_millis() as u64, Ordering::SeqCst)
        })
        .await
        .unwrap();
    assert_eq!(planner.pending.len(), 9);
    planner.round(s, &clock, |_| {}).await.unwrap();
    assert_eq!(planner.pending.len(), 1);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM planning_watermarks")
        .fetch_one(s.storage.pool())
        .await
        .unwrap();
    assert_eq!(count, 8);
    planner.round(s, &clock, |_| {}).await.unwrap();
    assert!(planner.pending.is_empty());
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM planning_watermarks")
        .fetch_one(s.storage.pool())
        .await
        .unwrap();
    assert_eq!(count, 9);
    s.drain().await;
}

// Test-only network ports use a single loopback server. Production still constructs the
// existing HTTPS/SSRF-enforcing HttpFetcher and BarkOutput; their TLS matrices run separately.
struct MockFetcher {
    origin: String,
}
impl calendar_application::RemoteFetcher for MockFetcher {
    async fn fetch(
        &self,
        request: calendar_application::RemoteFetchRequest,
    ) -> Result<calendar_application::RemoteFetchOutcome, calendar_application::RemoteFetchError>
    {
        use calendar_application::{
            RemoteDocument, RemoteFetchError, RemoteFetchOutcome, RemoteValidators,
        };
        let mut get = reqwest::Client::new().get(format!("{}/ics", self.origin));
        if let Some(tag) = request.validators.etag {
            get = get.header("if-none-match", tag);
        }
        let response = get.send().await.map_err(|_| RemoteFetchError::Transport)?;
        if response.status().as_u16() == 304 {
            return Ok(RemoteFetchOutcome::NotModified(RemoteValidators {
                etag: Some("v1".into()),
                last_modified: None,
            }));
        }
        if response.status().as_u16() != 200 {
            return Err(RemoteFetchError::Server {
                status: response.status().as_u16(),
                retry_after_ms: None,
            });
        }
        Ok(RemoteFetchOutcome::Modified(RemoteDocument {
            credential_revision: None,
            final_url: request.url,
            body: response
                .bytes()
                .await
                .map_err(|_| RemoteFetchError::Transport)?
                .to_vec(),
            validators: RemoteValidators {
                etag: Some("v1".into()),
                last_modified: None,
            },
        }))
    }
}
struct MockPush {
    origin: String,
    credentials: crate::credentials_bootstrap::RuntimeCredentialProvider,
}
impl PushOutput for MockPush {
    async fn deliver(&self, envelope: DeliveryEnvelope) -> DeliveryOutcome {
        let secret = self
            .credentials
            .credential("bark-credential", "bark")
            .await
            .unwrap();
        let response=reqwest::Client::new().post(format!("{}/push",self.origin))
            .json(&serde_json::json!({"device_key":std::str::from_utf8(secret.expose()).unwrap(),"title":envelope.event.title})).send().await.unwrap();
        if response.status().is_success() {
            DeliveryOutcome::Accepted
        } else {
            DeliveryOutcome::Retryable {
                retry_after_ms: Some(1000),
            }
        }
    }
}
#[tokio::test]
async fn mock_http_ics_304_failure_and_bark_retry_acceptance_survive_real_sqlite_restart() {
    use axum::{
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::{get, post},
        Router,
    };
    #[derive(Default)]
    struct Counts {
        ics: std::sync::atomic::AtomicUsize,
        push: std::sync::atomic::AtomicUsize,
    }
    let counts = Arc::new(Counts::default());
    let router=Router::new().route("/ics",get(|State(c):State<Arc<Counts>>,headers:HeaderMap|async move {
        let n=c.ics.fetch_add(1,Ordering::SeqCst);
        if n==2 { return (StatusCode::SERVICE_UNAVAILABLE,""); }
        if headers.contains_key("if-none-match") { return (StatusCode::NOT_MODIFIED,""); }
        (StatusCode::OK,"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//Runtime//EN\r\nBEGIN:VEVENT\r\nUID:one\r\nDTSTAMP:20260101T000000Z\r\nDTSTART:20261102T170000Z\r\nSUMMARY:Runtime mock\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n")
    })).route("/push",post(|State(c):State<Arc<Counts>>,axum::Json(body):axum::Json<serde_json::Value>|async move {
        assert_eq!(body["device_key"],"synthetic-runtime-bark");assert_eq!(body["title"],"Runtime mock");
        if c.push.fetch_add(1,Ordering::SeqCst)==0 { StatusCode::SERVICE_UNAVAILABLE } else { StatusCode::OK }
    })).with_state(counts.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let http = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let f = fixture().await;
    let s = &f.server.services;
    configure(s).await;
    let clock = Time::new();
    let mut scanner = SourceScanner::new(
        SourceScanLimits::new(1, 1, 100, ROUND, DELAY, DELAY, Duration::from_secs(60)).unwrap(),
    );
    let runner = IcsSourceRunner {
        fetcher: Arc::new(MockFetcher {
            origin: origin.clone(),
        }),
        ics_limits: s.ics_limits,
        sync_limits: SyncLimits::default(),
    };
    scanner.round(s, &runner, &clock, |_| {}).await.unwrap();
    let revision = s.storage.source_state("source").await.unwrap().revision;
    let mut planner = Planner::default();
    planner.round(s, &clock, |_| {}).await.unwrap();
    clock.mono.store(60_000, Ordering::SeqCst);
    clock.utc.store(NOW + 60_000, Ordering::SeqCst);
    scanner.round(s, &runner, &clock, |_| {}).await.unwrap();
    assert_eq!(
        s.storage.source_state("source").await.unwrap().revision,
        revision
    );

    let output = MockPush {
        origin,
        credentials: s.credential_provider().unwrap(),
    };
    delivery_round(
        s,
        &output,
        "test",
        DeliveryWorkerLimits::new(1, ROUND, DELAY).unwrap(),
        &clock,
        |_| {},
    )
    .await
    .unwrap();
    let state: String = sqlx::query_scalar("SELECT state FROM delivery_jobs")
        .fetch_one(s.storage.pool())
        .await
        .unwrap();
    assert_eq!(state, "retry_wait");
    clock.mono.store(120_000, Ordering::SeqCst);
    clock.utc.store(NOW + 120_000, Ordering::SeqCst);
    scanner.round(s, &runner, &clock, |_| {}).await.unwrap();
    assert_eq!(
        s.storage.source_state("source").await.unwrap().revision,
        revision
    );
    delivery_round(
        s,
        &output,
        "test",
        DeliveryWorkerLimits::new(1, ROUND, DELAY).unwrap(),
        &clock,
        |_| {},
    )
    .await
    .unwrap();
    s.drain().await;
    let doc = test_support::deployment(f.dir.path(), "127.0.0.1:0");
    let db = f.dir.path().join("db");
    let server = DeploymentConfig::from_deployment(doc.to_string().as_bytes(), |k| {
        (k == "CALRELAY_DATABASE").then(|| db.to_str().unwrap().into())
    })
    .unwrap()
    .initialize()
    .await
    .unwrap();
    let s = &server.services;
    Planner::default().round(s, &clock, |_| {}).await.unwrap();
    let round = delivery_round(
        s,
        &output,
        "restarted",
        DeliveryWorkerLimits::new(1, ROUND, DELAY).unwrap(),
        &clock,
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(round.processed, 0);
    let rows: (i64, i64) =
        sqlx::query_as("SELECT COUNT(*),COUNT(accepted_at) FROM logical_deliveries")
            .fetch_one(s.storage.pool())
            .await
            .unwrap();
    assert_eq!(rows, (1, 1));
    assert_eq!(counts.ics.load(Ordering::SeqCst), 3);
    assert_eq!(counts.push.load(Ordering::SeqCst), 2);
    s.drain().await;
    stop.send(()).unwrap();
    http.await.unwrap();
}

#[tokio::test]
async fn concurrent_sqlite_writer_retains_planning_scope_for_retry() {
    let f = fixture().await;
    let s = &f.server.services;
    configure(s).await;
    sync(s).await;
    let tx = s
        .storage
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .unwrap();
    let mut planner = Planner::default();
    let clock = Time::new();
    planner.round(s, &clock, |_| {}).await.unwrap();
    assert_eq!(
        planner.pending.len(),
        1,
        "Busy must retain work instead of ending the worker"
    );
    tx.rollback().await.unwrap();
    planner.round(s, &clock, |_| {}).await.unwrap();
    assert!(planner.pending.is_empty());
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM planning_watermarks")
        .fetch_one(s.storage.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
    s.drain().await;
}
