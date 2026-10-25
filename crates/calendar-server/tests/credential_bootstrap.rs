use calendar_application::{CredentialProvider, SecretBytes, SecurityError};
use calendar_server::DeploymentConfig;
use calendar_storage::security::SecurityStore;
use serde_json::{json, Value};
fn document(key: &std::path::Path) -> Value {
    json!({"bind":"127.0.0.1:0","credentials":{"current_key_version":1,"key_files":{"1":key}}})
}
fn parse(
    value: &Value,
    db: &std::path::Path,
) -> Result<DeploymentConfig, calendar_server::ConfigError> {
    DeploymentConfig::from_deployment(&serde_json::to_vec(value).unwrap(), |k| {
        (k == "CALRELAY_DATABASE").then(|| db.to_str().unwrap().into())
    })
}
#[tokio::test]
async fn runtime_provider_is_shared_read_only_and_returns_record_revision() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("key");
    std::fs::write(&key, [21u8; 32]).unwrap();
    let path = dir.path().join("db");
    let config = parse(&document(&key), &path).unwrap();
    let server = config.initialize().await.unwrap();
    let keys = calendar_storage::security::KeyRing::from_files(1, &[(1, key)].into()).unwrap();
    SecurityStore {
        storage: &server.services.storage,
        keys: &keys,
    }
    .create_credential(
        "bark-main",
        "bark",
        &SecretBytes::new(b"synthetic-bark".to_vec()),
        1,
    )
    .await
    .unwrap();
    let provider = server.services.credential_provider().unwrap();
    let second = provider.clone();
    let value = second.credential("bark-main", "bark").await.unwrap();
    assert_eq!(value.expose(), b"synthetic-bark");
    assert_eq!(value.revision(), Some(0));
    assert!(!format!("{value:?}").contains("synthetic-bark"));
    assert!(matches!(
        provider.credential("bark-main", "oauth2").await,
        Err(SecurityError::Denied)
    ));
    let services = server.services.clone();
    assert!(server.with_credential_keys(keys).is_err());
    services.drain().await;
    assert!(provider.credential("bark-main", "bark").await.is_err());
}
#[test]
fn key_configuration_is_strict_but_parse_only_never_opens_keys_or_database() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("missing-key");
    let db = dir.path().join("missing-db");
    let original = document(&key);
    let parsed = parse(&original, &db).unwrap();
    assert_eq!(parsed.server.bind, "127.0.0.1:0".parse().unwrap());
    assert!(!db.exists());
    for value in [
        json!(null),
        json!({}),
        json!({"current_key_version":0,"key_files":{"1":key}}),
        json!({"current_key_version":2,"key_files":{"1":key}}),
        json!({"current_key_version":1,"key_files":{"0":key}}),
        json!({"current_key_version":1,"key_files":{"01":key}}),
        json!({"current_key_version":1,"key_files":{"1":"relative"}}),
        json!({"current_key_version":1,"key_files":{"1":key},"raw_key":"forged"}),
    ] {
        let mut candidate = original.clone();
        candidate["credentials"] = value;
        assert!(parse(&candidate, &db).is_err());
        assert!(!db.exists());
    }
    let mut too_many = original.clone();
    let mut files = serde_json::Map::new();
    for n in 1..=17 {
        files.insert(n.to_string(), json!(&key));
    }
    too_many["credentials"]["key_files"] = serde_json::Value::Object(files);
    assert!(parse(&too_many, &db).is_err());
    let text = original.to_string();
    let duplicate = text.replace("\"1\":", "\"1\":\"/synthetic/other\",\"1\":");
    assert_ne!(duplicate, text);
    assert!(
        DeploymentConfig::from_deployment(duplicate.as_bytes(), |k| (k == "CALRELAY_DATABASE")
            .then(|| db.to_str().unwrap().into()))
        .is_err()
    );
}
#[tokio::test]
async fn missing_corrupt_and_nonregular_keys_fail_before_database_creation() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("key");
    let db = dir.path().join("db");
    for bytes in [vec![], vec![1; 31], vec![1; 33]] {
        std::fs::write(&key, bytes).unwrap();
        assert!(parse(&document(&key), &db)
            .unwrap()
            .initialize()
            .await
            .is_err());
        assert!(!db.exists());
    }
    let missing = dir.path().join("missing");
    assert!(parse(&document(&missing), &db)
        .unwrap()
        .initialize()
        .await
        .is_err());
    assert!(!db.exists());
    #[cfg(unix)]
    {
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&key, &link).unwrap();
        assert!(parse(&document(&link), &db)
            .unwrap()
            .initialize()
            .await
            .is_err());
        let fifo = dir.path().join("fifo");
        assert!(std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        assert!(parse(&document(&fifo), &db)
            .unwrap()
            .initialize()
            .await
            .is_err());
        assert!(!db.exists());
    }
}
#[tokio::test]
async fn absent_keys_do_not_create_a_default_provider() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    let server = parse(&json!({"bind":"127.0.0.1:0"}), &db)
        .unwrap()
        .initialize()
        .await
        .unwrap();
    assert!(matches!(
        server.services.credential_provider(),
        Err(SecurityError::MissingKey)
    ));
    server.services.drain().await;
}

#[tokio::test]
async fn wrong_material_and_missing_record_key_fail_without_plaintext_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("key");
    std::fs::write(&key, [71u8; 32]).unwrap();
    let path = dir.path().join("db");
    let first = parse(&document(&key), &path)
        .unwrap()
        .initialize()
        .await
        .unwrap();
    let keys = calendar_storage::security::KeyRing::from_files(1, &[(1, key)].into()).unwrap();
    SecurityStore {
        storage: &first.services.storage,
        keys: &keys,
    }
    .create_credential(
        "bark-main",
        "bark",
        &SecretBytes::new(b"synthetic-bark".to_vec()),
        1,
    )
    .await
    .unwrap();
    first.services.drain().await;
    let wrong = dir.path().join("wrong");
    std::fs::write(&wrong, [73u8; 32]).unwrap();
    let server = parse(&document(&wrong), &path)
        .unwrap()
        .initialize()
        .await
        .unwrap();
    let provider = server.services.credential_provider().unwrap();
    assert!(matches!(
        provider.credential("bark-main", "bark").await,
        Err(SecurityError::Authentication)
    ));
    server.services.drain().await;
    let value = json!({"bind":"127.0.0.1:0","credentials":{"current_key_version":2,"key_files":{"2":wrong}}});
    let server = parse(&value, &path).unwrap().initialize().await.unwrap();
    let provider = server.services.credential_provider().unwrap();
    assert!(matches!(
        provider.credential("bark-main", "bark").await,
        Err(SecurityError::MissingKey)
    ));
    server.services.drain().await;
}
