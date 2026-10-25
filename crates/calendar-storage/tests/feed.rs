//! #32 repository authorization, upgrade after 0009, and backup compatibility.
use calendar_application::SecurityError;
use calendar_storage::{backup, Storage};
use sqlx::{Connection, Executor};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
#[tokio::test]
async fn authorization_uses_digest_expiry_scope_source_and_rotation() -> Result {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db");
    let db = Storage::open(&path).await?;
    sqlx::query("INSERT INTO sources(id,kind) VALUES ('s','test'),('other','test')")
        .execute(db.pool())
        .await?;
    let token = db.issue_feed_token("sub", "public", "s", Some(100)).await?;
    let stored: Vec<u8> = sqlx::query_scalar("SELECT token_digest FROM feed_subscriptions")
        .fetch_one(db.pool())
        .await?;
    assert_eq!(stored.len(), 32);
    assert_ne!(stored, token.expose());
    let key = db.authorize_feed("public", token.expose(), 99).await?;
    assert_eq!(key.key.source_id, "s");
    assert_eq!(key.key.configuration_revision, 0);
    assert_eq!(
        db.authorize_feed("public", token.expose(), 100).await,
        Err(SecurityError::Denied)
    );
    assert_eq!(
        db.authorize_feed("other", token.expose(), 99).await,
        Err(SecurityError::Denied)
    );
    let api = db.issue_api_token("api", "s", true, true, None).await?;
    assert_eq!(
        db.authorize_feed("public", api.expose(), 99).await,
        Err(SecurityError::Denied)
    );
    sqlx::query("UPDATE feed_subscriptions SET scope_json='{\"unknown\":true}'")
        .execute(db.pool())
        .await?;
    assert_eq!(
        db.authorize_feed("public", token.expose(), 99).await,
        Err(SecurityError::Denied)
    );
    sqlx::query("UPDATE feed_subscriptions SET scope_json='{}'")
        .execute(db.pool())
        .await?;
    let key = db.authorize_feed("public", token.expose(), 99).await?;
    assert_eq!(key.key.configuration_revision, 2);
    let (a, b) = tokio::join!(
        db.rotate_feed_token("sub", 2),
        db.rotate_feed_token("sub", 2)
    );
    let fresh = match (a, b) {
        (Ok(t), Err(SecurityError::Conflict)) | (Err(SecurityError::Conflict), Ok(t)) => t,
        _ => panic!("one CAS winner"),
    };
    assert_eq!(
        db.authorize_feed("public", token.expose(), 99).await,
        Err(SecurityError::Denied)
    );
    db.close().await;
    let db = Storage::open(&path).await?;
    assert_eq!(
        db.authorize_feed("public", fresh.expose(), 99)
            .await?
            .key
            .configuration_revision,
        3
    );
    db.revoke_feed_token("sub", 1).await?;
    assert_eq!(
        db.authorize_feed("public", fresh.expose(), 0).await,
        Err(SecurityError::Denied)
    );
    assert!(db.rotate_feed_token("sub", 3).await.is_err());
    db.close().await;
    Ok(())
}
#[tokio::test]
async fn out_of_order_0008_upgrade_preserves_0009_checksums_and_restores() -> Result {
    for had_ingest in [false, true] {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("db");
        let mut conn = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await?;
        // Reconstruct the exact deployed history (0009 already applied, no 0008).
        static MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
        conn.execute("CREATE TABLE _sqlx_migrations(version BIGINT PRIMARY KEY,description TEXT NOT NULL,installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,success BOOLEAN NOT NULL,checksum BLOB NOT NULL,execution_time BIGINT NOT NULL)").await?;
        for m in MIGRATIONS
            .iter()
            .filter(|m| m.version <= 9 && m.version != 8 && (m.version != 7 || had_ingest))
        {
            conn.execute(m.sql.as_ref()).await?;
            sqlx::query("INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time) VALUES (?,?,1,?,0)").bind(m.version).bind(m.description.as_ref()).bind(m.checksum.as_ref()).execute(&mut conn).await?;
        }
        conn.close().await?;
        // A valid pre-upgrade backup must remain accepted by the new binary.
        let old_backup = dir.path().join("old-backup");
        backup::backup(&path, &old_backup).await?;
        let recovered = dir.path().join("old-restored");
        backup::restore(&old_backup, &recovered, 1000).await?;
        let old = Storage::open(recovered.join("database.sqlite3")).await?;
        old.close().await;
        let db = Storage::open(&path).await?;
        let versions: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
                .fetch_all(db.pool())
                .await?;
        assert_eq!(
            versions,
            MIGRATIONS.iter().map(|m| m.version).collect::<Vec<_>>()
        );
        let upgraded_backup = dir.path().join("upgraded-backup");
        backup::backup(&path, &upgraded_backup).await?;
        let recovered = dir.path().join("restored");
        backup::restore(&upgraded_backup, &recovered, 1000).await?;
        let restored = Storage::open(recovered.join("database.sqlite3")).await?;
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM sqlite_master WHERE type='trigger' AND name='feed_configuration_revision'").fetch_one(restored.pool()).await?,1);
        restored.close().await;
        db.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn scoped_repository_owns_calendars_and_keeps_legacy_source_only_consumers_closed() -> Result
{
    use calendar_application::FeedScope;
    let dir = tempfile::tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    sqlx::query("INSERT INTO sources(id,kind) VALUES ('s','test'),('other','test')")
        .execute(db.pool())
        .await?;
    sqlx::query("INSERT INTO calendars(id,source_id,external_id) VALUES ('c','s','c'),('foreign','other','c')").execute(db.pool()).await?;
    let token = db.issue_feed_token("sub", "p", "s", None).await?;
    let scoped =
        FeedScope::parse(r#"{"version":1,"calendar_ids":["c"],"visible_fields":["summary"]}"#)?;
    let foreign =
        FeedScope::parse(r#"{"version":1,"calendar_ids":["foreign"],"visible_fields":[]}"#)?;
    assert_eq!(
        db.configure_feed_scope("sub", 0, &foreign).await,
        Err(SecurityError::Denied)
    );
    assert!(matches!(
        db.issue_scoped_feed_token("bad", "bad", "s", None, &foreign)
            .await,
        Err(SecurityError::Denied)
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM feed_subscriptions WHERE id='bad'")
            .fetch_one(db.pool())
            .await?,
        0
    );
    let scoped_token = db
        .issue_scoped_feed_token("restricted", "restricted", "s", None, &scoped)
        .await?;
    let issued = db
        .authorize_feed("restricted", scoped_token.expose(), 0)
        .await?;
    assert_eq!(issued.scope, scoped);
    assert_eq!(issued.key.configuration_revision, 0);

    assert_eq!(
        db.authorize_feed("p", token.expose(), 0)
            .await?
            .key
            .configuration_revision,
        0
    );
    db.configure_feed_scope("sub", 0, &scoped).await?;
    assert_eq!(
        db.configure_feed_scope("sub", 0, &FeedScope::Legacy).await,
        Err(SecurityError::Conflict)
    );
    let auth = db.authorize_feed("p", token.expose(), 0).await?;
    assert_eq!(auth.scope, scoped);
    assert_eq!(auth.key.configuration_revision, 1);
    assert_eq!(
        db.authenticate_feed_token("p", token.expose(), 0).await,
        Err(SecurityError::Denied)
    );
    // Corrupted/imported cross-source policy is rejected at read time too.
    sqlx::query("UPDATE feed_subscriptions SET scope_json=?")
        .bind(foreign.to_json())
        .execute(db.pool())
        .await?;
    assert_eq!(
        db.authorize_feed("p", token.expose(), 0).await,
        Err(SecurityError::Denied)
    );
    assert!(db.feed_snapshot("p", token.expose(), 0).await.is_err());
    db.revoke_feed_token("sub", 0).await?;
    assert_eq!(
        db.configure_feed_scope("sub", 2, &FeedScope::Legacy).await,
        Err(SecurityError::Conflict)
    );
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn scoped_snapshot_excludes_other_calendars_and_never_mixes_committed_revisions() -> Result {
    use calendar_application::FeedScope;
    let dir = tempfile::tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    sqlx::query("INSERT INTO sources(id,kind,revision) VALUES ('s','test',1),('other','test',1)")
        .execute(db.pool())
        .await?;
    sqlx::query("INSERT INTO calendars(id,source_id,external_id) VALUES ('c','s','c'),('c2','s','c2'),('foreign','other','c')").execute(db.pool()).await?;
    for (id, source, calendar) in [
        ("e", "s", "c"),
        ("hidden", "s", "c2"),
        ("foreign", "other", "foreign"),
    ] {
        sqlx::query("INSERT INTO events(id,source_id,calendar_id,external_uid,revision,document_json,content_hash) VALUES (?,?,?,?,1,'{\"title\":\"old\"}','old')").bind(id).bind(source).bind(calendar).bind(id).execute(db.pool()).await?;
        sqlx::query("INSERT INTO event_revisions(event_id,revision,document_json,change_kind,changed_at) VALUES (?,1,'{\"title\":\"old\"}','upsert',1000)").bind(id).execute(db.pool()).await?;
    }
    let token = db.issue_feed_token("sub", "p", "s", None).await?;
    let scoped = FeedScope::parse(r#"{"version":1,"calendar_ids":["c"],"visible_fields":[]}"#)?;
    db.configure_feed_scope("sub", 0, &scoped).await?;
    let mut writer = db.pool().begin().await?;
    sqlx::query("UPDATE sources SET revision=2 WHERE id='s'")
        .execute(&mut *writer)
        .await?;
    sqlx::query("UPDATE events SET revision=2,document_json='{\"title\":\"new\"}' WHERE id='e'")
        .execute(&mut *writer)
        .await?;
    sqlx::query("INSERT INTO event_revisions(event_id,revision,document_json,change_kind,changed_at) VALUES ('e',2,'{\"title\":\"new\"}','upsert',2000)").execute(&mut *writer).await?;
    let before = db.feed_snapshot("p", token.expose(), 0).await?;
    assert_eq!(before.authorization.key.data_revision, 1);
    assert_eq!(before.records.len(), 1);
    assert_eq!(
        (
            &*before.records[0].event_id,
            before.records[0].revision,
            before.records[0].changed_at
        ),
        ("e", 1, 1000)
    );
    assert!(before.records[0].document_json.contains("old"));
    writer.commit().await?;
    let after = db.feed_snapshot("p", token.expose(), 0).await?;
    assert_eq!(after.authorization.key.data_revision, 2);
    assert_eq!(after.records.len(), 1);
    assert_eq!(
        (after.records[0].revision, after.records[0].changed_at),
        (2, 2000)
    );
    assert!(after.records[0].document_json.contains("new"));
    // Missing revision history is a whole-feed failure, never silent event omission.
    sqlx::query("DELETE FROM event_revisions WHERE event_id='e' AND revision=2")
        .execute(db.pool())
        .await?;
    assert!(matches!(
        db.feed_snapshot("p", token.expose(), 0).await,
        Err(SecurityError::Storage)
    ));
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn snapshot_budgets_reject_whole_feed_instead_of_silent_truncation() -> Result {
    let dir = tempfile::tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    sqlx::query("INSERT INTO sources(id,kind) VALUES ('s','test')")
        .execute(db.pool())
        .await?;
    sqlx::query("INSERT INTO calendars(id,source_id,external_id) VALUES ('c','s','c')")
        .execute(db.pool())
        .await?;
    let token = db.issue_feed_token("sub", "p", "s", None).await?;
    sqlx::query("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<10001) INSERT INTO events(id,source_id,calendar_id,external_uid,revision,document_json,content_hash) SELECT CAST(i AS TEXT),'s','c',CAST(i AS TEXT),1,'{}','hash' FROM n").execute(db.pool()).await?;
    sqlx::query("INSERT INTO event_revisions(event_id,revision,document_json,change_kind,changed_at) SELECT id,1,'{}','upsert',1000 FROM events").execute(db.pool()).await?;
    assert!(matches!(
        db.feed_snapshot("p", token.expose(), 0).await,
        Err(SecurityError::Storage)
    ));
    sqlx::query("DELETE FROM event_revisions WHERE event_id!='1'")
        .execute(db.pool())
        .await?;
    sqlx::query("DELETE FROM events WHERE id!='1'")
        .execute(db.pool())
        .await?;
    assert_eq!(
        db.feed_snapshot("p", token.expose(), 0)
            .await?
            .records
            .len(),
        1
    );
    sqlx::query("UPDATE events SET document_json=json_object('large',printf('%.*c',16777217,'x')) WHERE id='1'").execute(db.pool()).await?;
    assert!(matches!(
        db.feed_snapshot("p", token.expose(), 0).await,
        Err(SecurityError::Storage)
    ));
    db.close().await;
    Ok(())
}
