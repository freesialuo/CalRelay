use calendar_application::{
    DeletionPolicy, EventRepository, EventV1, SyncBatch, SyncMode, SyncRecord,
};
use calendar_storage::{Storage, SyncError};
use serde_json::json;
use tempfile::tempdir;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn upsert(uid: &str, title: &str) -> SyncRecord {
    let event: EventV1 = serde_json::from_value(json!({"schema_version":"1.0","external_uid":uid,"title":title,"status":"confirmed","start":{"kind":"utc","value":"2026-11-02T17:00:00Z"}})).unwrap();
    SyncRecord::Upsert {
        calendar_id: "c".into(),
        event: Box::new(event),
    }
}
fn delete(uid: &str) -> SyncRecord {
    SyncRecord::Delete {
        external_uid: uid.into(),
    }
}
fn batch(base: i64, records: Vec<SyncRecord>) -> SyncBatch {
    SyncBatch {
        source_id: "s".into(),
        base_revision: base,
        mode: SyncMode::Delta,
        records,
        proposed_checkpoint: Some(format!("page-{base}")),
        verified_at: 42_i64.saturating_add(base),
    }
}
fn snapshot(base: i64, records: Vec<SyncRecord>, limit: usize) -> SyncBatch {
    SyncBatch {
        mode: SyncMode::FullSnapshot {
            complete: true,
            deletion_policy: DeletionPolicy::Limit(limit),
        },
        ..batch(base, records)
    }
}
async fn seed(db: &Storage) -> Result {
    sqlx::raw_sql("INSERT INTO sources(id,kind) VALUES ('s','ics'),('t','ics'); INSERT INTO calendars(id,source_id,external_id) VALUES ('c','s','main'),('c2','s','other'),('d','t','main');").execute(db.pool()).await?;
    Ok(())
}
async fn state(db: &Storage) -> Result<(i64, Option<String>, Option<i64>)> {
    Ok(sqlx::query_as(
        "SELECT revision,committed_checkpoint,last_verified_at FROM sources WHERE id='s'",
    )
    .fetch_one(db.pool())
    .await?)
}
async fn count(db: &Storage, table: &str) -> Result<i64> {
    let query = match table {
        "event_revisions" => "SELECT count(*) FROM event_revisions",
        "events" => "SELECT count(*) FROM events",
        "source_changes" => "SELECT count(*) FROM source_changes",
        _ => panic!("unsupported test table"),
    };
    Ok(sqlx::query_scalar(query).fetch_one(db.pool()).await?)
}

#[tokio::test]
async fn commits_events_history_checkpoint_and_reopens_consistently() -> Result {
    let dir = tempdir()?;
    let path = dir.path().join("calendar.sqlite");
    let db = Storage::open(&path).await?;
    seed(&db).await?;
    let result = db
        .commit_sync(batch(0, vec![upsert("a", "A"), upsert("b", "B")]))
        .await?;
    assert_eq!((result.source_revision, result.changed_events), (1, 2));
    assert_eq!(state(&db).await?, (1, Some("page-0".into()), Some(42)));
    assert_eq!(count(&db, "event_revisions").await?, 2);
    assert_eq!(count(&db, "source_changes").await?, 2);
    db.close().await;
    let db = Storage::open(&path).await?;
    assert_eq!(count(&db, "events").await?, 2);
    assert_eq!(state(&db).await?.0, 1);
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn unchanged_sync_advances_cas_without_duplicate_event_revisions() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    db.commit_sync(batch(0, vec![upsert("a", "A")])).await?;
    let result = db
        .commit_sync(batch(1, vec![upsert("a", "A"), delete("missing")]))
        .await?;
    assert_eq!((result.source_revision, result.changed_events), (2, 0));
    assert_eq!(count(&db, "event_revisions").await?, 1);
    assert_eq!(count(&db, "source_changes").await?, 1);
    assert_eq!(state(&db).await?.1, Some("page-1".into()));
    assert!(matches!(
        db.commit_sync(batch(1, vec![])).await,
        Err(SyncError::Conflict)
    ));
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn delta_delete_restore_and_calendar_move_preserve_internal_identity() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    db.commit_sync(batch(0, vec![upsert("a", "A"), upsert("b", "B")]))
        .await?;
    let original: String = sqlx::query_scalar("SELECT id FROM events WHERE external_uid='a'")
        .fetch_one(db.pool())
        .await?;
    db.commit_sync(batch(1, vec![delete("a")])).await?;
    let active: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE deleted_at IS NULL")
        .fetch_one(db.pool())
        .await?;
    assert_eq!(active, 1);
    assert_eq!(
        db.commit_sync(batch(2, vec![delete("a")]))
            .await?
            .changed_events,
        0
    );
    let mut restored = upsert("a", "A");
    if let SyncRecord::Upsert { calendar_id, event } = &mut restored {
        *calendar_id = "c2".into();
        event.status = serde_json::from_value(json!("cancelled"))?;
    }
    db.commit_sync(batch(3, vec![restored])).await?;
    let row: (String, i64, String, Option<i64>) = sqlx::query_as(
        "SELECT id,revision,calendar_id,deleted_at FROM events WHERE external_uid='a'",
    )
    .fetch_one(db.pool())
    .await?;
    assert_eq!(row, (original, 3, "c2".into(), None));
    let kind: String =
        sqlx::query_scalar("SELECT change_kind FROM event_revisions WHERE revision=2")
            .fetch_one(db.pool())
            .await?;
    assert_eq!(kind, "delete");
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn full_snapshot_deletes_only_missing_live_events_and_requires_approval() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    db.commit_sync(batch(0, vec![upsert("a", "A"), upsert("b", "B")]))
        .await?;
    assert!(matches!(
        db.commit_sync(snapshot(1, vec![], 1)).await,
        Err(SyncError::ReviewRequired { deletions: 2 })
    ));
    assert_eq!(state(&db).await?.0, 1);
    assert_eq!(
        db.commit_sync(snapshot(1, vec![upsert("a", "A")], 1))
            .await?
            .changed_events,
        1
    );
    let mut approved = snapshot(2, vec![], 0);
    approved.mode = SyncMode::FullSnapshot {
        complete: true,
        deletion_policy: DeletionPolicy::Approved,
    };
    assert_eq!(db.commit_sync(approved).await?.changed_events, 1);
    assert_eq!(
        db.commit_sync(snapshot(3, vec![], 0)).await?.changed_events,
        0
    );
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn invalid_or_incomplete_batches_never_write_or_advance_checkpoint() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    let mut incomplete = snapshot(0, vec![], 0);
    incomplete.mode = SyncMode::FullSnapshot {
        complete: false,
        deletion_policy: DeletionPolicy::Approved,
    };
    let mut invalid_event = upsert("bad", "B");
    if let SyncRecord::Upsert { event, .. } = &mut invalid_event {
        event.duration = Some("-PT1H".into());
    }
    for invalid in [
        incomplete,
        batch(0, vec![upsert("a", "A"), invalid_event]),
        batch(0, vec![upsert("a", "A"), delete("a")]),
        snapshot(0, vec![delete("a")], 0),
    ] {
        assert!(matches!(
            db.commit_sync(invalid).await,
            Err(SyncError::Invalid(_))
        ));
    }
    assert_eq!(state(&db).await?, (0, None, None));
    assert_eq!(count(&db, "events").await?, 0);
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn concurrent_same_revision_has_exactly_one_winner() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    let (a, b) = tokio::join!(
        db.commit_sync(batch(0, vec![upsert("a", "A")])),
        db.commit_sync(batch(0, vec![upsert("b", "B")]))
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert!(matches!(a, Err(SyncError::Conflict)) || matches!(b, Err(SyncError::Conflict)));
    assert_eq!(count(&db, "events").await?, 1);
    assert_eq!(state(&db).await?.0, 1);
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn database_failure_rolls_back_earlier_writes_and_the_source_cas() -> Result {
    let dir = tempdir()?;
    let path = dir.path().join("db");
    let db = Storage::open(&path).await?;
    seed(&db).await?;
    db.commit_sync(batch(0, vec![upsert("a", "old")])).await?;
    sqlx::raw_sql("CREATE TRIGGER fail_log BEFORE INSERT ON source_changes WHEN NEW.event_revision=2 BEGIN SELECT RAISE(ABORT,'injected failure'); END;").execute(db.pool()).await?;
    assert!(matches!(
        db.commit_sync(batch(1, vec![upsert("new", "new"), upsert("a", "changed")]))
            .await,
        Err(SyncError::Database(_))
    ));
    db.close().await;
    let db = Storage::open(&path).await?;
    assert_eq!(count(&db, "events").await?, 1);
    assert_eq!(count(&db, "event_revisions").await?, 1);
    assert_eq!(count(&db, "source_changes").await?, 1);
    let document: String =
        sqlx::query_scalar("SELECT document_json FROM events WHERE external_uid='a'")
            .fetch_one(db.pool())
            .await?;
    let original: EventV1 = serde_json::from_str(&document)?;
    assert_eq!(original.title, "old");
    assert_eq!(state(&db).await?, (1, Some("page-0".into()), Some(42)));
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn calendar_scope_disabled_unknown_source_and_revision_overflow_are_rejected() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    let mut wrong = upsert("a", "A");
    if let SyncRecord::Upsert { calendar_id, .. } = &mut wrong {
        *calendar_id = "d".into();
    }
    assert!(matches!(
        db.commit_sync(batch(0, vec![wrong])).await,
        Err(SyncError::CalendarMismatch)
    ));
    let mut unknown = batch(0, vec![]);
    unknown.source_id = "unknown".into();
    assert!(matches!(
        db.commit_sync(unknown).await,
        Err(SyncError::SourceUnavailable)
    ));
    sqlx::query("UPDATE sources SET enabled=0 WHERE id='s'")
        .execute(db.pool())
        .await?;
    assert!(matches!(
        db.commit_sync(batch(0, vec![])).await,
        Err(SyncError::SourceUnavailable)
    ));
    sqlx::query("UPDATE sources SET enabled=1, revision=? WHERE id='s'")
        .bind(i64::MAX)
        .execute(db.pool())
        .await?;
    assert!(matches!(
        db.commit_sync(batch(i64::MAX, vec![])).await,
        Err(SyncError::RevisionOverflow)
    ));
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn older_upstream_sequence_cannot_overwrite_newer_content() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    let mut high = upsert("a", "new");
    if let SyncRecord::Upsert { event, .. } = &mut high {
        event.source_sequence = Some(2);
    }
    db.commit_sync(batch(0, vec![high])).await?;
    let mut low = upsert("a", "old");
    if let SyncRecord::Upsert { event, .. } = &mut low {
        event.source_sequence = Some(1);
    }
    assert!(matches!(
        db.commit_sync(batch(1, vec![low])).await,
        Err(SyncError::StaleSequence)
    ));
    assert_eq!(state(&db).await?.0, 1);
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn identical_uids_in_other_sources_are_untouched_by_snapshot_deletion() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    db.commit_sync(batch(0, vec![upsert("same", "S")])).await?;
    let mut other = batch(0, vec![upsert("same", "T")]);
    other.source_id = "t".into();
    if let SyncRecord::Upsert { calendar_id, .. } = &mut other.records[0] {
        *calendar_id = "d".into();
    }
    db.commit_sync(other).await?;
    db.commit_sync(snapshot(1, vec![], 1)).await?;
    let remaining: Vec<String> =
        sqlx::query_scalar("SELECT source_id FROM events WHERE deleted_at IS NULL")
            .fetch_all(db.pool())
            .await?;
    assert_eq!(remaining, vec!["t"]);
    assert_eq!(count(&db, "events").await?, 2);
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM sources WHERE id='t'")
        .fetch_one(db.pool())
        .await?;
    assert_eq!(revision, 1);
    db.close().await;
    Ok(())
}
