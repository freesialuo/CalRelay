#![cfg(all(unix, feature = "test-harness"))]
use calendar_application::{
    aggregate::AggregateConfiguration, configuration::*, Clock, CredentialProvider, DeletionPolicy,
    DeliveryEnvelope, DeliveryOutcome, EventRepository, PushOutput, RemoteFetchError,
    RemoteFetchOutcome, RemoteFetchRequest, RemoteFetcher, SourceRepository, SyncBatch, SyncLimits,
    SyncMode, SyncRecord,
};
use calendar_server::{
    source_worker::IcsSourceRunner,
    test_runtime::{ClockFrame, DatabaseMode, FileBusinessClock, HarnessRoot},
    Server,
};
use serde_json::json;
use std::{
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
struct Fixture {
    _dir: tempfile::TempDir,
    root: HarnessRoot,
    db: PathBuf,
    deployment: PathBuf,
    clock: PathBuf,
    now: i64,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("calrelay-system-")
            .tempdir()
            .unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = HarnessRoot::new(std::fs::canonicalize(dir.path()).unwrap()).unwrap();
        let key = root.path().join("key");
        std::fs::write(&key, [82u8; 32]).unwrap();
        let deployment = root.path().join("deployment.json");
        std::fs::write(&deployment,json!({"bind":"127.0.0.1:0","credentials":{"current_key_version":1,"key_files":{"1":key}}}).to_string()).unwrap();
        let now = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
            / 1000)
            * 1000
            + 86400000;
        let clock = root.path().join("clock.json");
        let db = root.path().join("db");
        let f = Self {
            _dir: dir,
            root,
            db,
            deployment,
            clock,
            now,
        };
        f.frame(now, 0);
        f
    }
    fn frame(&self, utc: i64, offset: u64) {
        let next = self.root.path().join("clock.next");
        std::fs::write(
            &next,
            serde_json::to_vec(&ClockFrame {
                utc_now_ms: utc,
                monotonic_offset_ms: offset,
            })
            .unwrap(),
        )
        .unwrap();
        std::fs::rename(next, &self.clock).unwrap();
    }
    fn provision(&self) {
        let secret = self.root.path().join("secret");
        std::fs::write(&secret, b"synthetic_typed_harness_key").unwrap();
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_calendar-server"))
            .env_clear()
            .env("CALRELAY_DATABASE", &self.db)
            .args([
                "credential",
                "provision-bark",
                "--deployment-config",
                self.deployment.to_str().unwrap(),
                "--credential-id",
                "bark-credential",
                "--secret-file",
                secret.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!String::from_utf8_lossy(&out.stdout).contains("synthetic_typed_harness_key"));
    }
}
fn guarded(server: Server) -> Server {
    use calendar_adapters::admin_auth::{AdminConfig, CloudflareVerifier};
    let verifier = CloudflareVerifier::new(
        AdminConfig {
            team: "synthetic-team".into(),
            audience: "synthetic-aud".into(),
            origin: "https://localhost:4443".into(),
            hostname: "localhost".into(),
            action: "admin-login".into(),
            administrators: [("synthetic-admin".into(), "admin@example.test".into())].into(),
            session_seconds: 120,
        },
        calendar_application::SecretBytes::new(b"synthetic-guard-secret".to_vec()),
    )
    .unwrap();
    server
        .with_admin_site_key(Arc::new(verifier), "synthetic-site".into())
        .unwrap()
}
struct NotModified(Arc<AtomicUsize>);
impl RemoteFetcher for NotModified {
    async fn fetch(
        &self,
        request: RemoteFetchRequest,
    ) -> Result<RemoteFetchOutcome, RemoteFetchError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(RemoteFetchOutcome::NotModified(request.validators))
    }
}
struct Accepted {
    provider: calendar_server::credentials_bootstrap::RuntimeCredentialProvider,
    calls: Arc<AtomicUsize>,
}
impl PushOutput for Accepted {
    async fn deliver(&self, _: DeliveryEnvelope) -> DeliveryOutcome {
        assert_eq!(
            self.provider
                .credential("bark-credential", "bark")
                .await
                .unwrap()
                .expose(),
            b"synthetic_typed_harness_key"
        );
        self.calls.fetch_add(1, Ordering::SeqCst);
        DeliveryOutcome::Accepted
    }
}
fn install(server: Server, f: &Fixture, fetch: Arc<AtomicUsize>, push: Arc<AtomicUsize>) -> Server {
    let output = Accepted {
        provider: server.services.credential_provider().unwrap(),
        calls: push,
    };
    let runner = IcsSourceRunner {
        fetcher: Arc::new(NotModified(fetch)),
        ics_limits: server.services.ics_limits,
        sync_limits: SyncLimits::default(),
    };
    guarded(server)
        .with_test_runtime_ports_and_clock(
            runner,
            output,
            Arc::new(FileBusinessClock::load(&f.root, &f.clock).unwrap()),
        )
        .unwrap()
}
async fn seed(server: &Server, f: &Fixture) -> (String, String) {
    let db = &server.services.storage;
    let mut source: SourceConfigurationV1 = serde_json::from_str(include_str!(
        "../../../examples/source-configuration-v1.json"
    ))
    .unwrap();
    source.mode = SourceMode::Pull;
    source.endpoint = Some("https://example.invalid/calendar.ics".into());
    source.refresh_interval_ms = Some(60000);
    source.freshness_sla_ms = Some(60000);
    source.capabilities.conditional_read = true;
    db.put_source_configuration(
        "source-a",
        ConfigurationCondition::Absent,
        source.clone(),
        "test",
        f.now,
    )
    .await
    .unwrap();
    source.enabled = true;
    db.put_source_configuration(
        "source-a",
        ConfigurationCondition::Revision(1),
        source,
        "test",
        f.now,
    )
    .await
    .unwrap();
    let start = chrono::DateTime::from_timestamp_millis(f.now)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let event=serde_json::from_value(json!({"schema_version":"1.0","external_uid":"one","title":"Typed test event","status":"confirmed","start":{"kind":"utc","value":start}})).unwrap();
    db.commit_sync(SyncBatch {
        source_id: "source-a".into(),
        base_revision: db.source_state("source-a").await.unwrap().revision,
        mode: SyncMode::FullSnapshot {
            complete: true,
            deletion_policy: DeletionPolicy::Limit(10),
        },
        records: vec![SyncRecord::Upsert {
            calendar_id: "calendar-a".into(),
            event: Box::new(event),
        }],
        proposed_checkpoint: None,
        verified_at: f.now,
    })
    .await
    .unwrap();
    let mut output: BarkOutputConfigurationV1 = serde_json::from_str(include_str!(
        "../../../examples/bark-output-configuration-v1.json"
    ))
    .unwrap();
    db.put_bark_output_configuration(
        "bark-a",
        ConfigurationCondition::Absent,
        output.clone(),
        "test",
        f.now,
    )
    .await
    .unwrap();
    output.enabled = true;
    db.put_bark_output_configuration(
        "bark-a",
        ConfigurationCondition::Revision(1),
        output,
        "test",
        f.now,
    )
    .await
    .unwrap();
    let mut rule: RuleConfigurationV1 =
        serde_json::from_str(include_str!("../../../examples/rule-configuration-v1.json")).unwrap();
    rule.policy.lead_ms = 0;
    db.put_rule_configuration(
        "rule-a",
        ConfigurationCondition::Absent,
        rule.clone(),
        "test",
        f.now,
    )
    .await
    .unwrap();
    rule.enabled = true;
    db.put_rule_configuration(
        "rule-a",
        ConfigurationCondition::Revision(1),
        rule,
        "test",
        f.now,
    )
    .await
    .unwrap();
    let mut aggregate: AggregateConfiguration = serde_json::from_str(include_str!(
        "../../../examples/aggregate-subscription-v1.json"
    ))
    .unwrap();
    let (record, secret) = db
        .put_aggregate(
            "aggregate",
            ConfigurationCondition::Absent,
            aggregate.clone(),
            "test",
            f.now,
        )
        .await
        .unwrap();
    aggregate.enabled = true;
    db.put_aggregate(
        "aggregate",
        ConfigurationCondition::Revision(1),
        aggregate,
        "test",
        f.now,
    )
    .await
    .unwrap();
    let aggregate = format!(
        "/feeds/aggregates/{}/{}.ics",
        record.public_id,
        std::str::from_utf8(secret.unwrap().expose()).unwrap()
    );
    let secret = db
        .issue_feed_token("feed", "public", "source-a", Some(f.now + 60000))
        .await
        .unwrap();
    let single = format!(
        "/feeds/public/{}.ics",
        std::str::from_utf8(secret.expose()).unwrap()
    );
    (single, aggregate)
}
async fn get(app: axum::Router, path: &str, tag: Option<&str>) -> axum::response::Response {
    use tower::ServiceExt;
    let mut r = axum::http::Request::builder().uri(path);
    if let Some(tag) = tag {
        r = r.header("if-none-match", tag);
    }
    app.oneshot(r.body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap()
}
async fn wait_for(services: &calendar_server::Services, predicate: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(8), async {
        while !predicate() {
            assert!(services.health.snapshot().live);
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn shared_clock_keeps_real_freshness_checks_and_lifecycle_preserves_pause_and_dedup() {
    let f = Fixture::new();
    let server = f
        .root
        .initialize(&f.db, &f.deployment, DatabaseMode::Fresh)
        .await
        .unwrap();
    f.provision();
    let (single, aggregate) = seed(&server, &f).await;
    // Aggregate freshness rejects a future source timestamp. Legacy single-feed policy
    // has no source-age gate; its capability expiry is still checked by the same clock.
    assert_eq!(get(server.router(), &aggregate, None).await.status(), 503);
    assert_eq!(get(server.router(), &single, None).await.status(), 200);
    let fetch = Arc::new(AtomicUsize::new(0));
    let push = Arc::new(AtomicUsize::new(0));
    let server = install(server, &f, fetch.clone(), push.clone());
    let services = server.services.clone();
    let app = server.router();
    assert_eq!(
        get(app.clone(), "/api/admin/v1/sources", None)
            .await
            .status(),
        403
    );
    let mut tags = vec![];
    for path in [&single, &aggregate] {
        let r = get(app.clone(), path, None).await;
        assert_eq!(r.status(), 200);
        tags.push(r.headers()["etag"].to_str().unwrap().to_owned());
    }
    // The same business UTC drives feed authorization, including cached 304 paths.
    f.frame(f.now + 120001, 120001);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        get(app.clone(), &single, Some(&tags[0])).await.status(),
        404
    );
    assert_eq!(
        get(app.clone(), &aggregate, Some(&tags[1])).await.status(),
        503
    );
    f.frame(f.now - 1000, 120001);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(get(app.clone(), &aggregate, None).await.status(), 503);
    assert_eq!(get(app.clone(), &single, None).await.status(), 200);
    f.frame(f.now, 120001);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let listener = server.bind().await.unwrap();
    let running = tokio::spawn(server.serve(listener, std::future::pending()));
    wait_for(&services, || {
        push.load(Ordering::SeqCst) == 1 && services.health.snapshot().ready
    })
    .await;
    f.frame(f.now + 30000, 180001);
    wait_for(&services, || fetch.load(Ordering::SeqCst) > 0).await;
    tokio::time::timeout(Duration::from_secs(8), async {
        while services
            .storage
            .source_state("source-a")
            .await
            .unwrap()
            .last_verified_at
            != Some(f.now + 30000)
        {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    for path in [&single, &aggregate] {
        assert_eq!(get(app.clone(), path, None).await.status(), 200);
    }
    let watermarks: Vec<(String, i64)> = sqlx::query_as(
        "SELECT source_id,planned_until FROM planning_watermarks ORDER BY source_id",
    )
    .fetch_all(services.storage.pool())
    .await
    .unwrap();
    services.stop();
    running.await.unwrap().unwrap();
    drop(app);
    assert!(f
        .root
        .initialize(&f.db, &f.deployment, DatabaseMode::default())
        .await
        .is_err());
    assert!(f
        .root
        .initialize(&f.db, &f.deployment, DatabaseMode::Restored)
        .await
        .is_err());
    let reopened = f
        .root
        .initialize(&f.db, &f.deployment, DatabaseMode::Reopen)
        .await
        .unwrap();
    assert_eq!(
        reopened
            .services
            .credential_provider()
            .unwrap()
            .credential("bark-credential", "bark")
            .await
            .unwrap()
            .expose(),
        b"synthetic_typed_harness_key"
    );
    let accepted: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM logical_deliveries WHERE accepted_at IS NOT NULL")
            .fetch_one(reopened.services.storage.pool())
            .await
            .unwrap();
    assert_eq!(accepted, 1);
    reopened.services.drain().await;
    let snapshot = f.root.path().join("snapshot");
    f.root.backup(&f.db, &snapshot).await.unwrap();
    let restored = f
        .root
        .restore(&snapshot, &f.root.path().join("restored"), f.now + 30000)
        .await
        .unwrap();
    let server = f
        .root
        .initialize(&restored, &f.deployment, DatabaseMode::Restored)
        .await
        .unwrap();
    assert_eq!(
        server
            .services
            .credential_provider()
            .unwrap()
            .credential("bark-credential", "bark")
            .await
            .unwrap()
            .expose(),
        b"synthetic_typed_harness_key"
    );
    let fetch = Arc::new(AtomicUsize::new(0));
    let push = Arc::new(AtomicUsize::new(0));
    let server = install(server, &f, fetch.clone(), push.clone());
    let services = server.services.clone();
    f.frame(f.now + 600000, 600000);
    let listener = server.bind().await.unwrap();
    let running = tokio::spawn(server.serve(listener, std::future::pending()));
    wait_for(&services, || {
        let h = services.health.snapshot();
        h.pending_workers.is_empty()
            && h.missing_workers.is_empty()
            && matches!(
                h.database,
                calendar_server::supervision::DatabaseState::Ready {
                    recovery_paused: true
                }
            )
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let health = services.health.snapshot();
    assert!(health.live);
    assert!(!health.ready);
    assert!(services.ready());
    assert_eq!(fetch.load(Ordering::SeqCst), 0);
    assert_eq!(push.load(Ordering::SeqCst), 0);
    let control: (bool, Option<i64>) = sqlx::query_as("SELECT paused,cutoff FROM recovery_control")
        .fetch_one(services.storage.pool())
        .await
        .unwrap();
    assert_eq!(control, (true, Some(f.now + 30000)));
    let after: Vec<(String, i64)> = sqlx::query_as(
        "SELECT source_id,planned_until FROM planning_watermarks ORDER BY source_id",
    )
    .fetch_all(services.storage.pool())
    .await
    .unwrap();
    assert_eq!(after, watermarks);
    let attempts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM delivery_attempts")
        .fetch_one(services.storage.pool())
        .await
        .unwrap();
    assert_eq!(attempts, 1);
    services.stop();
    running.await.unwrap().unwrap();
}

#[test]
fn controlled_clock_has_real_elapsed_floor_and_rejects_monotonic_rewind() {
    let f = Fixture::new();
    let clock = FileBusinessClock::load(&f.root, &f.clock).unwrap();
    assert_eq!(clock.utc_now_ms(), f.now);
    std::thread::sleep(Duration::from_millis(20));
    let first = clock.monotonic_now_ms();
    assert!(first >= 10);
    f.frame(f.now - 60000, 60000);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(clock.utc_now_ms(), f.now - 60000);
    assert!(clock.monotonic_now_ms() >= first + 60000);
    f.frame(f.now, 0);
    std::thread::sleep(Duration::from_millis(20));
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| clock.monotonic_now_ms()))
            .is_err()
    );
}
#[test]
fn controlled_root_rejects_shared_directory_permissions() {
    let f = Fixture::new();
    std::fs::set_permissions(f.root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(HarnessRoot::new(f.root.path().into()).is_err());
    std::fs::set_permissions(f.root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(HarnessRoot::new(f.root.path().into()).is_ok());
}
#[tokio::test]
async fn non_utf8_database_paths_never_touch_the_original_or_lossy_target() {
    use std::os::unix::ffi::OsStringExt;
    let f = Fixture::new();
    let database = f
        .root
        .path()
        .join(std::ffi::OsString::from_vec(b"database-\xff".to_vec()));
    let lossy = PathBuf::from(database.to_string_lossy().into_owned());
    std::fs::write(&lossy, b"existing lossy target").unwrap();
    assert!(f
        .root
        .initialize(&database, &f.deployment, DatabaseMode::Fresh)
        .await
        .is_err());
    assert!(!database.exists());
    assert_eq!(std::fs::read(&lossy).unwrap(), b"existing lossy target");
    std::fs::write(&database, b"existing original target").unwrap();
    for mode in [DatabaseMode::Reopen, DatabaseMode::Restored] {
        assert!(f
            .root
            .initialize(&database, &f.deployment, mode)
            .await
            .is_err());
        assert_eq!(
            std::fs::read(&database).unwrap(),
            b"existing original target"
        );
        assert_eq!(std::fs::read(&lossy).unwrap(), b"existing lossy target");
    }
}
#[tokio::test]
async fn bad_keys_paths_and_modes_do_not_create_or_overwrite_a_database() {
    let f = Fixture::new();
    let missing = f.root.path().join("missing");
    assert!(f
        .root
        .initialize(&missing, &f.deployment, DatabaseMode::Reopen)
        .await
        .is_err());
    assert!(!missing.exists());
    let key = f.root.path().join("key");
    std::fs::write(&key, [0u8; 31]).unwrap();
    assert!(f
        .root
        .initialize(&f.db, &f.deployment, DatabaseMode::Fresh)
        .await
        .is_err());
    assert!(!f.db.exists());
    let outside = f.root.path().join("../outside");
    assert!(f
        .root
        .initialize(&outside, &f.deployment, DatabaseMode::Fresh)
        .await
        .is_err());
    assert!(!f.db.exists());
    std::fs::write(&key, [82u8; 32]).unwrap();
    std::fs::write(&f.db, b"not a sqlite database").unwrap();
    assert!(f
        .root
        .initialize(&f.db, &f.deployment, DatabaseMode::Reopen)
        .await
        .is_err());
    assert_eq!(std::fs::read(&f.db).unwrap(), b"not a sqlite database");
    assert!(f
        .root
        .initialize(&f.db, &f.deployment, DatabaseMode::Fresh)
        .await
        .is_err());
    let linked = f.root.path().join("linked");
    std::os::unix::fs::symlink(&key, &linked).unwrap();
    assert!(FileBusinessClock::load(&f.root, &linked).is_err());
    for frame in [
        json!({"utc_now_ms":f.now,"monotonic_offset_ms":31536000001_u64}),
        json!({"utc_now_ms":f.now}),
        json!({"utc_now_ms":-1,"monotonic_offset_ms":0}),
    ] {
        std::fs::write(&f.clock, frame.to_string()).unwrap();
        assert!(FileBusinessClock::load(&f.root, &f.clock).is_err());
    }
}
