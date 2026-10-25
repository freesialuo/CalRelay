use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use calendar_server::{Config, Server};
use std::time::Duration;
use tower::ServiceExt;
#[tokio::test]
async fn disabled_bootstrap_is_public_explicit_and_never_cached() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::initialize(Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        database: dir.path().join("db"),
        max_request_bytes: 8192,
        max_response_bytes: 16384,
        request_timeout: Duration::from_secs(10),
    })
    .await
    .unwrap();
    let response = server
        .router()
        .oneshot(
            Request::builder()
                .uri("/api/admin/v1/bootstrap")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
    assert_eq!(body, serde_json::json!({"enabled":false}));
    server.services.drain().await;
}

fn deployment(path: &std::path::Path) -> serde_json::Value {
    serde_json::json!({"bind":"127.0.0.1:0","admin":{"team":"synthetic-team","audience":"admin-aud","origin":"https://admin.example.test","hostname":"admin.example.test","action":"admin-login","site_key":"synthetic-site-key","administrators":{"alice-sub":"alice@example.test"},"session_seconds":120,"turnstile_secret_file":path}})
}
#[test]
fn deployment_is_strict_and_cannot_select_mock_or_remote_transport() {
    use calendar_server::DeploymentConfig;
    let original = deployment(std::path::Path::new("/synthetic/secret"));
    for (field, value) in [
        ("site_key", serde_json::json!("")),
        ("team", serde_json::json!("https://evil.test")),
        ("audience", serde_json::json!("")),
        ("origin", serde_json::json!("http://admin.example.test")),
        ("origin", serde_json::json!("https://evil.test")),
        ("administrators", serde_json::json!({})),
        ("session_seconds", serde_json::json!(901)),
        ("action", serde_json::json!("a/b")),
        ("turnstile_secret_file", serde_json::json!("relative")),
        ("transport", serde_json::json!("mock")),
        ("secret", serde_json::json!("must-not-be-accepted")),
    ] {
        let mut candidate = original.clone();
        candidate["admin"][field] = value;
        assert!(
            DeploymentConfig::from_deployment(&serde_json::to_vec(&candidate).unwrap(), |k| (k
                == "CALRELAY_DATABASE")
                .then(|| "synthetic.db".into()))
            .is_err(),
            "accepted {field}"
        );
    }
}

#[tokio::test]
async fn production_assembly_reads_only_bounded_regular_secret_and_public_metadata_is_minimal() {
    use calendar_server::DeploymentConfig;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("secret");
    let database = dir.path().join("db");
    for secret in [
        Vec::new(),
        vec![b'x'; 4097],
        b"synthetic-secret\n".to_vec(),
        vec![0xff],
        vec![0],
    ] {
        std::fs::write(&file, secret).unwrap();
        let parsed = DeploymentConfig::from_deployment(
            &serde_json::to_vec(&deployment(&file)).unwrap(),
            |k| (k == "CALRELAY_DATABASE").then(|| database.to_str().unwrap().into()),
        )
        .unwrap();
        let error = match parsed.initialize().await {
            Err(e) => e.to_string(),
            Ok(_) => panic!("accepted malformed secret"),
        };
        assert_eq!(
            error,
            "invalid or missing server configuration: admin secret unavailable or invalid"
        );
        assert!(!database.exists());
    }
    std::fs::write(&file, b"synthetic-secret").unwrap();
    let parsed =
        DeploymentConfig::from_deployment(&serde_json::to_vec(&deployment(&file)).unwrap(), |k| {
            (k == "CALRELAY_DATABASE").then(|| database.to_str().unwrap().into())
        })
        .unwrap();
    let server = parsed.initialize().await.unwrap();
    let response = server
        .router()
        .oneshot(
            Request::builder()
                .uri("/api/admin/v1/bootstrap")
                .header("cf-access-authenticated-user-email", "forged@example.test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
    assert_eq!(
        body,
        serde_json::json!({"enabled":true,"turnstile":{"site_key":"synthetic-site-key","hostname":"admin.example.test","action":"admin-login"}})
    );
    let response = server
        .router()
        .oneshot(
            Request::builder()
                .uri("/api/admin/v1/session")
                .header("cf-access-authenticated-user-email", "alice@example.test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    server.services.drain().await;
}
#[cfg(unix)]
#[tokio::test]
async fn symlink_directory_missing_and_fifo_secret_files_are_denied_before_database_open() {
    use calendar_server::DeploymentConfig;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("real-synthetic");
    std::fs::write(&target, b"synthetic").unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let fifo = dir.path().join("fifo");
    assert!(std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    for file in [
        link,
        fifo,
        dir.path().join("missing"),
        dir.path().to_path_buf(),
    ] {
        let database = dir.path().join("never-created");
        let parsed = DeploymentConfig::from_deployment(
            &serde_json::to_vec(&deployment(&file)).unwrap(),
            |k| (k == "CALRELAY_DATABASE").then(|| database.to_str().unwrap().into()),
        )
        .unwrap();
        assert!(parsed.initialize().await.is_err());
        assert!(!database.exists());
    }
}

#[test]
fn duplicate_admin_subjects_and_duplicate_fields_fail_closed() {
    let original = deployment(std::path::Path::new("/synthetic/secret")).to_string();
    for text in [
        original.replace(
            "\"alice-sub\":\"alice@example.test\"",
            "\"alice-sub\":\"alice@example.test\",\"alice-sub\":\"other@example.test\"",
        ),
        original.replace(
            "\"team\":\"synthetic-team\"",
            "\"team\":\"synthetic-team\",\"team\":\"other\"",
        ),
    ] {
        assert_ne!(text, original);
        assert!(
            calendar_server::DeploymentConfig::from_deployment(text.as_bytes(), |k| (k
                == "CALRELAY_DATABASE")
                .then(|| "synthetic.db".into()))
            .is_err()
        );
    }
}

#[test]
fn explicit_null_admin_and_legacy_parser_cannot_silently_drop_configuration() {
    let lookup = |key: &str| (key == "CALRELAY_DATABASE").then(|| "synthetic.db".into());
    assert!(calendar_server::DeploymentConfig::from_deployment(
        br#"{"bind":"127.0.0.1:0","admin":null}"#,
        lookup
    )
    .is_err());
    assert!(Config::from_deployment(
        &serde_json::to_vec(&deployment(std::path::Path::new("/synthetic/secret"))).unwrap(),
        lookup
    )
    .is_err());
}
