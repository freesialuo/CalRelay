use calendar_storage::{backup, Storage};
use sqlx::{sqlite::SqliteConnectOptions, Connection, SqliteConnection};
type Result = std::result::Result<(), Box<dyn std::error::Error>>;
async fn v11(path: &std::path::Path) -> Result {
    let migrations = tempfile::tempdir()?;
    for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))? {
        let entry = entry?;
        let version: i64 = entry.file_name().to_string_lossy()[..4].parse()?;
        if version <= 11 {
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
    sqlx::raw_sql("INSERT INTO sources(id,kind) VALUES('legacy','test'); INSERT INTO calendars(id,source_id,external_id) VALUES('legacy-calendar','legacy','main'); INSERT INTO outputs(id,kind,config_json) VALUES('legacy-output','custom','{\"unknown\":true}'); INSERT INTO reminder_rules(id,policy_json) VALUES('legacy-rule','{\"custom\":true}'); INSERT INTO rule_outputs(rule_id,output_id) VALUES('legacy-rule','legacy-output'); INSERT INTO events(id,source_id,calendar_id,external_uid,revision,document_json,content_hash) VALUES('e','legacy','legacy-calendar','uid',1,'{}','hash'); INSERT INTO logical_deliveries(id,event_id,original_key,rule_id,output_id,accepted_at) VALUES('l','e','single','legacy-rule','legacy-output',123);").execute(&mut conn).await?;
    conn.close().await?;
    Ok(())
}
#[tokio::test]
async fn v11_upgrade_and_backup_keep_unknown_legacy_configuration_and_accepted_history() -> Result {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db");
    v11(&path).await?;
    let db = Storage::open(&path).await?;
    let row: (String, bool) =
        sqlx::query_as("SELECT config_json,managed FROM outputs WHERE id='legacy-output'")
            .fetch_one(db.pool())
            .await?;
    assert_eq!(row, ("{\"unknown\":true}".into(), false));
    let enabled: bool = sqlx::query_scalar("SELECT enabled FROM rule_outputs")
        .fetch_one(db.pool())
        .await?;
    assert!(enabled);
    assert!(db.bark_output_configuration("legacy-output").await.is_err());
    assert!(db.rule_configuration("legacy-rule").await.is_err());
    db.close().await;
    let destination = dir.path().join("backup");
    backup::backup(&path, &destination).await?;
    let restored = dir.path().join("restored");
    backup::restore(&destination, &restored, 1000).await?;
    let db = Storage::open(restored.join("database.sqlite3")).await?;
    let accepted: Option<i64> = sqlx::query_scalar("SELECT accepted_at FROM logical_deliveries")
        .fetch_one(db.pool())
        .await?;
    assert_eq!(accepted, Some(123));
    let errors: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(db.pool())
        .await?;
    assert!(errors.is_empty());
    db.close().await;
    Ok(())
}
#[tokio::test]
async fn failed_v12_keeps_original_schema_history_and_records() -> Result {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db");
    v11(&path).await?;
    let mut conn =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path)).await?;
    sqlx::query("CREATE TABLE reminder_configuration_audit(conflict INTEGER)")
        .execute(&mut conn)
        .await?;
    conn.close().await?;
    assert!(Storage::open(&path).await.is_err());
    let mut conn =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path)).await?;
    let version: i64 = sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations")
        .fetch_one(&mut conn)
        .await?;
    assert_eq!(version, 11);
    let managed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pragma_table_info('outputs') WHERE name='managed'",
    )
    .fetch_one(&mut conn)
    .await?;
    assert_eq!(managed, 0);
    let accepted: Option<i64> = sqlx::query_scalar("SELECT accepted_at FROM logical_deliveries")
        .fetch_one(&mut conn)
        .await?;
    assert_eq!(accepted, Some(123));
    conn.close().await?;
    Ok(())
}
