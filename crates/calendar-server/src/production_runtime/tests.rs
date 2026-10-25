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
    let path = dir.path().join("deployment.json");
    std::fs::write(&path, document.to_string()).unwrap();
    let secret = dir.path().join("bark-secret");
    std::fs::write(&secret, b"synthetic-runtime-bark").unwrap();
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
    source.endpoint = Some("https://calendar.example.test/source.ics".into());
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
async fn absent_security_is_rejected_before_database_and_library_install_is_fail_closed() {
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
    assert!(!services.health.snapshot().ready);
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
