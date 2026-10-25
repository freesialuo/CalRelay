use calendar_application::{Clock, IngestCondition, IngestOperation, IngestPolicy, IngestRequest};
use calendar_storage::{backup, ingest::IngestError, Storage};
use serde_json::json;
use std::sync::{
    atomic::{AtomicI64, Ordering},
    Arc,
};
use tempfile::tempdir;
struct Time(AtomicI64);
impl Time {
    fn new(n: i64) -> Self {
        Self(AtomicI64::new(n))
    }
    fn set(&self, n: i64) {
        self.0.store(n, Ordering::SeqCst);
    }
}
impl Clock for Time {
    fn utc_now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn monotonic_now_ms(&self) -> u64 {
        0
    }
}
fn request(key: &str) -> IngestRequest {
    IngestRequest{source_id:"s".into(),uid:"a".into(),key:key.into(),condition:IngestCondition::Absent,
        operation:IngestOperation::Upsert{calendar_id:"c".into(),event:Box::new(serde_json::from_value(json!({"schema_version":"1.0","external_uid":"a","title":"A","status":"confirmed","start":{"kind":"utc","value":"2026-11-02T17:00:00Z"}})).unwrap())}}
}
async fn seed(db: &Storage) -> Vec<u8> {
    sqlx::raw_sql("INSERT INTO sources(id,kind,committed_checkpoint) VALUES('s','api','retained'); INSERT INTO calendars(id,source_id,external_id) VALUES('c','s','main');").execute(db.pool()).await.unwrap();
    db.issue_api_token("token", "s", true, true, None)
        .await
        .unwrap()
        .expose()
        .to_vec()
}
#[tokio::test]
async fn retention_capacity_and_receipt_failure_are_atomic() {
    let dir = tempdir().unwrap();
    let db = Storage::open(dir.path().join("db")).await.unwrap();
    let t = seed(&db).await;
    let clock = Time::new(0);
    let policy = IngestPolicy {
        retention_ms: 60_000,
        max_records: 1,
    };
    sqlx::raw_sql("CREATE TRIGGER fail_receipt BEFORE INSERT ON ingest_requests BEGIN SELECT RAISE(ABORT,'injected'); END;").execute(db.pool()).await.unwrap();
    assert!(matches!(
        db.ingest(&t, request("first"), policy, &clock, "r1").await,
        Err(IngestError::Database)
    ));
    for query in [
        "SELECT count(*) FROM events",
        "SELECT count(*) FROM event_revisions",
        "SELECT count(*) FROM source_changes",
        "SELECT count(*) FROM ingest_requests",
        "SELECT revision FROM sources",
    ] {
        assert_eq!(
            sqlx::query_scalar::<_, i64>(query)
                .fetch_one(db.pool())
                .await
                .unwrap(),
            0
        );
    }
    sqlx::query("DROP TRIGGER fail_receipt")
        .execute(db.pool())
        .await
        .unwrap();
    let first = db
        .ingest(&t, request("first"), policy, &clock, "r1")
        .await
        .unwrap();
    assert_eq!(first.expires_at, 60_000);
    clock.set(59_999);
    let replay = db
        .ingest(&t, request("first"), policy, &clock, "r2")
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.request_id, "r1");
    assert_eq!(replay.expires_at, 60_000);
    assert!(matches!(
        db.ingest(&t, request("second"), policy, &clock, "r2").await,
        Err(IngestError::Capacity)
    ));
    clock.set(-1);
    assert!(
        db.ingest(&t, request("first"), policy, &clock, "r2")
            .await
            .unwrap()
            .replayed
    );
    clock.set(60_000);
    assert!(matches!(
        db.ingest(&t, request("first"), policy, &clock, "r3").await,
        Err(IngestError::Conflict)
    ));
    let mut next = request("first");
    next.condition = IngestCondition::Revision(1);
    let reused = db.ingest(&t, next, policy, &clock, "r3").await.unwrap();
    assert!(!reused.replayed);
    assert_eq!(reused.request_id, "r3");
    assert_eq!(reused.event_revision, 1);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT committed_checkpoint FROM sources")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        "retained"
    );
}
#[tokio::test]
async fn revocation_and_expiry_linearize_before_waiting_writer() {
    for revoke in [false, true] {
        let dir = tempdir().unwrap();
        let db = Arc::new(Storage::open(dir.path().join("db")).await.unwrap());
        let token = seed(&db).await;
        sqlx::query("UPDATE api_tokens SET expires_at=100")
            .execute(db.pool())
            .await
            .unwrap();
        let clock = Arc::new(Time::new(99));
        // A prior authentication is never accepted as the authority for the later commit.
        db.authenticate_api_token(
            &token,
            "s",
            calendar_storage::security::ApiAction::Upsert,
            99,
        )
        .await
        .unwrap();
        let mut tx = db.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        if revoke {
            sqlx::query("UPDATE api_tokens SET revoked_at=99")
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        let db2 = db.clone();
        let clock2 = clock.clone();
        let (started, waiting) = tokio::sync::oneshot::channel();
        let writer = tokio::spawn(async move {
            started.send(()).unwrap();
            db2.ingest(
                &token,
                request("key"),
                IngestPolicy::default(),
                &*clock2,
                "r",
            )
            .await
        });
        waiting.await.unwrap();
        if !revoke {
            clock.set(100);
        }
        tx.commit().await.unwrap();
        assert!(matches!(writer.await.unwrap(), Err(IngestError::Denied)));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM events")
                .fetch_one(db.pool())
                .await
                .unwrap(),
            0
        );
    }
}
#[tokio::test]
async fn v9_without_v7_upgrades_and_backup_restore_preserves_receipts() {
    let dir = tempdir().unwrap();
    let old_migrations = dir.path().join("migrations");
    std::fs::create_dir(&old_migrations).unwrap();
    for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations")).unwrap() {
        let entry = entry.unwrap();
        if !entry.file_name().to_str().unwrap().starts_with("0007") {
            std::fs::copy(entry.path(), old_migrations.join(entry.file_name())).unwrap();
        }
    }
    use sqlx::Connection;
    let path = dir.path().join("old");
    let mut conn = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::migrate::Migrator::new(old_migrations.as_path())
        .await
        .unwrap()
        .run(&mut conn)
        .await
        .unwrap();
    conn.close().await.unwrap();
    // The backup validator must accept known, checksum-correct non-contiguous history.
    let old_backup = dir.path().join("old-backup");
    backup::backup(&path, &old_backup).await.unwrap();
    let old_restore = dir.path().join("old-restore");
    backup::restore(&old_backup, &old_restore, 100)
        .await
        .unwrap();
    let db = Storage::open(&path).await.unwrap();
    let token = seed(&db).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM _sqlx_migrations WHERE version IN (7,9) AND success=1"
        )
        .fetch_one(db.pool())
        .await
        .unwrap(),
        2
    );
    db.ingest(
        &token,
        request("key"),
        IngestPolicy::default(),
        &Time::new(100),
        "first",
    )
    .await
    .unwrap();
    let b = dir.path().join("backup");
    backup::backup(&path, &b).await.unwrap();
    let r = dir.path().join("restore");
    backup::restore(&b, &r, 200).await.unwrap();
    let recovered = Storage::open(r.join("database.sqlite3")).await.unwrap();
    let result = recovered
        .ingest(
            &token,
            request("key"),
            IngestPolicy::default(),
            &Time::new(200),
            "retry",
        )
        .await
        .unwrap();
    assert!(result.replayed);
    assert_eq!(result.request_id, "first");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT paused FROM recovery_control")
            .fetch_one(recovered.pool())
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn new_unchanged_writes_verify_source_but_receipt_replays_do_not() {
    let dir = tempdir().unwrap();
    let db = Storage::open(dir.path().join("db")).await.unwrap();
    let token = seed(&db).await;
    let clock = Time::new(10);
    db.ingest(
        &token,
        request("create"),
        IngestPolicy::default(),
        &clock,
        "first",
    )
    .await
    .unwrap();
    clock.set(20);
    let mut no_op = request("new-key");
    no_op.condition = IngestCondition::Revision(1);
    let result = db
        .ingest(
            &token,
            no_op.clone(),
            IngestPolicy::default(),
            &clock,
            "second",
        )
        .await
        .unwrap();
    assert_eq!(result.event_revision, 1);
    assert_eq!(result.source_revision, 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT last_verified_at FROM sources")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        20
    );
    clock.set(30);
    assert!(
        db.ingest(&token, no_op, IngestPolicy::default(), &clock, "third")
            .await
            .unwrap()
            .replayed
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT last_verified_at FROM sources")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        20
    );
    for query in [
        "SELECT count(*) FROM event_revisions",
        "SELECT count(*) FROM source_changes",
    ] {
        assert_eq!(
            sqlx::query_scalar::<_, i64>(query)
                .fetch_one(db.pool())
                .await
                .unwrap(),
            1
        );
    }
}

#[tokio::test]
async fn managed_mode_and_capability_narrowing_precede_receipt_replay() {
    use calendar_application::configuration::*;
    let dir = tempdir().unwrap();
    let db = Storage::open(dir.path().join("db")).await.unwrap();
    let mut config: SourceConfigurationV1 = serde_json::from_str(include_str!(
        "../../../examples/source-configuration-v1.json"
    ))
    .unwrap();
    config.calendars[0].id = "c".into();
    config.mode = SourceMode::PushOnly;
    config.adapter = AdapterKind::StandardCalendar;
    config.capabilities.delta = true;
    config.capabilities.delete = true;
    db.put_source_configuration(
        "s",
        ConfigurationCondition::Absent,
        config.clone(),
        "alice",
        0,
    )
    .await
    .unwrap();
    config.enabled = true;
    db.put_source_configuration(
        "s",
        ConfigurationCondition::Revision(1),
        config.clone(),
        "alice",
        1,
    )
    .await
    .unwrap();
    let token = db
        .issue_api_token("managed", "s", true, true, None)
        .await
        .unwrap();
    let clock = Time::new(10);
    let first = db
        .ingest(
            token.expose(),
            request("same"),
            IngestPolicy::default(),
            &clock,
            "r1",
        )
        .await
        .unwrap();
    assert!(!first.replayed);
    config.mode = SourceMode::ManualStatic;
    db.put_source_configuration(
        "s",
        ConfigurationCondition::Revision(2),
        config,
        "alice",
        11,
    )
    .await
    .unwrap();
    assert!(matches!(
        db.ingest(
            token.expose(),
            request("same"),
            IngestPolicy::default(),
            &clock,
            "r2"
        )
        .await,
        Err(IngestError::Denied)
    ));
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM ingest_requests")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(receipts, 1);
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(events, 1);
    db.close().await;
}
