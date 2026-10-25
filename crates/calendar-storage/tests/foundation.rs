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

async fn rejected(db: &Storage, sql: &'static str, code: &str) {
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
            let actual: i64 = sqlx::query_scalar(match pragma {
                "foreign_keys" => "PRAGMA foreign_keys",
                "synchronous" => "PRAGMA synchronous",
                "busy_timeout" => "PRAGMA busy_timeout",
                _ => unreachable!(),
            })
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
        assert_eq!(migrations, 9);
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

#[tokio::test]
async fn upgrades_v1_database_to_latest_preserving_history_and_enforcing_planning_schema() -> Result
{
    let dir = tempdir()?;
    let ref_path = dir.path().join("ref.sqlite");
    let ref_db = Storage::open(&ref_path).await?;
    let (desc, checksum): (String, Vec<u8>) =
        sqlx::query_as("SELECT description, checksum FROM _sqlx_migrations WHERE version = 1")
            .fetch_one(ref_db.pool())
            .await?;
    ref_db.close().await;

    let build_v1 = |name: &str| {
        let path = dir.path().join(name);
        let desc = desc.clone();
        let checksum = checksum.clone();
        async move {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await?;
            sqlx::raw_sql(
                "CREATE TABLE _sqlx_migrations (
                    version BIGINT PRIMARY KEY,
                    description TEXT NOT NULL,
                    installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    success BOOLEAN NOT NULL,
                    checksum BLOB NOT NULL,
                    execution_time BIGINT NOT NULL
                );",
            )
            .execute(&mut conn)
            .await?;
            sqlx::query(
                "INSERT INTO _sqlx_migrations(version, description, success, checksum, execution_time)
                 VALUES (1, ?, 1, ?, 1)",
            )
            .bind(desc)
            .bind(checksum)
            .execute(&mut conn)
            .await?;
            sqlx::raw_sql(include_str!("../migrations/0001_event_core.sql"))
                .execute(&mut conn)
                .await?;
            sqlx::raw_sql(
                "INSERT INTO sources(id,kind,revision,committed_checkpoint) VALUES ('s1','ics',1,'cp-1');
                 INSERT INTO calendars(id,source_id,external_id) VALUES ('c1','s1','main');
                 INSERT INTO events(id,source_id,calendar_id,external_uid,revision,document_json,content_hash)
                   VALUES ('e1','s1','c1','uid-1',1,'{\"title\":\"v1\"}','h1');
                 INSERT INTO event_revisions(event_id,revision,document_json,change_kind,changed_at)
                   VALUES ('e1',1,'{\"title\":\"v1\"}','upsert',100);
                 INSERT INTO source_changes(source_id,source_revision,event_id,event_revision,change_kind)
                   VALUES ('s1',1,'e1',1,'upsert');",
            )
            .execute(&mut conn)
            .await?;
            conn.close().await?;
            Ok::<_, Box<dyn std::error::Error>>(path)
        }
    };

    // Failed v2 upgrade must not destroy v1 event data or revision history.
    let failing_path = build_v1("v1_fail.sqlite").await?;
    let mut conn =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&failing_path))
            .await?;
    sqlx::raw_sql("CREATE TABLE planning_watermarks(conflict INTEGER)")
        .execute(&mut conn)
        .await?;
    conn.close().await?;
    assert!(matches!(
        Storage::open(&failing_path).await,
        Err(OpenError::Migration(_))
    ));
    let mut conn =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&failing_path))
            .await?;
    let preserved: (String, i64) =
        sqlx::query_as("SELECT external_uid, revision FROM events WHERE id = 'e1'")
            .fetch_one(&mut conn)
            .await?;
    assert_eq!(preserved, ("uid-1".into(), 1));
    let preserved_changes: i64 = sqlx::query_scalar("SELECT count(*) FROM source_changes")
        .fetch_one(&mut conn)
        .await?;
    assert_eq!(preserved_changes, 1);
    conn.close().await?;

    // Successful v1 -> v2 upgrade preserves v1 rows and enforces v2 constraints and indexes.
    let upgrade_path = build_v1("v1_upgrade.sqlite").await?;
    let db = Storage::open(&upgrade_path).await?;
    let versions: Vec<i64> = sqlx::query_scalar(
        "SELECT version FROM _sqlx_migrations WHERE success = 1 ORDER BY version",
    )
    .fetch_all(db.pool())
    .await?;
    assert_eq!(versions, vec![1, 2, 3, 4, 5, 6, 7, 9, 10]);
    let event_row: (String, i64, String) =
        sqlx::query_as("SELECT external_uid, revision, content_hash FROM events WHERE id = 'e1'")
            .fetch_one(db.pool())
            .await?;
    assert_eq!(event_row, ("uid-1".into(), 1, "h1".into()));

    rejected(
        &db,
        "INSERT INTO delivery_jobs(id,logical_delivery_id,generation,event_revision,rule_revision,output_revision,due_at,state,next_attempt_at) VALUES ('j1','missing',1,1,1,1,0,'pending',0)",
        "787",
    )
    .await;
    rejected(
        &db,
        "INSERT INTO reminder_rules(id,policy_json,generation,enabled) VALUES ('r1','invalid',1,1)",
        "275",
    )
    .await;
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn upgrades_v2_jobs_to_nullable_planning_context_without_losing_history() -> Result {
    let dir = tempdir()?;
    let historical = tempdir()?;
    std::fs::write(
        historical.path().join("0001_event_core.sql"),
        include_str!("../migrations/0001_event_core.sql"),
    )?;
    std::fs::write(
        historical.path().join("0002_reminder_planning.sql"),
        include_str!("../migrations/0002_reminder_planning.sql"),
    )?;
    let path = dir.path().join("v2.sqlite");
    let mut conn = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true),
    )
    .await?;
    sqlx::migrate::Migrator::new(historical.path())
        .await?
        .run(&mut conn)
        .await?;
    sqlx::raw_sql(
        "INSERT INTO sources(id,kind) VALUES ('s','ics');
         INSERT INTO calendars(id,source_id,external_id) VALUES ('c','s','main');
         INSERT INTO events(id,source_id,calendar_id,external_uid,revision,document_json,content_hash)
           VALUES ('e','s','c','a',1,'{}','h');
         INSERT INTO reminder_rules(id) VALUES ('r');
         INSERT INTO outputs(id,kind) VALUES ('o','bark');
         INSERT INTO rule_outputs(rule_id,output_id) VALUES ('r','o');
         INSERT INTO logical_deliveries(id,event_id,original_key,rule_id,output_id,accepted_at)
           VALUES ('l','e','single','r','o',123);
         INSERT INTO delivery_jobs(id,logical_delivery_id,generation,event_revision,rule_revision,output_revision,due_at,state,next_attempt_at)
           VALUES ('j','l',1,1,1,1,100,'accepted',100);"
    ).execute(&mut conn).await?;
    conn.close().await?;
    for _ in 0..2 {
        let db = Storage::open(&path).await?;
        let row:(String,String,Option<String>,i64)=sqlx::query_as(
            "SELECT j.id,j.state,j.planning_context,l.accepted_at FROM delivery_jobs j JOIN logical_deliveries l ON l.id=j.logical_delivery_id"
        ).fetch_one(db.pool()).await?;
        assert_eq!(row, ("j".into(), "accepted".into(), None, 123));
        let versions: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
                .fetch_all(db.pool())
                .await?;
        assert_eq!(versions, vec![1, 2, 3, 4, 5, 6, 7, 9, 10]);
        rejected(
            &db,
            "UPDATE delivery_jobs SET planning_context='invalid json' WHERE id='j'",
            "275",
        )
        .await;
        assert!(sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(db.pool())
            .await?
            .is_empty());
        db.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn upgrades_v3_to_delivery_attempts_preserving_jobs_and_failure_history() -> Result {
    let dir = tempdir()?;
    let migrations = tempdir()?;
    for (name, source) in [
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
    ] {
        std::fs::write(migrations.path().join(name), source)?;
    }
    for conflict in [false, true] {
        let path = dir.path().join(if conflict { "fail" } else { "ok" });
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
        sqlx::raw_sql("INSERT INTO sources(id,kind) VALUES ('s','test'); INSERT INTO calendars(id,source_id,external_id) VALUES ('c','s','c'); INSERT INTO events(id,source_id,calendar_id,external_uid,revision,document_json,content_hash) VALUES ('e','s','c','a',1,'{}','h'); INSERT INTO outputs(id,kind) VALUES ('o','test'); INSERT INTO reminder_rules(id) VALUES ('r'); INSERT INTO rule_outputs VALUES ('r','o'); INSERT INTO logical_deliveries(id,event_id,original_key,rule_id,output_id,accepted_at) VALUES ('l','e','single','r','o',123); INSERT INTO delivery_jobs(id,logical_delivery_id,generation,event_revision,rule_revision,output_revision,due_at,state,next_attempt_at,planning_context) VALUES ('j','l',1,1,1,1,100,'accepted',100,'[null,\"2025b\"]');").execute(&mut conn).await?;
        if conflict {
            sqlx::query("CREATE TABLE delivery_attempts(conflict INTEGER)")
                .execute(&mut conn)
                .await?;
        }
        conn.close().await?;
        if conflict {
            assert!(matches!(
                Storage::open(&path).await,
                Err(OpenError::Migration(_))
            ));
            let mut conn =
                SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path))
                    .await?;
            let row:(String,i64)=sqlx::query_as("SELECT j.state,l.accepted_at FROM delivery_jobs j JOIN logical_deliveries l ON l.id=j.logical_delivery_id").fetch_one(&mut conn).await?;
            assert_eq!(row, ("accepted".into(), 123));
            let versions: Vec<i64> = sqlx::query_scalar(
                "SELECT version FROM _sqlx_migrations WHERE success=1 ORDER BY version",
            )
            .fetch_all(&mut conn)
            .await?;
            assert_eq!(versions, vec![1, 2, 3]);
            conn.close().await?;
        } else {
            for _ in 0..2 {
                let db = Storage::open(&path).await?;
                let row:(String,String,i64)=sqlx::query_as("SELECT j.state,j.planning_context,l.accepted_at FROM delivery_jobs j JOIN logical_deliveries l ON l.id=j.logical_delivery_id").fetch_one(db.pool()).await?;
                assert_eq!(row, ("accepted".into(), "[null,\"2025b\"]".into(), 123));
                let attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM delivery_attempts")
                    .fetch_one(db.pool())
                    .await?;
                assert_eq!(attempts, 0);
                assert!(sqlx::query("PRAGMA foreign_key_check")
                    .fetch_all(db.pool())
                    .await?
                    .is_empty());
                db.close().await;
            }
        }
    }
    Ok(())
}
