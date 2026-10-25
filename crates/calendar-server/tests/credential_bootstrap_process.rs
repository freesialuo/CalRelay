#![cfg(unix)]
mod common;
use serde_json::{json, Value};
use std::process::{Command, Output};
fn invoke(
    config: &std::path::Path,
    db: &std::path::Path,
    id: &str,
    secret: &std::path::Path,
    revision: Option<i64>,
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_calendar-server"));
    command.env_clear().env("CALRELAY_DATABASE", db).args([
        "credential",
        "provision-bark",
        "--deployment-config",
        config.to_str().unwrap(),
        "--credential-id",
        id,
        "--secret-file",
        secret.to_str().unwrap(),
    ]);
    if let Some(n) = revision {
        command.args(["--expected-revision", &n.to_string()]);
    }
    command.output().unwrap()
}
#[test]
fn standard_binary_provisions_without_listening_or_loading_turnstile() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("key");
    std::fs::write(&key, [17u8; 32]).unwrap();
    let secret = dir.path().join("bark");
    std::fs::write(&secret, b"synthetic-bark").unwrap();
    let config = dir.path().join("deployment.json");
    let db = dir.path().join("db");
    std::fs::write(&config,json!({"bind":"127.0.0.1:0","credentials":{"current_key_version":1,"key_files":{"1":key}},"admin":{"team":"synthetic-team","audience":"synthetic-aud","origin":"https://admin.example.test","hostname":"admin.example.test","action":"admin-login","site_key":"synthetic-site","administrators":{"synthetic-sub":"admin@example.test"},"session_seconds":120,"turnstile_secret_file":dir.path().join("must-not-read")}}).to_string()).unwrap();
    let output = invoke(&config, &db, "bark-main", &secret, None);
    assert!(
        output.status.success(),
        "credential command must succeed with synthetic files: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt, json!({"credential_id":"bark-main","revision":0}));
    assert!(db.exists());
}

fn rotate(config: &std::path::Path, db: &std::path::Path, id: &str, revision: i64) -> Output {
    Command::new(env!("CARGO_BIN_EXE_calendar-server"))
        .env_clear()
        .env("CALRELAY_DATABASE", db)
        .args([
            "credential",
            "rotate-bark",
            "--deployment-config",
            config.to_str().unwrap(),
            "--credential-id",
            id,
            "--expected-revision",
            &revision.to_string(),
        ])
        .output()
        .unwrap()
}
fn assert_denied(output: Output) {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let text = String::from_utf8(output.stderr).unwrap();
    assert!(text.starts_with("invalid or missing server configuration:"));
    assert!(!text.contains("synthetic-"));
    assert!(!text.contains("/tmp/"));
    assert!(!text.contains("SQL"));
}
#[test]
fn provision_replace_concurrent_cas_rotate_and_reopen_never_expose_secret() {
    use calendar_application::{CredentialProvider, SecretBytes};
    use calendar_storage::{
        security::{KeyRing, SecurityStore},
        Storage,
    };
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("deployment.json");
    let dbpath = dir.path().join("db");
    let key1 = dir.path().join("key1");
    let key2 = dir.path().join("key2");
    let secret = dir.path().join("bark");
    std::fs::write(&key1, [31u8; 32]).unwrap();
    std::fs::write(&key2, [47u8; 32]).unwrap();
    std::fs::write(&secret, b"synthetic-first").unwrap();
    std::fs::write(&config,json!({"bind":"127.0.0.1:0","credentials":{"current_key_version":1,"key_files":{"1":key1}}}).to_string()).unwrap();
    assert!(invoke(&config, &dbpath, "bark-main", &secret, None)
        .status
        .success());
    assert_denied(invoke(&config, &dbpath, "bark-main", &secret, None));
    std::fs::write(&secret, b"synthetic-second").unwrap();
    let output = invoke(&config, &dbpath, "bark-main", &secret, Some(0));
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"credential_id":"bark-main","revision":1})
    );
    assert_denied(invoke(&config, &dbpath, "bark-main", &secret, Some(0)));
    let (a, b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| invoke(&config, &dbpath, "bark-main", &secret, Some(1)));
        let b = scope.spawn(|| invoke(&config, &dbpath, "bark-main", &secret, Some(1)));
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_ne!(a.status.success(), b.status.success());
    let loser = if a.status.success() { b } else { a };
    assert!(String::from_utf8(loser.stderr)
        .unwrap()
        .contains("credential operation denied or failed"));
    std::fs::write(&config,json!({"bind":"127.0.0.1:0","credentials":{"current_key_version":2,"key_files":{"1":key1,"2":key2}}}).to_string()).unwrap();
    let output = rotate(&config, &dbpath, "bark-main", 2);
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"credential_id":"bark-main","revision":3})
    );
    assert_denied(rotate(&config, &dbpath, "bark-main", 2));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let db = Storage::open(&dbpath).await.unwrap();
        let row: (String, i64, i64, Vec<u8>) = sqlx::query_as(
            "SELECT kind,key_version,revision,ciphertext FROM credentials WHERE id='bark-main'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!((&row.0, row.1, row.2), (&"bark".to_string(), 2, 3));
        assert!(!row
            .3
            .windows(b"synthetic-second".len())
            .any(|w| w == b"synthetic-second"));
        let keys = KeyRing::from_files(2, &[(2, key2.clone())].into()).unwrap();
        let store = SecurityStore {
            storage: &db,
            keys: &keys,
        };
        let value = store.credential("bark-main", "bark").await.unwrap();
        assert_eq!(value.expose(), b"synthetic-second");
        assert_eq!(value.revision(), Some(3));
        let mut output: Value = serde_json::from_str(include_str!(
            "../../../examples/bark-output-configuration-v1.json"
        ))
        .unwrap();
        output["credential_id"] = json!("bark-main");
        let output = serde_json::from_value(output).unwrap();
        let receipt = db
            .put_bark_output_configuration(
                "managed-bark",
                calendar_application::configuration::ConfigurationCondition::Absent,
                output,
                "synthetic-operator",
                1,
            )
            .await
            .unwrap();
        assert_eq!(receipt.configuration.credential_id, "bark-main");
        store
            .create_credential(
                "other",
                "oauth2",
                &SecretBytes::new(b"synthetic-oauth".to_vec()),
                1,
            )
            .await
            .unwrap();
        db.close().await;
    });
    assert_denied(invoke(&config, &dbpath, "other", &secret, Some(0)));
    assert_denied(rotate(&config, &dbpath, "other", 0));
    // Current-key-only server bootstrap can still decrypt the rotated record.
    std::fs::write(&config,json!({"bind":"127.0.0.1:0","credentials":{"current_key_version":2,"key_files":{"2":key2}}}).to_string()).unwrap();
    runtime.block_on(async {
        let parsed = calendar_server::DeploymentConfig::from_deployment(
            &std::fs::read(&config).unwrap(),
            |k| (k == "CALRELAY_DATABASE").then(|| dbpath.to_str().unwrap().into()),
        )
        .unwrap();
        let server = parsed.initialize().await.unwrap();
        let provider = server.services.credential_provider().unwrap();
        assert_eq!(
            provider
                .credential("bark-main", "bark")
                .await
                .unwrap()
                .expose(),
            b"synthetic-second"
        );
        server.services.drain().await;
    });
}

#[test]
fn malformed_cli_or_secret_fails_before_creating_database_and_never_echoes_values() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("key");
    std::fs::write(&key, [53u8; 32]).unwrap();
    let secret = dir.path().join("synthetic-secret-file");
    let db = dir.path().join("must-not-exist");
    let config = dir.path().join("deployment.json");
    std::fs::write(
        &config,
        json!({"bind":"127.0.0.1:0","credentials":{"current_key_version":1,"key_files":{"1":key}}})
            .to_string(),
    )
    .unwrap();
    for value in [
        vec![],
        vec![b'x'; 4097],
        b"synthetic-value\n".to_vec(),
        vec![0xff],
    ] {
        std::fs::write(&secret, value).unwrap();
        assert_denied(invoke(&config, &db, "bark-main", &secret, None));
        assert!(!db.exists());
    }
    for extra in [
        vec!["--secret", "synthetic-inline"],
        vec!["--credential-id", "duplicate"],
        vec!["--expected-revision", "-1"],
        vec!["--expected-revision", "01"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_calendar-server"))
            .env_clear()
            .env("CALRELAY_DATABASE", &db)
            .args([
                "credential",
                "provision-bark",
                "--deployment-config",
                config.to_str().unwrap(),
                "--credential-id",
                "bark-main",
                "--secret-file",
                secret.to_str().unwrap(),
            ])
            .args(extra)
            .output()
            .unwrap();
        assert_denied(output);
        assert!(!db.exists());
    }
    let output = Command::new(env!("CARGO_BIN_EXE_calendar-server"))
        .env_clear()
        .env("CALRELAY_DATABASE", &db)
        .args([
            "credential",
            "rotate-bark",
            "--deployment-config",
            config.to_str().unwrap(),
            "--credential-id",
            "bark-main",
        ])
        .output()
        .unwrap();
    assert_denied(output);
    assert!(!db.exists());
}

#[test]
fn sigterm_does_not_reload_removed_key_or_resume_recovery() {
    use std::{
        io::{BufRead, BufReader},
        process::Stdio,
        sync::mpsc,
        time::{Duration, Instant},
    };
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("key");
    std::fs::write(&key, [61u8; 32]).unwrap();
    let secret = dir.path().join("secret");
    std::fs::write(&secret, b"synthetic-bark").unwrap();
    let config = dir.path().join("deployment.json");
    let db = dir.path().join("db");
    std::fs::write(
        &config,
        json!({"bind":"127.0.0.1:0","credentials":{"current_key_version":1,"key_files":{"1":key}}})
            .to_string(),
    )
    .unwrap();
    assert!(invoke(&config, &db, "bark-main", &secret, None)
        .status
        .success());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let storage = calendar_storage::Storage::open(&db).await.unwrap();
        sqlx::query("UPDATE recovery_control SET paused=1,cutoff=42 WHERE id=1")
            .execute(storage.pool())
            .await
            .unwrap();
        storage.close().await;
    });
    let mut serving: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    serving["admin"] = common::deployment(dir.path(), "127.0.0.1:0")["admin"].clone();
    std::fs::write(&config, serving.to_string()).unwrap();
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_calendar-server"))
            .env_clear()
            .env("CALRELAY_DATABASE", &db)
            .args(["--deployment-config", config.to_str().unwrap()])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stderr = child.0.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stderr).read_line(&mut line);
        let _ = tx.send(line);
    });
    let line = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(line.starts_with("CalRelay listening on "));
    std::fs::rename(&key, dir.path().join("removed-after-bootstrap")).unwrap();
    assert!(Command::new("kill")
        .args(["-TERM", &child.0.id().to_string()])
        .status()
        .unwrap()
        .success());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "shutdown must not reload unavailable key"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    runtime.block_on(async {
        let storage = calendar_storage::Storage::open(&db).await.unwrap();
        let state: (bool, Option<i64>) =
            sqlx::query_as("SELECT paused,cutoff FROM recovery_control WHERE id=1")
                .fetch_one(storage.pool())
                .await
                .unwrap();
        assert_eq!(state, (true, Some(42)));
        storage.close().await;
    });
}
