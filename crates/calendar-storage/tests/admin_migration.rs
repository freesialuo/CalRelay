//! Real historical v9 database, migration checksum compatibility and isolated backup/restore.
use calendar_storage::{backup, Storage};
use sqlx::{sqlite::SqliteConnectOptions, Connection, SqliteConnection};
type Result = std::result::Result<(), Box<dyn std::error::Error>>;
async fn v9(path: &std::path::Path) -> Result {
    let migrations = tempfile::tempdir()?;
    for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))? {
        let entry = entry?;
        let version: i64 = entry.file_name().to_string_lossy()[..4].parse()?;
        if version > 6 && version != 9 {
            continue;
        }
        std::fs::copy(entry.path(), migrations.path().join(entry.file_name()))?;
    }
    let mut conn = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true),
    )
    .await?;
    sqlx::migrate::Migrator::new(migrations.path())
        .await?
        .run(&mut conn)
        .await?;
    sqlx::query("INSERT INTO sources(id,kind) VALUES ('preserved','synthetic')")
        .execute(&mut conn)
        .await?;
    conn.close().await?;
    Ok(())
}
#[tokio::test]
async fn v9_upgrade_and_historical_and_current_backups_keep_checksums_and_data() -> Result {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db");
    v9(&path).await?;
    let mut conn =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path)).await?;
    let old: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version,checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&mut conn)
            .await?;
    assert_eq!(
        old.iter().map(|(v, _)| *v).collect::<Vec<_>>(),
        [1, 2, 3, 4, 5, 6, 9]
    );
    conn.close().await?;
    let old_backup = dir.path().join("old-backup");
    backup::backup(&path, &old_backup).await?;
    let recovered = dir.path().join("old-recovered");
    backup::restore(&old_backup, &recovered, 1000).await?;
    let restored = Storage::open(recovered.join("database.sqlite3")).await?;
    let paused: bool = sqlx::query_scalar("SELECT paused FROM recovery_control WHERE id=1")
        .fetch_one(restored.pool())
        .await?;
    assert!(paused);
    restored.close().await;
    let db = Storage::open(&path).await?;
    let history: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version,checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(db.pool())
            .await?;
    for (version, checksum) in &old {
        assert_eq!(
            history.iter().find(|(v, _)| v == version).map(|(_, c)| c),
            Some(checksum)
        );
    }
    assert_eq!(history.last().unwrap().0, 10);
    let grant = db
        .create_admin_session("alice", "policy", "challenge", 100, 200)
        .await?;
    let current_backup = dir.path().join("current-backup");
    backup::backup(&path, &current_backup).await?;
    let current_recovered = dir.path().join("current-recovered");
    backup::restore(&current_backup, &current_recovered, 1000).await?;
    let restored = Storage::open(current_recovered.join("database.sqlite3")).await?;
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM sources WHERE id='preserved'")
        .fetch_one(restored.pool())
        .await?;
    assert_eq!(rows, 1);
    let token = std::str::from_utf8(grant.session.expose())?;
    restored
        .check_admin_session(token, "alice", "policy", 101)
        .await?;
    assert!(restored
        .create_admin_session("alice", "policy", "challenge", 101, 200)
        .await
        .is_err());
    restored.close().await;
    db.close().await;
    Storage::open(&path).await?.close().await;
    Ok(())
}
#[tokio::test]
async fn failed_v10_is_atomic_and_keeps_historical_data_and_history() -> Result {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db");
    v9(&path).await?;
    let mut conn =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path)).await?;
    sqlx::query("CREATE TABLE admin_challenge_uses(conflict INTEGER)")
        .execute(&mut conn)
        .await?;
    conn.close().await?;
    assert!(Storage::open(&path).await.is_err());
    let mut conn =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path)).await?;
    let latest: i64 = sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations")
        .fetch_one(&mut conn)
        .await?;
    assert_eq!(latest, 9);
    let tables: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE name='admin_sessions'")
            .fetch_one(&mut conn)
            .await?;
    assert_eq!(tables, 0);
    let preserved: i64 = sqlx::query_scalar("SELECT count(*) FROM sources WHERE id='preserved'")
        .fetch_one(&mut conn)
        .await?;
    assert_eq!(preserved, 1);
    conn.close().await?;
    Ok(())
}
