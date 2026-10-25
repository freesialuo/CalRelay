use calendar_storage::{backup, Storage};
use sqlx::{sqlite::SqliteConnectOptions, Connection, SqliteConnection};
type Result = std::result::Result<(), Box<dyn std::error::Error>>;
async fn v10(path: &std::path::Path) -> Result {
    let migrations = tempfile::tempdir()?;
    for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))? {
        let entry = entry?;
        let version: i64 = entry.file_name().to_string_lossy()[..4].parse()?;
        if version <= 10 {
            std::fs::copy(entry.path(), migrations.path().join(entry.file_name()))?;
        }
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
    sqlx::raw_sql("INSERT INTO sources(id,kind,config_json,revision,committed_checkpoint) VALUES('legacy','ics','{\"old_secret_reference\":\"handle\"}',9,'old'); INSERT INTO calendars(id,source_id,external_id) VALUES('legacy-calendar','legacy','main');").execute(&mut conn).await?;
    conn.close().await?;
    Ok(())
}
#[tokio::test]
async fn v10_upgrade_and_backup_preserve_legacy_and_managed_configuration() -> Result {
    use calendar_application::configuration::{ConfigurationCondition, SourceConfigurationV1};
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db");
    v10(&path).await?;
    let db = Storage::open(&path).await?;
    let legacy:(String,i64,i64,Option<String>)=sqlx::query_as("SELECT config_json,config_revision,revision,committed_checkpoint FROM sources WHERE id='legacy'").fetch_one(db.pool()).await?;
    assert_eq!(
        legacy,
        (
            "{\"old_secret_reference\":\"handle\"}".into(),
            0,
            9,
            Some("old".into())
        )
    );
    let selected: bool = sqlx::query_scalar("SELECT selected FROM calendars")
        .fetch_one(db.pool())
        .await?;
    assert!(selected);
    assert!(db.source_configuration("legacy").await.is_err());
    let config: SourceConfigurationV1 = serde_json::from_str(include_str!(
        "../../../examples/source-configuration-v1.json"
    ))?;
    let record = db
        .put_source_configuration(
            "managed",
            ConfigurationCondition::Absent,
            config,
            "alice",
            100,
        )
        .await?;
    let old: Vec<(i64, Vec<u8>)> = sqlx::query_as("SELECT version,checksum FROM _sqlx_migrations")
        .fetch_all(db.pool())
        .await?;
    db.close().await;
    let backup_path = dir.path().join("backup");
    backup::backup(&path, &backup_path).await?;
    let recovered = dir.path().join("restored");
    backup::restore(&backup_path, &recovered, 1000).await?;
    let restored = Storage::open(recovered.join("database.sqlite3")).await?;
    assert_eq!(restored.source_configuration("managed").await?, record);
    let history: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version,checksum FROM _sqlx_migrations")
            .fetch_all(restored.pool())
            .await?;
    assert_eq!(history, old);
    let violations: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(restored.pool())
        .await?;
    assert!(violations.is_empty());
    restored.close().await;
    Storage::open(&path).await?.close().await;
    Ok(())
}
#[tokio::test]
async fn failed_v11_rolls_back_columns_and_preserves_v10_history() -> Result {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db");
    v10(&path).await?;
    let mut conn =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path)).await?;
    sqlx::query("CREATE TABLE configuration_audit(conflict INTEGER)")
        .execute(&mut conn)
        .await?;
    conn.close().await?;
    assert!(Storage::open(&path).await.is_err());
    let mut conn =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path)).await?;
    let latest: i64 = sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations")
        .fetch_one(&mut conn)
        .await?;
    assert_eq!(latest, 10);
    let new_columns: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pragma_table_info('sources') WHERE name='config_revision'",
    )
    .fetch_one(&mut conn)
    .await?;
    assert_eq!(new_columns, 0);
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM sources WHERE id='legacy'")
        .fetch_one(&mut conn)
        .await?;
    assert_eq!(revision, 9);
    conn.close().await?;
    Ok(())
}
