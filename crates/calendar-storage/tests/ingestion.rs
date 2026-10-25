use calendar_application::*;
use calendar_storage::Storage;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::tempdir;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
struct Pages {
    fail_second: bool,
    calls: AtomicUsize,
}
fn record(uid: &str) -> SyncRecord {
    SyncRecord::Upsert{calendar_id:"c".into(),event:Box::new(serde_json::from_value(json!({"schema_version":"1.0","external_uid":uid,"title":uid,"status":"confirmed","start":{"kind":"utc","value":"2026-11-02T17:00:00Z"}})).unwrap())}
}
impl SourceAdapter for Pages {
    async fn page(
        &self,
        _state: &SourceState,
        cursor: Option<&str>,
        _now: i64,
    ) -> std::result::Result<SourcePageOutcome, IngestionError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if cursor.is_some() && self.fail_second {
            return Err(IngestionError::Adapter);
        }
        Ok(SourcePageOutcome::Modified(SourcePage {
            records: vec![record(if cursor.is_some() { "b" } else { "a" })],
            next: if cursor.is_none() {
                Some("next".into())
            } else {
                None
            },
            checkpoint: Some("new".into()),
            bytes: 10,
            snapshot_token: Some("stable".into()),
        }))
    }
}
struct NotModified;
impl SourceAdapter for NotModified {
    async fn page(
        &self,
        _: &SourceState,
        _: Option<&str>,
        _: i64,
    ) -> std::result::Result<SourcePageOutcome, IngestionError> {
        Ok(SourcePageOutcome::NotModified)
    }
}
async fn seed(db: &Storage) -> Result {
    sqlx::raw_sql("INSERT INTO sources(id,kind) VALUES ('s','ics');INSERT INTO calendars(id,source_id,external_id) VALUES ('c','s','main');").execute(db.pool()).await?;
    Ok(())
}
#[tokio::test]
async fn ac1_pages_commit_together_and_failure_preserves_events_checkpoint_and_revision() -> Result
{
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    let adapter = Pages {
        fail_second: false,
        calls: AtomicUsize::new(0),
    };
    assert!(matches!(
        synchronize(
            &db,
            &adapter,
            "s",
            10,
            DeletionPolicy::Limit(0),
            SyncLimits::default()
        )
        .await?,
        SyncOutcome::Committed(_)
    ));
    let before = db.source_state("s").await?;
    assert_eq!(before.revision, 1);
    assert_eq!(before.checkpoint.as_deref(), Some("new"));
    assert!(before.has_snapshot);
    let fail = Pages {
        fail_second: true,
        calls: AtomicUsize::new(0),
    };
    assert_eq!(
        synchronize(
            &db,
            &fail,
            "s",
            20,
            DeletionPolicy::Limit(10),
            SyncLimits::default()
        )
        .await,
        Err(IngestionError::Adapter)
    );
    assert_eq!(db.source_state("s").await?, before);
    let live: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE deleted_at IS NULL")
        .fetch_one(db.pool())
        .await?;
    assert_eq!(live, 2);
    db.close().await;
    Ok(())
}
#[tokio::test]
async fn ac2_first_304_rejected_then_only_verification_advances_and_old_cas_fails() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    assert_eq!(
        synchronize(
            &db,
            &NotModified,
            "s",
            1,
            DeletionPolicy::Limit(0),
            SyncLimits::default()
        )
        .await,
        Err(IngestionError::UnexpectedNotModified)
    );
    let pages = Pages {
        fail_second: false,
        calls: AtomicUsize::new(0),
    };
    synchronize(
        &db,
        &pages,
        "s",
        10,
        DeletionPolicy::Limit(0),
        SyncLimits::default(),
    )
    .await?;
    let old = db.source_state("s").await?;
    assert_eq!(
        synchronize(
            &db,
            &NotModified,
            "s",
            20,
            DeletionPolicy::Limit(0),
            SyncLimits::default()
        )
        .await?,
        SyncOutcome::NotModified
    );
    let new = db.source_state("s").await?;
    assert_eq!(new.revision, old.revision);
    assert_eq!(new.checkpoint, old.checkpoint);
    assert_eq!(new.last_verified_at, Some(20));
    db.verify_unchanged("s", old.revision, 15).await?;
    assert_eq!(db.source_state("s").await?.last_verified_at, Some(20));
    assert_eq!(
        db.verify_unchanged("s", 0, 30).await,
        Err(IngestionError::Conflict)
    );
    assert!(new.push_ready(25, 5));
    assert!(!new.push_ready(26, 5));
    sqlx::query("UPDATE sources SET enabled=0 WHERE id='s'")
        .execute(db.pool())
        .await?;
    assert_eq!(
        synchronize(
            &db,
            &pages,
            "s",
            30,
            DeletionPolicy::Limit(0),
            SyncLimits::default()
        )
        .await,
        Err(IngestionError::Unavailable)
    );
    db.close().await;
    Ok(())
}

struct FakeRemote {
    replies: std::sync::Mutex<
        std::collections::VecDeque<std::result::Result<RemoteFetchOutcome, RemoteFetchError>>,
    >,
    calls: std::sync::Mutex<Vec<(String, bool, Option<String>)>>,
}
impl RemoteFetcher for FakeRemote {
    async fn fetch(
        &self,
        req: RemoteFetchRequest,
    ) -> std::result::Result<RemoteFetchOutcome, RemoteFetchError> {
        self.calls
            .lock()
            .unwrap()
            .push((req.url, req.has_committed_snapshot, req.validators.etag));
        self.replies.lock().unwrap().pop_front().unwrap()
    }
}
fn remote_reply(body: Vec<u8>) -> std::result::Result<RemoteFetchOutcome, RemoteFetchError> {
    Ok(RemoteFetchOutcome::Modified(RemoteDocument {
        body,
        validators: RemoteValidators {
            etag: Some("\"page\"".into()),
            last_modified: None,
        },
    }))
}
#[tokio::test]
async fn ac4_real_ics_and_declarative_json_mapping_reach_atomic_storage() -> Result {
    use calendar_adapters::{
        ingestion::{HttpSource, JsonMapping, SourceFormat},
        IcsLimits,
    };
    use std::{
        collections::{BTreeMap, VecDeque},
        sync::Arc,
    };
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    let remote=Arc::new(FakeRemote{replies:std::sync::Mutex::new(VecDeque::from([remote_reply(b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:ics\r\nDTSTAMP:20261022T000000Z\r\nDTSTART:20261102T170000Z\r\nSUMMARY:Imported\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n".to_vec())])),calls:Default::default()});
    let adapter = HttpSource {
        fetcher: remote,
        url: "https://calendar.example/ics".into(),
        authentication: RemoteAuthentication::None,
        calendar_id: "c".into(),
        format: SourceFormat::Ics(IcsLimits::default()),
    };
    synchronize(
        &db,
        &adapter,
        "s",
        10,
        DeletionPolicy::Limit(0),
        SyncLimits::default(),
    )
    .await?;
    let event: String =
        sqlx::query_scalar("SELECT external_uid FROM events WHERE deleted_at IS NULL")
            .fetch_one(db.pool())
            .await?;
    assert_eq!(event, "ics");
    let page = |uid: &str, next: serde_json::Value| {
        serde_json::to_vec(&json!({"snapshot_id":"stable","items":[{"uid":uid,"name":uid,"status":"confirmed","when":{"kind":"utc","value":"2026-11-02T17:00:00Z"}}],"next":next})).unwrap()
    };
    let remote = Arc::new(FakeRemote {
        replies: std::sync::Mutex::new(VecDeque::from([
            remote_reply(page("a", json!("/v2/pages/2"))),
            remote_reply(page("b", json!("3"))),
            remote_reply(page("c", json!(null))),
        ])),
        calls: Default::default(),
    });
    let adapter = HttpSource {
        fetcher: remote.clone(),
        url: "https://calendar.example/api/events".into(),
        authentication: RemoteAuthentication::None,
        calendar_id: "c".into(),
        format: SourceFormat::Json(JsonMapping {
            items: "/items".into(),
            fields: BTreeMap::from([
                ("external_uid".into(), "/uid".into()),
                ("title".into(), "/name".into()),
                ("status".into(), "/status".into()),
                ("start".into(), "/when".into()),
            ]),
            next: "/next".into(),
        }),
    };
    // A different adapter/configuration must not reuse the old ICS validator.

    synchronize(
        &db,
        &adapter,
        "s",
        20,
        DeletionPolicy::Limit(1),
        SyncLimits::default(),
    )
    .await?;
    let live: Vec<String> = sqlx::query_scalar(
        "SELECT external_uid FROM events WHERE deleted_at IS NULL ORDER BY external_uid",
    )
    .fetch_all(db.pool())
    .await?;
    assert_eq!(live, vec!["a", "b", "c"]);
    let checkpoint = db.source_state("s").await?.checkpoint.unwrap();
    assert!(!checkpoint.contains("page"));
    {
        let calls = remote.calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert!(!calls[0].1);
        assert!(calls[0].2.is_none());
        assert_eq!(calls[1].0, "https://calendar.example/v2/pages/2");
        assert_eq!(calls[2].0, "https://calendar.example/v2/pages/3");
        assert!(!calls[1].1);
        assert!(calls[1].2.is_none());
    }
    let before = db.source_state("s").await?;
    // Missing explicit end-of-pagination marker must not delete the old events.
    remote
        .replies
        .lock()
        .unwrap()
        .push_back(remote_reply(br#"{"items":[]}"#.to_vec()));
    assert_eq!(
        synchronize(
            &db,
            &adapter,
            "s",
            30,
            DeletionPolicy::Limit(10),
            SyncLimits::default()
        )
        .await,
        Err(IngestionError::InvalidSnapshot)
    );
    assert_eq!(db.source_state("s").await?, before);
    for next in [
        "https://other.example/page",
        "http://calendar.example/plain",
        "https://user:pass@calendar.example/page",
        "#fragment",
    ] {
        remote
            .replies
            .lock()
            .unwrap()
            .push_back(remote_reply(page("z", json!(next))));
        assert_eq!(
            synchronize(
                &db,
                &adapter,
                "s",
                40,
                DeletionPolicy::Limit(10),
                SyncLimits::default()
            )
            .await,
            Err(IngestionError::Adapter)
        );
        assert_eq!(db.source_state("s").await?, before);
    }
    db.close().await;
    Ok(())
}
#[tokio::test]
async fn ac1_concurrent_snapshots_use_cas_and_repeated_content_does_not_duplicate_history() -> Result
{
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    struct Race {
        barrier: tokio::sync::Barrier,
    }
    impl SourceAdapter for Race {
        async fn page(
            &self,
            _: &SourceState,
            _: Option<&str>,
            _: i64,
        ) -> std::result::Result<SourcePageOutcome, IngestionError> {
            self.barrier.wait().await;
            Ok(SourcePageOutcome::Modified(SourcePage {
                records: vec![record("same")],
                next: None,
                checkpoint: None,
                bytes: 1,
                snapshot_token: None,
            }))
        }
    }
    let adapter = Race {
        barrier: tokio::sync::Barrier::new(2),
    };
    let (a, b) = tokio::join!(
        synchronize(
            &db,
            &adapter,
            "s",
            10,
            DeletionPolicy::Limit(0),
            SyncLimits::default()
        ),
        synchronize(
            &db,
            &adapter,
            "s",
            10,
            DeletionPolicy::Limit(0),
            SyncLimits::default()
        )
    );
    assert!(matches!(
        (a, b),
        (Ok(_), Err(IngestionError::Conflict)) | (Err(IngestionError::Conflict), Ok(_))
    ));
    let state = db.source_state("s").await?;
    let batch = SyncBatch {
        source_id: "s".into(),
        base_revision: state.revision,
        mode: SyncMode::FullSnapshot {
            complete: true,
            deletion_policy: DeletionPolicy::Limit(0),
        },
        records: vec![record("same")],
        proposed_checkpoint: Some("repeat".into()),
        verified_at: 20,
    };
    assert_eq!(db.commit_snapshot(batch.clone()).await?.changed_events, 0);
    assert_eq!(
        db.commit_snapshot(batch).await,
        Err(IngestionError::Conflict)
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM event_revisions")
        .fetch_one(db.pool())
        .await?;
    assert_eq!(count, 1);
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac1_budgets_incomplete_304_duplicate_records_and_loops_never_commit() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    struct Invalid {
        mode: u8,
    }
    impl SourceAdapter for Invalid {
        async fn page(
            &self,
            _: &SourceState,
            cursor: Option<&str>,
            _: i64,
        ) -> std::result::Result<SourcePageOutcome, IngestionError> {
            if self.mode == 1 && cursor.is_some() {
                return Ok(SourcePageOutcome::NotModified);
            }
            if self.mode == 3 {
                std::future::pending::<()>().await;
            }
            Ok(SourcePageOutcome::Modified(SourcePage {
                records: vec![record("duplicate")],
                next: if self.mode == 0 || self.mode == 1 || cursor.is_none() {
                    Some("same-cursor".into())
                } else {
                    None
                },
                checkpoint: None,
                bytes: 100,
                snapshot_token: Some(
                    if self.mode == 4 && cursor.is_some() {
                        "changed"
                    } else {
                        "stable"
                    }
                    .into(),
                ),
            }))
        }
    }
    let before = db.source_state("s").await?;
    for (mode, limits, expected) in [
        (0, SyncLimits::default(), IngestionError::InvalidSnapshot),
        (
            1,
            SyncLimits::default(),
            IngestionError::UnexpectedNotModified,
        ),
        (2, SyncLimits::default(), IngestionError::InvalidSnapshot),
        (4, SyncLimits::default(), IngestionError::InvalidSnapshot),
        (
            2,
            SyncLimits {
                max_pages: 1,
                ..SyncLimits::default()
            },
            IngestionError::Budget,
        ),
        (
            2,
            SyncLimits {
                max_bytes: 99,
                ..SyncLimits::default()
            },
            IngestionError::Budget,
        ),
        (
            3,
            SyncLimits {
                timeout: std::time::Duration::from_millis(20),
                ..SyncLimits::default()
            },
            IngestionError::Timeout,
        ),
    ] {
        assert_eq!(
            synchronize(
                &db,
                &Invalid { mode },
                "s",
                10,
                DeletionPolicy::Limit(10),
                limits
            )
            .await,
            Err(expected)
        );
        assert_eq!(db.source_state("s").await?, before);
    }
    db.close().await;
    Ok(())
}
#[tokio::test]
async fn ac1_commit_failure_rolls_back_snapshot_proof_events_and_checkpoint() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    let before = db.source_state("s").await?;
    sqlx::query("CREATE TRIGGER fail_source BEFORE UPDATE ON sources BEGIN SELECT RAISE(ABORT,'injected write failure'); END").execute(db.pool()).await?;
    let pages = Pages {
        fail_second: false,
        calls: AtomicUsize::new(0),
    };
    assert_eq!(
        synchronize(
            &db,
            &pages,
            "s",
            10,
            DeletionPolicy::Limit(0),
            SyncLimits::default()
        )
        .await,
        Err(IngestionError::Storage)
    );
    assert_eq!(db.source_state("s").await?, before);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(db.pool())
        .await?;
    assert_eq!(count, 0);
    db.close().await;
    Ok(())
}
#[tokio::test]
async fn ac2_v5_upgrade_preserves_data_but_requires_fresh_snapshot_proof() -> Result {
    use sqlx::{sqlite::SqliteConnectOptions, Connection, SqliteConnection};
    let dir = tempdir()?;
    let migrations = tempdir()?;
    for (name, text) in [
        (
            "0001_event_core.sql",
            include_str!("../migrations/0001_event_core.sql"),
        ),
        (
            "0002_reminder_planning.sql",
            include_str!("../migrations/0002_reminder_planning.sql"),
        ),
        (
            "0003_job_planning_context.sql",
            include_str!("../migrations/0003_job_planning_context.sql"),
        ),
        (
            "0004_delivery_attempts.sql",
            include_str!("../migrations/0004_delivery_attempts.sql"),
        ),
        (
            "0005_security.sql",
            include_str!("../migrations/0005_security.sql"),
        ),
    ] {
        std::fs::write(migrations.path().join(name), text)?;
    }
    let path = dir.path().join("db");
    let mut conn = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true),
    )
    .await?;
    sqlx::migrate::Migrator::new(migrations.path())
        .await?
        .run(&mut conn)
        .await?;
    sqlx::query("INSERT INTO sources(id,kind,revision,committed_checkpoint,last_verified_at) VALUES ('s','ics',7,'old-cursor',10)").execute(&mut conn).await?;
    conn.close().await?;
    let db = Storage::open(&path).await?;
    let state = db.source_state("s").await?;
    assert_eq!(state.revision, 7);
    assert_eq!(state.checkpoint.as_deref(), Some("old-cursor"));
    assert!(!state.has_snapshot);
    assert_eq!(
        synchronize(
            &db,
            &NotModified,
            "s",
            20,
            DeletionPolicy::Limit(0),
            SyncLimits::default()
        )
        .await,
        Err(IngestionError::UnexpectedNotModified)
    );
    db.close().await;
    Ok(())
}
