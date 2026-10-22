//! File-backed acceptance/integration tests for Issue #21 (V-Model R1–R5).

use calendar_storage::{OpenError, Storage};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Connection, SqliteConnection};
use tempfile::tempdir;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

async fn seed(db: &Storage) -> Result {
    sqlx::raw_sql(
        "INSERT INTO sources(id,kind) VALUES ('s1','ics'),('s2','ics');
         INSERT INTO calendars(id,source_id,external_id) VALUES ('c1','s1','main'),('c2','s2','main');
         INSERT INTO events(id,source_id,calendar_id,external_uid,document_json,content_hash)
           VALUES ('e1','s1','c1','uid','{}','hash');
         INSERT INTO event_revisions(event_id,revision,document_json,change_kind,changed_at)
           VALUES ('e1',0,'{}','upsert',0);",
    )
    .execute(db.pool())
    .await?;
    Ok(())
}

async fn rejected(db: &Storage, sql: &str, code: &str) {
    let error = sqlx::query(sql).execute(db.pool()).await.unwrap_err();
    let actual = error.as_database_error().and_then(|e| e.code());
    assert_eq!(actual.as_deref(), Some(code), "{sql}: {error}");
}

#[tokio::test]
async fn every_connection_has_durable_pragmas_and_the_pool_is_bounded() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("calendar.sqlite")).await?;
    assert_eq!(db.pool().options().get_max_connections(), 4);
    // Hold all four simultaneously so a single correctly configured connection cannot hide others.
    let mut held = Vec::new();
    for _ in 0..4 {
        let mut connection = db.pool().acquire().await?;
        let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&mut *connection)
            .await?;
        assert_eq!(journal, "wal");
        for (pragma, expected) in [
            ("foreign_keys", 1_i64),
            ("synchronous", 2),
            ("busy_timeout", 5000),
        ] {
            let actual: i64 = sqlx::query_scalar(&format!("PRAGMA {pragma}"))
                .fetch_one(&mut *connection)
                .await?;
            assert_eq!(actual, expected, "{pragma}");
        }
        rejected_connection(&mut connection).await?;
        held.push(connection);
    }
    drop(held);
    db.close().await;
    Ok(())
}

async fn rejected_connection(connection: &mut SqliteConnection) -> Result {
    let error = sqlx::query(
        "INSERT INTO calendars(id,source_id,external_id) VALUES ('orphan','missing','main')",
    )
    .execute(connection)
    .await
    .unwrap_err();
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("787")
    );
    Ok(())
}

#[tokio::test]
async fn migrations_are_repeatable_and_committed_data_survives_reopen() -> Result {
    let dir = tempdir()?;
    let path = dir.path().join("calendar.sqlite");
    let db = Storage::open(&path).await?;
    seed(&db).await?;
    db.close().await;
    for _ in 0..2 {
        let db = Storage::open(&path).await?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE id='e1'")
            .fetch_one(db.pool())
            .await?;
        assert_eq!(count, 1);
        let migrations: i64 =
            sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success=1")
                .fetch_one(db.pool())
                .await?;
        assert_eq!(migrations, 1);
        let health: String = sqlx::query_scalar("PRAGMA integrity_check")
            .fetch_one(db.pool())
            .await?;
        assert_eq!(health, "ok");
        assert!(sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(db.pool())
            .await?
            .is_empty());
        db.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn event_identity_is_source_scoped_and_calendar_ownership_is_enforced() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("calendar.sqlite")).await?;
    seed(&db).await?;
    sqlx::query("INSERT INTO events(id,source_id,calendar_id,external_uid,document_json,content_hash) VALUES ('e2','s2','c2','uid','{}','hash')").execute(db.pool()).await?;
    rejected(&db, "INSERT INTO events(id,source_id,calendar_id,external_uid,document_json,content_hash) VALUES ('dup','s1','c1','uid','{}','hash')", "2067").await;
    rejected(&db, "INSERT INTO events(id,source_id,calendar_id,external_uid,document_json,content_hash) VALUES ('wrong','s1','c2','another','{}','hash')", "787").await;
    rejected(
        &db,
        "UPDATE events SET calendar_id='c2' WHERE id='e1'",
        "787",
    )
    .await;
    rejected(
        &db,
        "INSERT INTO calendars(id,source_id,external_id) VALUES ('dup','s1','main')",
        "2067",
    )
    .await;
    // Moving calendars within the same source preserves the source-level event identity.
    sqlx::query("INSERT INTO calendars(id,source_id,external_id) VALUES ('c3','s1','other')")
        .execute(db.pool())
        .await?;
    sqlx::query("UPDATE events SET calendar_id='c3' WHERE id='e1'")
        .execute(db.pool())
        .await?;
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn revision_log_rejects_cross_source_and_missing_event_revisions() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("calendar.sqlite")).await?;
    seed(&db).await?;
    sqlx::query("INSERT INTO source_changes(source_id,source_revision,event_id,event_revision,change_kind) VALUES ('s1',1,'e1',0,'upsert')").execute(db.pool()).await?;
    rejected(&db, "INSERT INTO source_changes(source_id,source_revision,event_id,event_revision,change_kind) VALUES ('s2',1,'e1',0,'upsert')", "787").await;
    rejected(&db, "INSERT INTO source_changes(source_id,source_revision,event_id,event_revision,change_kind) VALUES ('s1',2,'e1',99,'upsert')", "787").await;
    rejected(&db, "INSERT INTO source_changes(source_id,source_revision,event_id,event_revision,change_kind) VALUES ('s1',2,'e1',0,'delete')", "787").await;
    rejected(&db, "INSERT INTO source_changes(source_id,source_revision,event_id,event_revision,change_kind) VALUES ('s1',1,'e1',0,'upsert')", "1555").await;
    rejected(
        &db,
        "DELETE FROM event_revisions WHERE event_id='e1'",
        "787",
    )
    .await;
    rejected(&db, "DELETE FROM sources WHERE id='s1'", "787").await;
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn invalid_json_revisions_and_change_kinds_are_rejected() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("calendar.sqlite")).await?;
    seed(&db).await?;
    for statement in [
        "UPDATE sources SET config_json='broken' WHERE id='s1'",
        "UPDATE sources SET revision=-1 WHERE id='s1'",
        "UPDATE sources SET enabled=2 WHERE id='s1'",
        "UPDATE calendars SET timezone_policy='broken' WHERE id='c1'",
        "UPDATE events SET document_json='broken' WHERE id='e1'",
        "UPDATE events SET revision=-1 WHERE id='e1'",
        "UPDATE event_revisions SET document_json='broken' WHERE event_id='e1'",
        "UPDATE event_revisions SET revision=-1 WHERE event_id='e1'",
        "UPDATE event_revisions SET change_kind='unknown' WHERE event_id='e1'",
        "UPDATE event_revisions SET document_json=NULL WHERE event_id='e1'",
        "INSERT INTO source_changes(source_id,source_revision,event_id,event_revision,change_kind) VALUES ('s1',-1,'e1',0,'upsert')",
        "INSERT INTO source_changes(source_id,source_revision,event_id,event_revision,change_kind) VALUES ('s1',1,'e1',-1,'upsert')",
        "INSERT INTO source_changes(source_id,source_revision,event_id,event_revision,change_kind) VALUES ('s1',1,'e1',0,'unknown')",
    ] {
        rejected(&db, statement, "275").await;
    }
    // Deletion history can explicitly omit a document while retaining the revision.
    sqlx::query("INSERT INTO event_revisions(event_id,revision,document_json,change_kind,changed_at) VALUES ('e1',1,NULL,'delete',1)").execute(db.pool()).await?;
    sqlx::query("INSERT INTO source_changes(source_id,source_revision,event_id,event_revision,change_kind) VALUES ('s1',1,'e1',1,'delete')").execute(db.pool()).await?;
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn uncommitted_writes_do_not_survive_reopen() -> Result {
    let dir = tempdir()?;
    let path = dir.path().join("calendar.sqlite");
    let db = Storage::open(&path).await?;
    let mut transaction = db.pool().begin().await?;
    sqlx::query("INSERT INTO sources(id,kind) VALUES ('uncommitted','ics')")
        .execute(&mut *transaction)
        .await?;
    drop(transaction);
    db.close().await;
    let db = Storage::open(&path).await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM sources")
        .fetch_one(db.pool())
        .await?;
    assert_eq!(count, 0);
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn incompatible_migration_history_fails_without_destroying_data() -> Result {
    for mutation in [
        "UPDATE _sqlx_migrations SET checksum=X'00' WHERE version=1",
        "INSERT INTO _sqlx_migrations(version,description,installed_on,success,checksum,execution_time) VALUES (999,'future',CURRENT_TIMESTAMP,1,X'00',0)",
        "UPDATE _sqlx_migrations SET success=0 WHERE version=1",
    ] {
        let dir = tempdir()?;
        let path = dir.path().join("calendar.sqlite");
        let db = Storage::open(&path).await?;
        seed(&db).await?;
        sqlx::query(mutation).execute(db.pool()).await?;
        db.close().await;
        assert!(matches!(Storage::open(&path).await, Err(OpenError::Migration(_))));
        let mut connection = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path)).await?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE id='e1'").fetch_one(&mut connection).await?;
        assert_eq!(count, 1);
        connection.close().await?;
    }
    Ok(())
}

#[tokio::test]
async fn invalid_paths_and_corrupt_files_are_reported_without_replacement() -> Result {
    let dir = tempdir()?;
    assert!(Storage::open(dir.path().join("missing/calendar.sqlite"))
        .await
        .is_err());
    assert!(Storage::open(dir.path()).await.is_err());
    let path = dir.path().join("corrupt.sqlite");
    let contents = b"This is not a SQLite database; preserve the original bytes.";
    std::fs::write(&path, contents)?;
    assert!(Storage::open(&path).await.is_err());
    assert_eq!(std::fs::read(&path)?, contents);
    Ok(())
}
