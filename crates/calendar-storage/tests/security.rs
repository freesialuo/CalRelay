//! Issue #26 acceptance: real file DB, deterministic times, generated disposable keys.
use calendar_application::{CredentialProvider, SecretBytes, SecurityError};
use calendar_storage::{
    security::{ApiAction, KeyRing, SecurityStore},
    Storage,
};
use rand_core::{OsRng, RngCore};
use std::collections::BTreeMap;
use tempfile::tempdir;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn keys(dir: &std::path::Path, current: i64) -> KeyRing {
    let mut files = BTreeMap::new();
    for version in 1..=2 {
        let path = dir.join(format!("key-{version}"));
        if !path.exists() {
            let mut key = [0u8; 32];
            OsRng.fill_bytes(&mut key);
            std::fs::write(&path, key).unwrap();
        }
        files.insert(version, path);
    }
    KeyRing::from_files(current, &files).unwrap()
}
async fn seed(db: &Storage) -> Result {
    sqlx::query("INSERT INTO sources(id,kind) VALUES ('s1','ics'),('s2','ics')")
        .execute(db.pool())
        .await?;
    Ok(())
}
#[tokio::test]
async fn ac1_encrypted_credentials_survive_reopen_and_rotate_without_old_key() -> Result {
    let dir = tempdir()?;
    let path = dir.path().join("db");
    let ring = keys(dir.path(), 1);
    let db = Storage::open(&path).await?;
    let secret = SecretBytes::new(b"disposable-test-value".to_vec());
    let store = SecurityStore {
        storage: &db,
        keys: &ring,
    };
    store.create_credential("c1", "bearer", &secret, 10).await?;
    store.create_credential("c2", "bearer", &secret, 10).await?;
    let rows: Vec<(Vec<u8>, Vec<u8>, i64)> =
        sqlx::query_as("SELECT ciphertext,nonce,key_version FROM credentials ORDER BY id")
            .fetch_all(db.pool())
            .await?;
    assert_ne!(rows[0].0, secret.expose());
    assert_ne!(rows[0].1, rows[1].1);
    assert_eq!(rows[0].1.len(), 24);
    assert_eq!(rows[0].2, 1);
    assert_eq!(
        store.credential("c1", "bearer").await?.expose(),
        secret.expose()
    );
    assert_eq!(
        store.credential("c1", "basic").await.err(),
        Some(SecurityError::Denied)
    );
    assert!(!format!("{secret:?}").contains("disposable"));
    db.close().await;
    let db = Storage::open(&path).await?;
    let ring = keys(dir.path(), 2);
    let store = SecurityStore {
        storage: &db,
        keys: &ring,
    };
    store.rotate_credential("c1", 0, 20).await?;
    assert_eq!(
        store.rotate_credential("c1", 0, 21).await,
        Err(SecurityError::Conflict)
    );
    let row: (i64, i64, i64) =
        sqlx::query_as("SELECT key_version,revision,rotated_at FROM credentials WHERE id='c1'")
            .fetch_one(db.pool())
            .await?;
    assert_eq!(row, (2, 1, 20));
    let only_new = KeyRing::from_files(2, &BTreeMap::from([(2, dir.path().join("key-2"))]))?;
    let store = SecurityStore {
        storage: &db,
        keys: &only_new,
    };
    assert_eq!(
        store.credential("c1", "bearer").await?.expose(),
        secret.expose()
    );
    assert_eq!(
        store.credential("c2", "bearer").await.err(),
        Some(SecurityError::MissingKey)
    );
    assert_eq!(
        store.rotate_credential("c2", 0, 30).await,
        Err(SecurityError::MissingKey)
    );
    let version: i64 = sqlx::query_scalar("SELECT key_version FROM credentials WHERE id='c2'")
        .fetch_one(db.pool())
        .await?;
    assert_eq!(version, 1);
    db.close().await;
    Ok(())
}
#[tokio::test]
async fn ac3_tampering_identity_kind_nonce_ciphertext_and_version_fail_closed() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    let ring = keys(dir.path(), 1);
    let store = SecurityStore {
        storage: &db,
        keys: &ring,
    };
    let secret = SecretBytes::new(b"synthetic-only".to_vec());
    store.create_credential("a", "bearer", &secret, 0).await?;
    store.create_credential("b", "bearer", &secret, 0).await?;
    let original: (Vec<u8>, Vec<u8>) =
        sqlx::query_as("SELECT ciphertext,nonce FROM credentials WHERE id='b'")
            .fetch_one(db.pool())
            .await?;
    for sql in [
  "UPDATE credentials SET ciphertext=(SELECT ciphertext FROM credentials WHERE id='a'),nonce=(SELECT nonce FROM credentials WHERE id='a') WHERE id='b'",
  "UPDATE credentials SET nonce=randomblob(24) WHERE id='b'",
  "UPDATE credentials SET ciphertext=randomblob(32) WHERE id='b'",
  "UPDATE credentials SET key_version=2 WHERE id='b'",
 ] {
  sqlx::query("UPDATE credentials SET ciphertext=?,nonce=?,key_version=1 WHERE id='b'").bind(&original.0).bind(&original.1).execute(db.pool()).await?;
  sqlx::query(sql).execute(db.pool()).await?;
  let kind:String=sqlx::query_scalar("SELECT kind FROM credentials WHERE id='b'").fetch_one(db.pool()).await?;
  let error=store.credential("b",&kind).await.err().unwrap(); assert_eq!(error,SecurityError::Authentication);
  assert!(!error.to_string().contains("synthetic"));
 }
    // Identity/kind cannot be edited even by ordinary SQL; a copied envelope still fails AAD.
    assert!(
        sqlx::query("UPDATE credentials SET kind='basic' WHERE id='b'")
            .execute(db.pool())
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE credentials SET id='renamed' WHERE id='b'")
            .execute(db.pool())
            .await
            .is_err()
    );
    assert_eq!(
        store.credential("a", "bearer").await?.expose(),
        secret.expose()
    );
    db.close().await;
    Ok(())
}
#[tokio::test]
async fn ac2_api_tokens_digest_only_source_action_expiry_and_revocation() -> Result {
    let dir = tempdir()?;
    let path = dir.path().join("db");
    let db = Storage::open(&path).await?;
    seed(&db).await?;
    let token = db
        .issue_api_token("t", "s1", true, false, Some(100))
        .await?;
    assert_eq!(token.expose().len(), 64); // hex encoding of 32 random bytes
    let digest: Vec<u8> = sqlx::query_scalar("SELECT token_digest FROM api_tokens WHERE id='t'")
        .fetch_one(db.pool())
        .await?;
    assert_eq!(digest.len(), 32);
    assert_ne!(digest, token.expose());
    assert_eq!(
        db.authenticate_api_token(token.expose(), "s1", ApiAction::Upsert, 99)
            .await?,
        "t"
    );
    for (source, action, now) in [
        ("s2", ApiAction::Upsert, 99),
        ("s1", ApiAction::Delete, 99),
        ("s1", ApiAction::Upsert, 100),
    ] {
        assert_eq!(
            db.authenticate_api_token(token.expose(), source, action, now)
                .await,
            Err(SecurityError::Denied)
        );
    }
    assert_eq!(
        db.authenticate_api_token(b"bad", "s1", ApiAction::Upsert, 0)
            .await,
        Err(SecurityError::Denied)
    );
    assert!(db
        .issue_api_token("none", "missing", true, true, None)
        .await
        .is_err());
    assert!(db
        .issue_api_token("empty", "s1", false, false, None)
        .await
        .is_err());
    db.close().await;
    let db = Storage::open(&path).await?;
    assert_eq!(
        db.authenticate_api_token(token.expose(), "s1", ApiAction::Upsert, 99)
            .await?,
        "t"
    );
    db.revoke_api_token("t", 50).await?;
    assert_eq!(
        db.authenticate_api_token(token.expose(), "s1", ApiAction::Upsert, 51)
            .await,
        Err(SecurityError::Denied)
    );
    db.close().await;
    Ok(())
}
#[tokio::test]
async fn ac2_feed_tokens_are_separate_revocable_and_rotation_is_cas() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    let api = db.issue_api_token("api", "s1", true, true, None).await?;
    let feed = db.issue_feed_token("f", "public", "s1", Some(100)).await?;
    assert_eq!(
        db.authenticate_feed_token("public", feed.expose(), 99)
            .await?,
        "s1"
    );
    assert_eq!(
        db.authenticate_feed_token("other", feed.expose(), 99).await,
        Err(SecurityError::Denied)
    );
    assert_eq!(
        db.authenticate_feed_token("public", api.expose(), 99).await,
        Err(SecurityError::Denied)
    );
    assert_eq!(
        db.authenticate_api_token(feed.expose(), "s1", ApiAction::Upsert, 99)
            .await,
        Err(SecurityError::Denied)
    );
    assert_eq!(
        db.authenticate_feed_token("public", feed.expose(), 100)
            .await,
        Err(SecurityError::Denied)
    );
    let (a, b) = tokio::join!(db.rotate_feed_token("f", 0), db.rotate_feed_token("f", 0));
    let fresh = match (a, b) {
        (Ok(t), Err(SecurityError::Conflict)) | (Err(SecurityError::Conflict), Ok(t)) => t,
        _ => panic!("rotation must have one winner"),
    };
    assert_eq!(
        db.authenticate_feed_token("public", feed.expose(), 99)
            .await,
        Err(SecurityError::Denied)
    );
    assert_eq!(
        db.authenticate_feed_token("public", fresh.expose(), 99)
            .await?,
        "s1"
    );
    db.revoke_feed_token("f", 90).await?;
    assert_eq!(
        db.authenticate_feed_token("public", fresh.expose(), 99)
            .await,
        Err(SecurityError::Denied)
    );
    assert!(db.rotate_feed_token("f", 1).await.is_err());
    db.close().await;
    Ok(())
}
#[test]
fn ac1_invalid_key_files_and_versions_report_no_paths_or_bytes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("private-key-name");
    for size in [0, 31, 33, 4096] {
        std::fs::write(&path, vec![42; size]).unwrap();
        let error = KeyRing::from_files(1, &BTreeMap::from([(1, path.clone())]))
            .err()
            .unwrap();
        assert_eq!(error, SecurityError::KeyFile);
        assert!(!format!("{error:?} {error}").contains("private-key-name"));
    }
    assert!(KeyRing::from_files(1, &BTreeMap::new()).is_err());
}

#[tokio::test]
async fn ac1_replacement_conflict_failure_rollback_and_reference_constraints() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    let ring = keys(dir.path(), 1);
    let store = SecurityStore {
        storage: &db,
        keys: &ring,
    };
    let initial = SecretBytes::new(b"initial-disposable".to_vec());
    store.create_credential("c", "bearer", &initial, 0).await?;
    sqlx::query("UPDATE sources SET credential_id='c' WHERE id='s1'")
        .execute(db.pool())
        .await?;
    sqlx::query("INSERT INTO outputs(id,kind,credential_id) VALUES ('o','bark','c')")
        .execute(db.pool())
        .await?;
    for sql in [
        "UPDATE sources SET credential_id='missing' WHERE id='s1'",
        "UPDATE outputs SET credential_id='missing' WHERE id='o'",
        "INSERT INTO outputs(id,kind,credential_id) VALUES ('bad','bark','missing')",
        "DELETE FROM credentials WHERE id='c'",
    ] {
        assert!(sqlx::query(sql).execute(db.pool()).await.is_err());
    }
    let replacement = SecretBytes::new(b"replacement-disposable".to_vec());
    sqlx::query("CREATE TRIGGER fail_rotation BEFORE UPDATE OF ciphertext ON credentials BEGIN SELECT RAISE(ABORT, 'injected failure'); END").execute(db.pool()).await?;
    assert_eq!(
        store.rotate_credential("c", 0, 10).await,
        Err(SecurityError::Storage)
    );
    assert_eq!(
        store.credential("c", "bearer").await?.expose(),
        initial.expose()
    );
    let row: (i64, i64) =
        sqlx::query_as("SELECT revision,rotated_at FROM credentials WHERE id='c'")
            .fetch_one(db.pool())
            .await?;
    assert_eq!(row, (0, 0));
    sqlx::query("DROP TRIGGER fail_rotation")
        .execute(db.pool())
        .await?;
    store.replace_credential("c", 0, &replacement, 20).await?;
    assert_eq!(
        store.replace_credential("c", 0, &initial, 21).await,
        Err(SecurityError::Conflict)
    );
    assert_eq!(
        store.credential("c", "bearer").await?.expose(),
        replacement.expose()
    );
    let wrong_dir = tempdir()?;
    let wrong = keys(wrong_dir.path(), 1);
    let wrong_store = SecurityStore {
        storage: &db,
        keys: &wrong,
    };
    assert_eq!(
        wrong_store.credential("c", "bearer").await.err(),
        Some(SecurityError::Authentication)
    );
    db.close().await;
    Ok(())
}
#[tokio::test]
async fn ac2_disabled_sources_and_unknown_feed_scope_fail_closed() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    let api = db.issue_api_token("a", "s1", false, true, None).await?;
    let feed = db.issue_feed_token("f", "p", "s1", None).await?;
    assert_eq!(
        db.authenticate_api_token(api.expose(), "s1", ApiAction::Delete, 0)
            .await?,
        "a"
    );
    assert_eq!(
        db.authenticate_api_token(api.expose(), "s1", ApiAction::Upsert, 0)
            .await,
        Err(SecurityError::Denied)
    );
    sqlx::query("UPDATE sources SET enabled=0 WHERE id='s1'")
        .execute(db.pool())
        .await?;
    assert_eq!(
        db.authenticate_api_token(api.expose(), "s1", ApiAction::Delete, 0)
            .await,
        Err(SecurityError::Denied)
    );
    assert_eq!(
        db.authenticate_feed_token("p", feed.expose(), 0).await,
        Err(SecurityError::Denied)
    );
    sqlx::query("UPDATE sources SET enabled=1 WHERE id='s1'")
        .execute(db.pool())
        .await?;
    sqlx::query(
        "UPDATE feed_subscriptions SET scope_json='{\"fields\":[\"summary\"]}' WHERE id='f'",
    )
    .execute(db.pool())
    .await?;
    assert_eq!(
        db.authenticate_feed_token("p", feed.expose(), 0).await,
        Err(SecurityError::Denied)
    );
    db.close().await;
    Ok(())
}
#[tokio::test]
async fn ac1_upgrade_v4_is_atomic_and_rejects_dangling_legacy_handles() -> Result {
    use sqlx::{sqlite::SqliteConnectOptions, Connection, SqliteConnection};
    let dir = tempdir()?;
    let migrations = tempdir()?;
    for (name, content) in [
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
    ] {
        std::fs::write(migrations.path().join(name), content)?;
    }
    for dangling in [false, true] {
        let path = dir.path().join(if dangling { "bad" } else { "good" });
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
        sqlx::query("INSERT INTO outputs(id,kind,credential_id) VALUES ('o','bark',?)")
            .bind(if dangling { Some("legacy") } else { None })
            .execute(&mut conn)
            .await?;
        conn.close().await?;
        if dangling {
            assert!(matches!(
                Storage::open(&path).await,
                Err(calendar_storage::OpenError::Migration(_))
            ));
            let mut conn =
                SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path))
                    .await?;
            let handle: String =
                sqlx::query_scalar("SELECT credential_id FROM outputs WHERE id='o'")
                    .fetch_one(&mut conn)
                    .await?;
            assert_eq!(handle, "legacy");
            let version: i64 =
                sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations WHERE success=1")
                    .fetch_one(&mut conn)
                    .await?;
            assert_eq!(version, 4);
            let tables: i64 =
                sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE name='credentials'")
                    .fetch_one(&mut conn)
                    .await?;
            assert_eq!(tables, 0);
            conn.close().await?;
        } else {
            let db = Storage::open(&path).await?;
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM outputs WHERE id='o'")
                .fetch_one(db.pool())
                .await?;
            assert_eq!(count, 1);
            assert!(sqlx::query("PRAGMA foreign_key_check")
                .fetch_all(db.pool())
                .await?
                .is_empty());
            db.close().await;
        }
    }
    Ok(())
}

#[tokio::test]
async fn ac3_aad_binds_kind_and_version_even_with_reused_key_material() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    let _ = keys(dir.path(), 1);
    let path = dir.path().join("key-1");
    let ring = KeyRing::from_files(1, &BTreeMap::from([(1, path.clone()), (2, path)]))?;
    let store = SecurityStore {
        storage: &db,
        keys: &ring,
    };
    store
        .create_credential("c", "bearer", &SecretBytes::new(b"synthetic".to_vec()), 0)
        .await?;
    // Simulate offline database tampering: SQL triggers are not a cryptographic boundary.
    sqlx::query("DROP TRIGGER credential_identity_immutable")
        .execute(db.pool())
        .await?;
    sqlx::query("UPDATE credentials SET kind='basic' WHERE id='c'")
        .execute(db.pool())
        .await?;
    assert_eq!(
        store.credential("c", "basic").await.err(),
        Some(SecurityError::Authentication)
    );
    sqlx::query("UPDATE credentials SET kind='bearer',key_version=2 WHERE id='c'")
        .execute(db.pool())
        .await?;
    assert_eq!(
        store.credential("c", "bearer").await.err(),
        Some(SecurityError::Authentication)
    );
    sqlx::query("UPDATE credentials SET key_version=1,id='renamed' WHERE id='c'")
        .execute(db.pool())
        .await?;
    assert_eq!(
        store.credential("renamed", "bearer").await.err(),
        Some(SecurityError::Authentication)
    );
    db.close().await;
    Ok(())
}
