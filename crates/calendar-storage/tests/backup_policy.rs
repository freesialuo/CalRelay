//! Recovery policy checks use temporary SQLite files and synthetic delivery state only.
use calendar_application::{Clock, DeliveryPolicy, DeliveryRepository};
use calendar_storage::{backup, Storage};
use sqlx::{sqlite::SqliteConnectOptions, Connection, SqliteConnection};
use tempfile::tempdir;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Time(i64);
impl Clock for Time {
    fn utc_now_ms(&self) -> i64 {
        self.0
    }
    fn monotonic_now_ms(&self) -> u64 {
        0
    }
}

async fn seed(db: &Storage) -> Result {
    sqlx::raw_sql(r#"
        INSERT INTO sources(id,kind,revision,last_verified_at) VALUES ('s','synthetic',1,1000);
        INSERT INTO calendars(id,source_id,external_id) VALUES ('c','s','main');
        INSERT INTO events(id,source_id,calendar_id,external_uid,revision,document_json,content_hash)
          VALUES ('e','s','c','a',1,'{"schema_version":"1.0","external_uid":"a","title":"Synthetic","status":"confirmed","start":{"kind":"utc","value":"2026-11-02T17:00:00Z"},"duration":"PT1H"}','h');
        INSERT INTO outputs(id,kind) VALUES ('o','synthetic');
        INSERT INTO reminder_rules(id,policy_json) VALUES ('r','{"calendar_ids":["c"]}');
        INSERT INTO rule_outputs VALUES ('r','o');
        INSERT INTO planning_watermarks(scope_id,source_id,rule_id,output_id,source_revision,rule_revision,output_revision,planned_until,tzdata_version)
          VALUES ('w','s','r','o',1,1,1,20000,'2025b');
        INSERT INTO logical_deliveries(id,event_id,original_key,rule_id,output_id)
          VALUES ('old','e','old','r','o'),('equal','e','equal','r','o'),('future','e','future','r','o');
        INSERT INTO delivery_jobs(id,logical_delivery_id,generation,event_revision,rule_revision,output_revision,due_at,state,next_attempt_at,planning_context)
          VALUES ('old','old',1,1,1,1,999,'pending',999,'[null,"2025b"]'),
                 ('equal','equal',1,1,1,1,1000,'pending',1000,'[null,"2025b"]'),
                 ('future','future',1,1,1,1,2000,'pending',2000,'[null,"2025b"]');
    "#).execute(db.pool()).await?;
    Ok(())
}

#[tokio::test]
async fn recovered_database_stays_paused_across_reopen_and_resume_keeps_cutoff() -> Result {
    let dir = tempdir()?;
    let original = dir.path().join("original.sqlite3");
    let snapshot = dir.path().join("backup");
    let destination = dir.path().join("isolated");
    let db = Storage::open(&original).await?;
    seed(&db).await?;
    backup::backup(&original, &snapshot).await?;
    backup::restore(&snapshot, &destination, 1000).await?;
    let restored = destination.join("database.sqlite3");
    for _ in 0..2 {
        let recovered = Storage::open(&restored).await?;
        // A paused recovery may return no lease or a classified denial, never a lease.
        assert!(!matches!(
            recovered
                .claim("synthetic", &DeliveryPolicy::default(), &Time(2000))
                .await,
            Ok(Some(_))
        ));
        let attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM delivery_attempts")
            .fetch_one(recovered.pool())
            .await?;
        assert_eq!(attempts, 0, "pause must not commit a partial claim");
        recovered.close().await;
    }
    backup::resume(&restored).await?;
    let recovered = Storage::open(&restored).await?;
    let mut claimed = Vec::new();
    for _ in 0..2 {
        claimed.push(
            recovered
                .claim("synthetic", &DeliveryPolicy::default(), &Time(2000))
                .await?
                .expect("cutoff equality and future jobs remain eligible")
                .job_id,
        );
    }
    claimed.sort();
    assert_eq!(claimed, ["equal", "future"]);
    assert!(recovered
        .claim("synthetic", &DeliveryPolicy::default(), &Time(2000))
        .await?
        .is_none());
    let old: String = sqlx::query_scalar("SELECT state FROM delivery_jobs WHERE id='old'")
        .fetch_one(recovered.pool())
        .await?;
    assert_eq!(old, "expired");
    let original_states: Vec<String> =
        sqlx::query_scalar("SELECT state FROM delivery_jobs ORDER BY id")
            .fetch_all(db.pool())
            .await?;
    assert_eq!(
        original_states,
        ["pending", "pending", "pending"],
        "recovery must never alter the source database"
    );
    recovered.close().await;
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn recovered_inflight_lease_cannot_prepare_or_record_send_intent() -> Result {
    let dir = tempdir()?;
    let original = dir.path().join("original.sqlite3");
    let snapshot = dir.path().join("backup");
    let destination = dir.path().join("isolated");
    let db = Storage::open(&original).await?;
    seed(&db).await?;
    sqlx::query("UPDATE delivery_jobs SET state='expired' WHERE id IN ('old','equal')")
        .execute(db.pool())
        .await?;
    let lease = db
        .claim("old-worker", &DeliveryPolicy::default(), &Time(2000))
        .await?
        .unwrap();
    assert_eq!(lease.job_id, "future");
    backup::backup(&original, &snapshot).await?;
    backup::restore(&snapshot, &destination, 1000).await?;
    let recovered = Storage::open(destination.join("database.sqlite3")).await?;
    assert!(!matches!(
        recovered
            .prepare(&lease, &DeliveryPolicy::default(), &Time(2001))
            .await,
        Ok(Some(_))
    ));
    let send_intents: i64 =
        sqlx::query_scalar("SELECT count(*) FROM delivery_attempts WHERE sent_at IS NOT NULL")
            .fetch_one(recovered.pool())
            .await?;
    assert_eq!(
        send_intents, 0,
        "blocked prepare must roll back its send intent"
    );
    backup::resume(&destination.join("database.sqlite3")).await?;
    assert!(!matches!(
        recovered
            .prepare(&lease, &DeliveryPolicy::default(), &Time(2001))
            .await,
        Ok(Some(_))
    ));
    recovered.close().await;
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn restoring_cancelled_inflight_attempt_preserves_terminal_disposition_and_clears_lease(
) -> Result {
    let dir = tempdir()?;
    let original = dir.path().join("original.sqlite3");
    let snapshot = dir.path().join("backup");
    let destination = dir.path().join("isolated");
    let db = Storage::open(&original).await?;
    seed(&db).await?;
    sqlx::query("UPDATE delivery_jobs SET state='expired' WHERE id IN ('old','equal')")
        .execute(db.pool())
        .await?;
    let lease = db
        .claim("original-worker", &DeliveryPolicy::default(), &Time(2000))
        .await?
        .unwrap();
    assert!(db
        .prepare(&lease, &DeliveryPolicy::default(), &Time(2001))
        .await?
        .is_some());
    // Cancellation may race with an already prepared external request (#24 contract).
    sqlx::query("UPDATE delivery_jobs SET state='cancelled' WHERE id='future'")
        .execute(db.pool())
        .await?;
    backup::backup(&original, &snapshot).await?;
    backup::restore(&snapshot, &destination, 3000).await?;
    let recovered = Storage::open(destination.join("database.sqlite3")).await?;
    let audit: (String, String, String) = sqlx::query_as("SELECT j.state,a.disposition,a.outcome FROM delivery_jobs j JOIN delivery_attempts a ON a.job_id=j.id WHERE j.id='future'")
        .fetch_one(recovered.pool()).await?;
    assert_eq!(
        audit,
        ("cancelled".into(), "cancelled".into(), "unknown".into())
    );
    let lease_fields: (Option<String>, Option<String>, Option<i64>) = sqlx::query_as(
        "SELECT lease_owner,lease_token,lease_until FROM delivery_jobs WHERE id='future'",
    )
    .fetch_one(recovered.pool())
    .await?;
    assert_eq!(lease_fields, (None, None, None));
    let source: (String, Option<String>) =
        sqlx::query_as("SELECT state,lease_token FROM delivery_jobs WHERE id='future'")
            .fetch_one(db.pool())
            .await?;
    assert_eq!(source, ("cancelled".into(), Some(lease.token)));
    recovered.close().await;
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn pausing_a_live_lease_blocks_prepare_and_rolls_back_send_intent() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("paused.sqlite3")).await?;
    seed(&db).await?;
    sqlx::query("UPDATE delivery_jobs SET state='expired' WHERE id IN ('old','equal')")
        .execute(db.pool())
        .await?;
    let lease = db
        .claim("synthetic", &DeliveryPolicy::default(), &Time(2000))
        .await?
        .unwrap();
    let before: (Option<i64>, Option<i64>) = sqlx::query_as("SELECT a.sent_at,j.lease_until FROM delivery_attempts a JOIN delivery_jobs j ON j.id=a.job_id WHERE j.id='future'").fetch_one(db.pool()).await?;
    sqlx::query("UPDATE recovery_control SET paused=1,cutoff=1000 WHERE id=1")
        .execute(db.pool())
        .await?;
    assert!(db
        .prepare(&lease, &DeliveryPolicy::default(), &Time(2001))
        .await
        .is_err());
    let after: (Option<i64>, Option<i64>) = sqlx::query_as("SELECT a.sent_at,j.lease_until FROM delivery_attempts a JOIN delivery_jobs j ON j.id=a.job_id WHERE j.id='future'").fetch_one(db.pool()).await?;
    assert_eq!(
        after, before,
        "guard rejection must roll back sent_at and lease renewal together"
    );
    assert_eq!(after.0, None);
    // The exact same lease is valid when pause is explicitly lifted; other fixture state is ready.
    backup::resume(&dir.path().join("paused.sqlite3")).await?;
    assert!(db
        .prepare(&lease, &DeliveryPolicy::default(), &Time(2001))
        .await?
        .is_some());
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn replanning_cannot_resurrect_jobs_before_persisted_cutoff() -> Result {
    let dir = tempdir()?;
    let original = dir.path().join("original.sqlite3");
    let snapshot = dir.path().join("backup");
    let destination = dir.path().join("isolated");
    let db = Storage::open(&original).await?;
    seed(&db).await?;
    backup::backup(&original, &snapshot).await?;
    backup::restore(&snapshot, &destination, 1000).await?;
    let path = destination.join("database.sqlite3");
    backup::resume(&path).await?;
    let recovered = Storage::open(&path).await?;
    sqlx::query("INSERT INTO delivery_jobs(id,logical_delivery_id,generation,event_revision,rule_revision,output_revision,due_at,state,next_attempt_at,planning_context) VALUES ('replanned','old',2,1,1,1,999,'pending',999,'[null,\"2025b\"]')").execute(recovered.pool()).await?;
    sqlx::query("UPDATE delivery_jobs SET state='retry_wait' WHERE id='old'")
        .execute(recovered.pool())
        .await?;
    let states: Vec<String> = sqlx::query_scalar(
        "SELECT state FROM delivery_jobs WHERE id IN ('old','replanned') ORDER BY id",
    )
    .fetch_all(recovered.pool())
    .await?;
    assert_eq!(states, ["expired", "expired"]);
    recovered.close().await;
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn v6_upgrades_to_recovery_policy_without_pausing_normal_database() -> Result {
    let dir = tempdir()?;
    let historical = tempdir()?;
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
        (
            "0004_delivery_attempts.sql",
            include_str!("../migrations/0004_delivery_attempts.sql"),
        ),
        (
            "0005_security.sql",
            include_str!("../migrations/0005_security.sql"),
        ),
        (
            "0006_source_snapshot.sql",
            include_str!("../migrations/0006_source_snapshot.sql"),
        ),
    ] {
        std::fs::write(historical.path().join(name), source)?;
    }
    let path = dir.path().join("v6.sqlite3");
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true),
    )
    .await?;
    sqlx::migrate::Migrator::new(historical.path())
        .await?
        .run(&mut connection)
        .await?;
    sqlx::query("INSERT INTO sources(id,kind) VALUES ('legacy','synthetic')")
        .execute(&mut connection)
        .await?;
    connection.close().await?;
    // Back up the actual historical database before any current Storage::open upgrades it.
    let snapshot = dir.path().join("v6-backup");
    let destination = dir.path().join("v6-isolated");
    backup::backup(&path, &snapshot).await?;
    backup::restore(&snapshot, &destination, 1000).await?;
    let recovered = Storage::open(destination.join("database.sqlite3")).await?;
    let recovered_versions: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success=1 ORDER BY version")
            .fetch_all(recovered.pool())
            .await?;
    assert_eq!(
        recovered_versions,
        sqlx::migrate!("./migrations")
            .iter()
            .map(|m| m.version)
            .collect::<Vec<_>>()
    );
    let recovery_policy: (bool, Option<i64>) =
        sqlx::query_as("SELECT paused,cutoff FROM recovery_control WHERE id=1")
            .fetch_one(recovered.pool())
            .await?;
    assert_eq!(recovery_policy, (true, Some(1000)));
    let recovered_legacy: String = sqlx::query_scalar("SELECT kind FROM sources WHERE id='legacy'")
        .fetch_one(recovered.pool())
        .await?;
    assert_eq!(recovered_legacy, "synthetic");
    assert!(sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(recovered.pool())
        .await?
        .is_empty());
    recovered.close().await;
    let mut original = SqliteConnection::connect_with(
        &SqliteConnectOptions::new().filename(&path).read_only(true),
    )
    .await?;
    let original_max: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&mut original)
        .await?;
    assert_eq!(
        original_max, 6,
        "isolated recovery must not migrate the source"
    );
    original.close().await?;
    for _ in 0..2 {
        let db = Storage::open(&path).await?;
        let legacy: String = sqlx::query_scalar("SELECT kind FROM sources WHERE id='legacy'")
            .fetch_one(db.pool())
            .await?;
        assert_eq!(legacy, "synthetic");
        assert!(sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(db.pool())
            .await?
            .is_empty());
        let versions: Vec<i64> = sqlx::query_scalar(
            "SELECT version FROM _sqlx_migrations WHERE success=1 ORDER BY version",
        )
        .fetch_all(db.pool())
        .await?;
        assert_eq!(
            versions,
            sqlx::migrate!("./migrations")
                .iter()
                .map(|m| m.version)
                .collect::<Vec<_>>()
        );
        db.close().await;
    }
    let db = Storage::open(&path).await?;
    seed(&db).await?;
    assert_eq!(
        db.claim("normal", &DeliveryPolicy::default(), &Time(999))
            .await?
            .unwrap()
            .job_id,
        "old"
    );
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn restoring_a_recovery_again_cannot_lower_its_historical_cutoff() -> Result {
    let dir = tempdir()?;
    let original = dir.path().join("original.sqlite3");
    let snapshot = dir.path().join("backup");
    let first = dir.path().join("first-isolated");
    let second_snapshot = dir.path().join("recovery-backup");
    let second = dir.path().join("second-isolated");
    let db = Storage::open(&original).await?;
    seed(&db).await?;
    backup::backup(&original, &snapshot).await?;
    backup::restore(&snapshot, &first, 1500).await?;
    let first_path = first.join("database.sqlite3");
    backup::resume(&first_path).await?;
    backup::backup(&first_path, &second_snapshot).await?;
    backup::restore(&second_snapshot, &second, 1000).await?;
    let second_path = second.join("database.sqlite3");
    let recovered = Storage::open(&second_path).await?;
    let policy: (bool, Option<i64>) =
        sqlx::query_as("SELECT paused,cutoff FROM recovery_control WHERE id=1")
            .fetch_one(recovered.pool())
            .await?;
    assert_eq!(policy, (true, Some(1500)));
    backup::resume(&second_path).await?;
    sqlx::query("UPDATE delivery_jobs SET state='pending' WHERE id='equal'")
        .execute(recovered.pool())
        .await?;
    let state: String = sqlx::query_scalar("SELECT state FROM delivery_jobs WHERE id='equal'")
        .fetch_one(recovered.pool())
        .await?;
    assert_eq!(
        state, "expired",
        "due_at=1000 must not be resurrected by weaker requested cutoff"
    );
    let lease = recovered
        .claim("synthetic", &DeliveryPolicy::default(), &Time(2000))
        .await?
        .unwrap();
    assert_eq!(lease.job_id, "future");
    recovered.close().await;
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn sqlite_full_rolls_back_transaction_and_preserves_preexisting_data() -> Result {
    let dir = tempdir()?;
    let path = dir.path().join("full.sqlite3");
    let db = Storage::open(&path).await?;
    seed(&db).await?;
    db.close().await;
    let mut connection =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path)).await?;
    let pages: i64 = sqlx::query_scalar("PRAGMA page_count")
        .fetch_one(&mut connection)
        .await?;
    // PRAGMA assignment cannot bind parameters; pages is an i64 read from SQLite.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "PRAGMA max_page_count={pages}"
    )))
    .execute(&mut connection)
    .await?;
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut connection)
        .await?;
    sqlx::query("UPDATE sources SET kind='must-roll-back' WHERE id='s'")
        .execute(&mut connection)
        .await?;
    let error = sqlx::query("UPDATE sources SET config_json=? WHERE id='s'")
        .bind(format!("\"{}\"", "x".repeat(1_000_000)))
        .execute(&mut connection)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("13"),
        "fault must be SQLITE_FULL, not an unrelated fixture error"
    );
    // SQLITE_FULL may have rolled the transaction back automatically.
    let _ = sqlx::query("ROLLBACK").execute(&mut connection).await;
    connection.close().await?;
    let reopened = Storage::open(&path).await?;
    let kind: String = sqlx::query_scalar("SELECT kind FROM sources WHERE id='s'")
        .fetch_one(reopened.pool())
        .await?;
    assert_eq!(kind, "synthetic");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM delivery_jobs")
        .fetch_one(reopened.pool())
        .await?;
    assert_eq!(count, 3);
    assert_eq!(
        sqlx::query_scalar::<_, String>("PRAGMA integrity_check")
            .fetch_one(reopened.pool())
            .await?,
        "ok"
    );
    reopened.close().await;
    Ok(())
}
