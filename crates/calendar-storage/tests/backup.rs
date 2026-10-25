use calendar_storage::{backup, Storage};
use tempfile::tempdir;
type Result = std::result::Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn wal_snapshot_excludes_uncommitted_data_and_recovers_paused() -> Result {
    let dir = tempdir()?;
    let source = dir.path().join("source");
    let db = Storage::open(&source).await?;
    sqlx::query("INSERT INTO sources(id,kind) VALUES ('committed','test')")
        .execute(db.pool())
        .await?;
    let mut tx = db.pool().begin().await?;
    sqlx::query("INSERT INTO sources(id,kind) VALUES ('uncommitted','test')")
        .execute(&mut *tx)
        .await?;
    let snapshot = dir.path().join("backup");
    backup::backup(&source, &snapshot).await?;
    assert!(
        snapshot.join("manifest.json").is_file(),
        "successful backup must publish a verified manifest"
    );
    let target = dir.path().join("recovered");
    backup::restore(&snapshot, &target, 1000).await?;
    let recovered = Storage::open(target.join("database.sqlite3")).await?;
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM sources ORDER BY id")
        .fetch_all(recovered.pool())
        .await?;
    assert_eq!(ids, ["committed"]);
    let policy: (i64, i64) = sqlx::query_as("SELECT paused,cutoff FROM recovery_control")
        .fetch_one(recovered.pool())
        .await?;
    assert_eq!(policy, (1, 1000));
    recovered.close().await;
    tx.rollback().await?;
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn missing_and_existing_targets_are_rejected_without_touching_source() -> Result {
    let dir = tempdir()?;
    let missing = dir.path().join("missing");
    assert!(backup::backup(&missing, &dir.path().join("backup"))
        .await
        .is_err());
    assert!(!missing.exists());
    let source = dir.path().join("source");
    let db = Storage::open(&source).await?;
    let before = std::fs::read(&source)?;
    assert!(backup::backup(&source, dir.path()).await.is_err());
    assert_eq!(std::fs::read(&source)?, before);
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn damaged_manifest_corruption_and_future_versions_never_publish_recovery() -> Result {
    use sha2::{Digest, Sha256};
    let dir = tempdir()?;
    let source = dir.path().join("source");
    let db = Storage::open(&source).await?;
    sqlx::query("INSERT INTO sources(id,kind) VALUES ('preserved','test')")
        .execute(db.pool())
        .await?;
    let snapshot = dir.path().join("backup");
    backup::backup(&source, &snapshot).await?;
    let original = std::fs::read(snapshot.join("snapshot.sqlite3"))?;
    let manifest = std::fs::read(snapshot.join("manifest.json"))?;
    std::fs::write(snapshot.join("snapshot.sqlite3"), b"broken SQLite")?;
    let bad_target = dir.path().join("bad");
    assert!(backup::restore(&snapshot, &bad_target, 1000).await.is_err());
    assert!(!bad_target.join("database.sqlite3").exists());
    std::fs::write(snapshot.join("snapshot.sqlite3"), &original)?;
    // Even a matching digest cannot make unsupported migration metadata acceptable.
    let altered = Storage::open(snapshot.join("snapshot.sqlite3")).await?;
    sqlx::query("UPDATE _sqlx_migrations SET version=999 WHERE version=9")
        .execute(altered.pool())
        .await?;
    altered.close().await;
    let bytes = std::fs::read(snapshot.join("snapshot.sqlite3"))?;
    let mut value: serde_json::Value = serde_json::from_slice(&manifest)?;
    value["sha256"] = format!("{:x}", Sha256::digest(&bytes)).into();
    std::fs::write(snapshot.join("manifest.json"), serde_json::to_vec(&value)?)?;
    let future = dir.path().join("future");
    assert!(matches!(
        backup::restore(&snapshot, &future, 1000).await,
        Err(backup::Error::Version)
    ));
    assert!(!future.join("database.sqlite3").exists());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM sources WHERE id='preserved'")
        .fetch_one(db.pool())
        .await?;
    assert_eq!(count, 1);
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn orphaned_foreign_keys_are_rejected_before_manifest_publication() -> Result {
    use sqlx::Connection;
    let dir = tempdir()?;
    let source = dir.path().join("source");
    let db = Storage::open(&source).await?;
    db.close().await;
    let mut conn = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&source)
            .foreign_keys(false),
    )
    .await?;
    sqlx::query(
        "INSERT INTO calendars(id,source_id,external_id) VALUES ('orphan','missing','main')",
    )
    .execute(&mut conn)
    .await?;
    conn.close().await?;
    let target = dir.path().join("backup");
    assert!(matches!(
        backup::backup(&source, &target).await,
        Err(backup::Error::Invalid)
    ));
    assert!(!target.join("manifest.json").exists());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn interrupted_output_write_preserves_source_and_never_publishes_manifest() -> Result {
    let dir = tempdir()?;
    let source = dir.path().join("source");
    let db = Storage::open(&source).await?;
    sqlx::query("INSERT INTO sources(id,kind,config_json) VALUES ('large','test',json_object('padding',hex(randomblob(100000))))").execute(db.pool()).await?;
    db.close().await;
    let before = std::fs::read(&source)?;
    let target = dir.path().join("limited");
    // A real OS file-size failure, not a claim to exhaust the host disk.
    let status = std::process::Command::new("bash")
        .args(["-c", "ulimit -f 8; exec \"$@\"", "maintenance"])
        .arg(env!("CARGO_BIN_EXE_calrelay-maintenance"))
        .arg("backup")
        .arg(&source)
        .arg(&target)
        .status()?;
    assert!(!status.success());
    assert!(!target.join("manifest.json").exists());
    assert_eq!(std::fs::read(&source)?, before);
    Ok(())
}

#[tokio::test]
async fn encrypted_credentials_require_separately_supplied_key_after_restore() -> Result {
    use calendar_application::{CredentialProvider, SecretBytes};
    use calendar_storage::security::{KeyRing, SecurityStore};
    use rand_core::{OsRng, RngCore};
    use std::collections::BTreeMap;
    let dir = tempdir()?;
    let mut key = [0u8; 32];
    OsRng.fill_bytes(&mut key);
    let key_path = dir.path().join("independent-key");
    std::fs::write(&key_path, key)?;
    let ring = KeyRing::from_files(1, &BTreeMap::from([(1, key_path)]))?;
    let source = dir.path().join("source");
    let db = Storage::open(&source).await?;
    let store = SecurityStore {
        storage: &db,
        keys: &ring,
    };
    store
        .create_credential(
            "synthetic",
            "bearer",
            &SecretBytes::new(b"synthetic-credential".to_vec()),
            1,
        )
        .await?;
    let snapshot = dir.path().join("backup");
    backup::backup(&source, &snapshot).await?;
    let files: Vec<_> = std::fs::read_dir(&snapshot)?
        .map(|f| f.unwrap().file_name())
        .collect();
    assert_eq!(files.len(), 2);
    let bytes = std::fs::read(snapshot.join("snapshot.sqlite3"))?;
    assert!(!bytes.windows(key.len()).any(|w| w == key));
    assert!(!bytes
        .windows(b"synthetic-credential".len())
        .any(|w| w == b"synthetic-credential"));
    let target = dir.path().join("recovered");
    backup::restore(&snapshot, &target, 1000).await?;
    let recovered = Storage::open(target.join("database.sqlite3")).await?;
    let restored = SecurityStore {
        storage: &recovered,
        keys: &ring,
    };
    assert_eq!(
        restored.credential("synthetic", "bearer").await?.expose(),
        b"synthetic-credential"
    );
    recovered.close().await;
    db.close().await;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn interrupted_recovery_copy_preserves_backup_and_original() -> Result {
    let dir = tempdir()?;
    let source = dir.path().join("source");
    let db = Storage::open(&source).await?;
    sqlx::query("INSERT INTO sources(id,kind,config_json) VALUES ('large','test',json_object('padding',hex(randomblob(100000))))").execute(db.pool()).await?;
    db.close().await;
    let source_before = std::fs::read(&source)?;
    let snapshot = dir.path().join("backup");
    backup::backup(&source, &snapshot).await?;
    let snapshot_before = std::fs::read(snapshot.join("snapshot.sqlite3"))?;
    let target = dir.path().join("limited");
    let status = std::process::Command::new("bash")
        .args(["-c", "ulimit -f 8; exec \"$@\"", "maintenance"])
        .arg(env!("CARGO_BIN_EXE_calrelay-maintenance"))
        .arg("restore")
        .arg(&snapshot)
        .arg(&target)
        .arg("1000")
        .status()?;
    assert!(!status.success());
    assert!(!target.join("database.sqlite3").exists());
    assert_eq!(std::fs::read(&source)?, source_before);
    assert_eq!(
        std::fs::read(snapshot.join("snapshot.sqlite3"))?,
        snapshot_before
    );
    Ok(())
}
