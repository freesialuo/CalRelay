use calendar_application::{
    aggregate::*,
    configuration::{ConfigurationCondition as Condition, SourceConfigurationV1},
    SecretBytes,
};
use calendar_storage::Storage;
use serde_json::json;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
async fn source(db: &Storage, id: &str, calendar: &str) -> Result {
    let mut c: SourceConfigurationV1 = serde_json::from_str(include_str!(
        "../../../examples/source-configuration-v1.json"
    ))?;
    c.calendars[0].id = calendar.into();
    db.put_source_configuration(id, Condition::Absent, c.clone(), "synthetic", 1)
        .await?;
    c.enabled = true;
    db.put_source_configuration(id, Condition::Revision(1), c, "synthetic", 2)
        .await?;
    let document=json!({"schema_version":"1.0","external_uid":"same-upstream","title":id,"status":"confirmed","start":{"kind":"utc","value":"2026-01-01T00:00:00Z"},"duration":"PT1H"}).to_string();
    sqlx::query("INSERT INTO events(id,source_id,calendar_id,external_uid,revision,document_json,content_hash) VALUES (?,?,?,'same-upstream',1,?,'hash')").bind(format!("event-{id}")).bind(id).bind(calendar).bind(&document).execute(db.pool()).await?;
    sqlx::query("INSERT INTO event_revisions(event_id,revision,document_json,change_kind,changed_at) VALUES (?,1,?,'upsert',1767225600000)").bind(format!("event-{id}")).bind(document).execute(db.pool()).await?;
    Ok(())
}
fn configuration() -> AggregateConfiguration {
    serde_json::from_value(json!({"version":1,"name":"Combined","enabled":false,"sources":[{"source_id":"s1","calendar_ids":["c1"]},{"source_id":"s2","calendar_ids":["c2"]}],"visible_fields":["summary"],"stale_policy":"reject","expires_at":null})).unwrap()
}
async fn subscription(db: &Storage) -> Result<(AggregateRecord, SecretBytes)> {
    let mut c = configuration();
    let (r, secret) = db
        .put_aggregate("sub", Condition::Absent, c.clone(), "synthetic", 3)
        .await?;
    assert_eq!(r.revision, 1);
    c.enabled = true;
    let (r, unchanged) = db
        .put_aggregate("sub", Condition::Revision(1), c, "synthetic", 4)
        .await?;
    assert!(unchanged.is_none());
    Ok((r, secret.unwrap()))
}
#[tokio::test]
async fn managed_aggregate_reads_both_sources_without_uid_dedup_and_keeps_token_cas() -> Result {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db");
    let db = Storage::open(&path).await?;
    source(&db, "s1", "c1").await?;
    source(&db, "s2", "c2").await?;
    let (r, token) = subscription(&db).await?;
    let auth = db
        .authorize_aggregate(&r.public_id, token.expose(), 10)
        .await?;
    assert_eq!(auth.key.sources.len(), 2);
    let snap = db
        .aggregate_snapshot(&r.public_id, token.expose(), 10)
        .await?;
    assert_eq!(snap.events.len(), 2);
    assert_ne!(snap.events[0].event_id, snap.events[1].event_id);
    let raw: Vec<u8> = sqlx::query_scalar("SELECT token_digest FROM aggregate_subscriptions")
        .fetch_one(db.pool())
        .await?;
    assert_eq!(raw.len(), 32);
    assert_ne!(raw, token.expose());
    let api = db
        .issue_api_token("writer", "s1", true, false, None)
        .await?;
    assert_eq!(
        db.authorize_aggregate(&r.public_id, api.expose(), 10).await,
        Err(AggregateError::Denied)
    );
    let legacy = db.issue_feed_token("legacy", "legacy", "s1", None).await?;
    assert_eq!(
        db.authorize_aggregate(&r.public_id, legacy.expose(), 10)
            .await,
        Err(AggregateError::Denied)
    );
    let (a, b) = tokio::join!(
        db.rotate_aggregate("sub", 2, "synthetic", 11),
        db.rotate_aggregate("sub", 2, "synthetic", 11)
    );
    let fresh = match (a, b) {
        (Ok(v), Err(AggregateError::Conflict)) | (Err(AggregateError::Conflict), Ok(v)) => v,
        _ => panic!("one rotation winner"),
    };
    assert_eq!(
        db.authorize_aggregate(&r.public_id, token.expose(), 12)
            .await,
        Err(AggregateError::Denied)
    );
    db.close().await;
    let db = Storage::open(&path).await?;
    db.authorize_aggregate(&r.public_id, fresh.secret.expose(), 12)
        .await?;
    db.retire_aggregate("sub", 3, "synthetic", 13).await?;
    assert_eq!(
        db.authorize_aggregate(&r.public_id, fresh.secret.expose(), 0)
            .await,
        Err(AggregateError::Denied)
    );
    assert!(db
        .rotate_aggregate("sub", 4, "synthetic", 14)
        .await
        .is_err());
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn policy_validation_freshness_and_whole_member_failure_are_persistent() -> Result {
    use calendar_application::configuration::{AdapterKind, SourceMode};
    let dir = tempfile::tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    source(&db, "s1", "c1").await?;
    source(&db, "s2", "c2").await?;
    let (r, token) = subscription(&db).await?;
    let mut bad = r.configuration.clone();
    bad.sources[0].calendar_ids = vec!["c2".into()];
    assert!(matches!(
        db.put_aggregate("sub", Condition::Revision(2), bad, "test", 5)
            .await,
        Err(AggregateError::Denied)
    ));
    assert_eq!(db.aggregate("sub").await?.revision, 2);
    let mut source = db.source_configuration("s1").await?.configuration;
    source.mode = SourceMode::Pull;
    source.endpoint = Some("https://synthetic.example.test/calendar.ics".into());
    source.refresh_interval_ms = Some(60000);
    source.freshness_sla_ms = Some(60000);
    db.put_source_configuration("s1", Condition::Revision(2), source.clone(), "test", 5)
        .await?;
    assert_eq!(
        db.authorize_aggregate(&r.public_id, token.expose(), 10)
            .await,
        Err(AggregateError::Unavailable)
    );
    sqlx::query("UPDATE sources SET snapshot_revision=revision,last_verified_at=10 WHERE id='s1'")
        .execute(db.pool())
        .await?;
    db.authorize_aggregate(&r.public_id, token.expose(), 60010)
        .await?;
    assert_eq!(
        db.authorize_aggregate(&r.public_id, token.expose(), 60011)
            .await,
        Err(AggregateError::Unavailable)
    );
    assert_eq!(
        db.authorize_aggregate(&r.public_id, token.expose(), 9)
            .await,
        Err(AggregateError::Unavailable)
    );
    source.mode = SourceMode::PushOnly;
    source.adapter = AdapterKind::StandardCalendar;
    source.endpoint = None;
    source.refresh_interval_ms = None;
    source.freshness_sla_ms = None;
    db.put_source_configuration("s1", Condition::Revision(3), source.clone(), "test", 6)
        .await?;
    db.authorize_aggregate(&r.public_id, token.expose(), i64::MAX)
        .await?;
    source.freshness_sla_ms = Some(60000);
    db.put_source_configuration("s1", Condition::Revision(4), source, "test", 7)
        .await?;
    assert_eq!(
        db.authorize_aggregate(&r.public_id, token.expose(), 60011)
            .await,
        Err(AggregateError::Unavailable)
    );
    sqlx::query("UPDATE sources SET last_verified_at=10 WHERE id='s1'")
        .execute(db.pool())
        .await?;
    sqlx::query("UPDATE calendars SET selected=0 WHERE id='c2'")
        .execute(db.pool())
        .await?;
    assert_eq!(
        db.authorize_aggregate(&r.public_id, token.expose(), 10)
            .await,
        Err(AggregateError::Denied)
    );
    sqlx::query("UPDATE calendars SET selected=1 WHERE id='c2'")
        .execute(db.pool())
        .await?;
    db.retire_source_configuration("s2", 2, "test", 8).await?;
    assert_eq!(
        db.authorize_aggregate(&r.public_id, token.expose(), 10)
            .await,
        Err(AggregateError::Denied)
    );
    // Empty selection is explicit and valid, even after a formerly selected source retires.
    let mut empty = configuration();
    empty.sources.clear();
    empty.visible_fields.clear();
    let (_, secret) = db
        .put_aggregate("empty", Condition::Absent, empty.clone(), "test", 9)
        .await?;
    empty.enabled = true;
    let (empty, _) = db
        .put_aggregate("empty", Condition::Revision(1), empty, "test", 10)
        .await?;
    assert!(db
        .aggregate_snapshot(&empty.public_id, secret.as_ref().unwrap().expose(), 11)
        .await?
        .events
        .is_empty());
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn aggregate_snapshot_is_atomic_across_sources_and_global_budgets() -> Result {
    let dir = tempfile::tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    source(&db, "s1", "c1").await?;
    source(&db, "s2", "c2").await?;
    let (r, token) = subscription(&db).await?;
    let before = db
        .aggregate_snapshot(&r.public_id, token.expose(), 10)
        .await?;
    let mut writer = db.pool().begin().await?;
    sqlx::query("UPDATE sources SET revision=revision+1")
        .execute(&mut *writer)
        .await?;
    sqlx::query(
        "UPDATE events SET revision=2,document_json=json_set(document_json,'$.title','new')",
    )
    .execute(&mut *writer)
    .await?;
    sqlx::query("INSERT INTO event_revisions(event_id,revision,document_json,change_kind,changed_at) SELECT id,revision,document_json,'upsert',1767225601000 FROM events").execute(&mut *writer).await?;
    let during = db
        .aggregate_snapshot(&r.public_id, token.expose(), 10)
        .await?;
    assert_eq!(before.authorization.key, during.authorization.key);
    assert!(during.events.iter().all(|e| e.revision == 1));
    writer.commit().await?;
    let after = db
        .aggregate_snapshot(&r.public_id, token.expose(), 10)
        .await?;
    assert_ne!(before.authorization.key, after.authorization.key);
    assert!(after.events.iter().all(|e| e.revision == 2));
    // Each member independently fits the input budget; their sum must fail.
    sqlx::query("UPDATE events SET document_json=json_object('large',printf('%.*c',8388608,'x'))")
        .execute(db.pool())
        .await?;
    assert!(matches!(
        db.aggregate_snapshot(&r.public_id, token.expose(), 10)
            .await,
        Err(AggregateError::Unavailable)
    ));
    sqlx::query("UPDATE events SET document_json='{}'")
        .execute(db.pool())
        .await?;
    sqlx::query("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<10000) INSERT INTO events(id,source_id,calendar_id,external_uid,revision,document_json,content_hash) SELECT 'extra-'||i,CASE WHEN i%2=0 THEN 's1' ELSE 's2' END,CASE WHEN i%2=0 THEN 'c1' ELSE 'c2' END,CAST(i AS TEXT),1,'{}','hash' FROM n").execute(db.pool()).await?;
    sqlx::query("INSERT INTO event_revisions(event_id,revision,document_json,change_kind,changed_at) SELECT id,1,'{}','upsert',1000 FROM events WHERE id LIKE 'extra-%'").execute(db.pool()).await?;
    assert!(matches!(
        db.aggregate_snapshot(&r.public_id, token.expose(), 10)
            .await,
        Err(AggregateError::Unavailable)
    ));
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn upgrade_after_0012_and_backup_restore_preserve_legacy_and_aggregate_capabilities() -> Result
{
    use calendar_storage::backup;
    use sqlx::{Connection, Executor};
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db");
    let mut conn = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true),
    )
    .await?;
    static MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
    conn.execute("CREATE TABLE _sqlx_migrations(version BIGINT PRIMARY KEY,description TEXT NOT NULL,installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,success BOOLEAN NOT NULL,checksum BLOB NOT NULL,execution_time BIGINT NOT NULL)").await?;
    for m in MIGRATIONS.iter().filter(|m| m.version <= 12) {
        conn.execute(m.sql.as_ref()).await?;
        sqlx::query("INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time) VALUES (?,?,1,?,0)").bind(m.version).bind(m.description.as_ref()).bind(m.checksum.as_ref()).execute(&mut conn).await?;
    }
    conn.close().await?;
    let old = dir.path().join("old");
    backup::backup(&path, &old).await?;
    let restored_old = dir.path().join("old-restored");
    backup::restore(&old, &restored_old, 1000).await?;
    let old_db = Storage::open(restored_old.join("database.sqlite3")).await?;
    old_db.close().await;
    let db = Storage::open(&path).await?;
    source(&db, "s1", "c1").await?;
    source(&db, "s2", "c2").await?;
    let (r, secret) = subscription(&db).await?;
    let legacy = db
        .issue_feed_token("legacy", "legacy-public", "s1", None)
        .await?;
    let snapshot = dir.path().join("snapshot");
    backup::backup(&path, &snapshot).await?;
    let restored = dir.path().join("restored");
    backup::restore(&snapshot, &restored, 1000).await?;
    let other = Storage::open(restored.join("database.sqlite3")).await?;
    other
        .authorize_aggregate(&r.public_id, secret.expose(), 10)
        .await?;
    assert_eq!(
        other
            .authenticate_feed_token("legacy-public", legacy.expose(), 10)
            .await?,
        "s1"
    );
    let versions: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(other.pool())
            .await?;
    assert_eq!(
        versions,
        MIGRATIONS.iter().map(|m| m.version).collect::<Vec<_>>()
    );
    other.close().await;
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn audit_failure_rolls_back_policy_members_rotation_and_retirement() -> Result {
    let dir = tempfile::tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    source(&db, "s1", "c1").await?;
    source(&db, "s2", "c2").await?;
    let (r, token) = subscription(&db).await?;
    sqlx::query("CREATE TRIGGER fail_aggregate_audit BEFORE INSERT ON aggregate_subscription_audit BEGIN SELECT RAISE(ABORT,'synthetic failure'); END").execute(db.pool()).await?;
    let mut changed = r.configuration.clone();
    changed.sources.clear();
    changed.visible_fields.clear();
    assert!(matches!(
        db.put_aggregate("sub", Condition::Revision(2), changed, "test", 12)
            .await,
        Err(AggregateError::Storage)
    ));
    assert_eq!(db.aggregate("sub").await?, r);
    assert!(matches!(
        db.rotate_aggregate("sub", 2, "test", 12).await,
        Err(AggregateError::Storage)
    ));
    assert!(matches!(
        db.retire_aggregate("sub", 2, "test", 12).await,
        Err(AggregateError::Storage)
    ));
    assert_eq!(
        db.authorize_aggregate(&r.public_id, token.expose(), 13)
            .await?
            .key
            .sources
            .len(),
        2
    );
    assert_eq!(db.aggregate("sub").await?, r);
    db.close().await;
    Ok(())
}
