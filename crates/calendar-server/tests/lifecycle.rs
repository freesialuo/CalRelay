use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use calendar_server::{Config, Server};
use std::time::Duration;
use tower::ServiceExt;

fn config(path: &std::path::Path) -> Config {
    Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        database: path.into(),
        max_request_bytes: 64,
        max_response_bytes: 131_072,
        request_timeout: Duration::from_secs(1),
    }
}
async fn get(
    app: axum::Router,
    path: &str,
    method: &str,
    body: &str,
) -> (StatusCode, String, serde_json::Value) {
    let response = app
        .oneshot(
            Request::builder()
                .uri(path)
                .method(method)
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let id = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let bytes = to_bytes(response.into_body(), 131_072).await.unwrap();
    let value = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, id, value)
}

#[tokio::test]
async fn http_contract_limits_ids_errors_and_openapi() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::initialize(config(&dir.path().join("service.db")))
        .await
        .unwrap();
    assert!(server.services.ready());
    let migrations: i64 =
        sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success=1")
            .fetch_one(server.services.storage.pool())
            .await
            .unwrap();
    assert!(migrations >= 5);
    let app = server.router();
    let method_error = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/version")
                .method("POST")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        method_error
            .headers()
            .get("allow")
            .unwrap()
            .to_str()
            .unwrap(),
        "GET,HEAD"
    );
    let (status, first, version) = get(app.clone(), "/api/v1/version", "GET", "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(version["api_version"], "v1");
    assert_eq!(version["version"], env!("CARGO_PKG_VERSION"));
    for (path, method, body, expected, code) in [
        (
            "/api/v1/version",
            "GET",
            "x".repeat(65),
            413,
            "request_too_large",
        ),
        (
            "/api/v1/version",
            "POST",
            String::new(),
            405,
            "method_not_allowed",
        ),
        (
            "/api/admin/v1/events",
            "PUT",
            String::new(),
            404,
            "not_found",
        ),
        ("/api/v2/version", "GET", String::new(), 404, "not_found"),
    ] {
        let (status, id, value) = get(app.clone(), path, method, &body).await;
        assert_eq!(status.as_u16(), expected);
        assert_ne!(id, first);
        assert_eq!(
            value,
            serde_json::json!({"error":{"code":code,"request_id":id}})
        );
    }
    let (status, _, document) = get(app.clone(), "/api/v1/openapi.json", "GET", "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(document["openapi"], "3.1.0");
    assert_eq!(document["paths"].as_object().unwrap().len(), 18);
    for path in [
        "/health/live",
        "/health/ready",
        "/api/v1/version",
        "/api/v1/openapi.json",
    ] {
        assert!(
            document["paths"][path]["get"]["responses"]["413"]["content"]["application/json"]
                ["schema"]["$ref"]
                .as_str()
                .unwrap()
                .ends_with("Error")
        );
        let expected = if path == "/health/ready" {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::OK
        };
        assert_eq!(get(app.clone(), path, "GET", "").await.0, expected);
    }
    server.services.stop();
    let (status, id, value) = get(app.clone(), "/health/ready", "GET", "").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(value["error"]["request_id"], id);
    assert_eq!(get(app, "/health/live", "GET", "").await.0, StatusCode::OK);
    server.services.drain().await;
}

#[tokio::test]
async fn migration_failure_never_constructs_ready_service() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(&dir.path().join("bad.db"));
    let storage = calendar_storage::Storage::open(&cfg.database)
        .await
        .unwrap();
    sqlx::query("UPDATE _sqlx_migrations SET checksum=x'00' WHERE version=1")
        .execute(storage.pool())
        .await
        .unwrap();
    storage.close().await;
    assert!(Server::initialize(cfg).await.is_err());
}

#[tokio::test]
async fn shutdown_blocks_new_claims_and_drains_committing_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(&dir.path().join("drain.db"));
    let server = Server::initialize(cfg.clone()).await.unwrap();
    let services = server.services.clone();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
    let task_services = services.clone();
    let task = tokio::spawn(async move {
        task_services
            .run_task(async |storage: &calendar_storage::Storage| {
                let mut tx = storage.pool().begin().await.unwrap();
                sqlx::query("CREATE TABLE lifecycle_witness(value INTEGER)")
                    .execute(&mut *tx)
                    .await
                    .unwrap();
                entered_tx.send(()).unwrap();
                finish_rx.await.unwrap();
                sqlx::query("INSERT INTO lifecycle_witness VALUES(27)")
                    .execute(&mut *tx)
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
            })
            .await
    });
    entered_rx.await.unwrap();
    let listener = server.bind().await.unwrap();
    let (signal_tx, signal_rx) = tokio::sync::oneshot::channel();
    let (stopped_tx, stopped_rx) = tokio::sync::oneshot::channel();
    let signal_services = services.clone();
    let serving = tokio::spawn(server.serve(listener, async move {
        signal_rx.await.unwrap();
        signal_services.stop();
        stopped_tx.send(()).unwrap();
    }));
    signal_tx.send(()).unwrap();
    stopped_rx.await.unwrap();
    assert!(!services.ready());
    assert_eq!(
        services
            .run_task(async |_| panic!("claim after stop"))
            .await,
        None::<()>
    );
    // The held transaction is the deterministic barrier; serving must wait for it.
    assert!(!serving.is_finished());
    finish_tx.send(()).unwrap();
    assert_eq!(task.await.unwrap(), Some(()));
    serving.await.unwrap().unwrap();
    let reopened = calendar_storage::Storage::open(&cfg.database)
        .await
        .unwrap();
    let value: i64 = sqlx::query_scalar("SELECT value FROM lifecycle_witness")
        .fetch_one(reopened.pool())
        .await
        .unwrap();
    assert_eq!(value, 27);
    reopened.close().await;
}

#[test]
fn invalid_configuration_is_rejected_without_secrets_in_diagnostics() {
    assert!(Config::from_deployment(b"{}", |_| None).is_err());
    for (key, value) in [
        ("CALRELAY_BIND", "secret-invalid"),
        ("CALRELAY_DATABASE", ""),
        ("CALRELAY_MAX_REQUEST_BYTES", "0"),
        ("CALRELAY_MAX_RESPONSE_BYTES", "511"),
        ("CALRELAY_REQUEST_TIMEOUT_MS", "0"),
    ] {
        let result = Config::from_deployment(br#"{"bind":"127.0.0.1:0"}"#, |name| {
            Some(if name == key {
                value.to_owned()
            } else if name == "CALRELAY_DATABASE" {
                "service.db".into()
            } else {
                return None;
            })
        });
        let error = result.unwrap_err().to_string();
        assert!(!error.contains("secret-invalid"));
    }
}

#[test]
fn deployment_file_is_the_only_listener_authority_and_budgets_stay_deployment_owned() {
    let lookup = |key: &str| match key {
        "CALRELAY_DATABASE" => Some("persistent.db".into()),
        "CALRELAY_MAX_REQUEST_BYTES" => Some("4096".into()),
        "CALRELAY_SOURCE_NAME" => Some("must-not-be-read".into()),
        _ => None,
    };
    let parsed = Config::from_deployment(br#"{"bind":"127.0.0.1:0"}"#, lookup).unwrap();
    assert_eq!(parsed.bind, "127.0.0.1:0".parse().unwrap());
    assert_eq!(parsed.max_request_bytes, 4096);
    for body in [
        b"{}".as_slice(),
        br#"{"bind":"invalid-secret"}"#,
        br#"{"bind":"127.0.0.1:0","enabled":true}"#,
        br#"{"bind":"127.0.0.1:0","bind":"127.0.0.1:1"}"#,
    ] {
        let error = Config::from_deployment(body, lookup)
            .unwrap_err()
            .to_string();
        assert!(!error.contains("invalid-secret"));
    }
    let legacy = Config::from_deployment(br#"{"bind":"127.0.0.1:0"}"#, |key| {
        if key == "CALRELAY_BIND" {
            Some(String::new())
        } else {
            lookup(key)
        }
    })
    .unwrap_err()
    .to_string();
    assert!(legacy.contains("--deployment-config"));
}

#[test]
fn management_openapi_embeds_the_shared_configuration_schema_and_discovery_dto() {
    let document = calendar_server::openapi();
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../../schemas/source-configuration-v1.schema.json"
    ))
    .unwrap();
    assert_eq!(
        document["components"]["schemas"]["SourceConfigurationV1"],
        schema
    );
    assert_eq!(
        document["paths"]["/api/admin/v1/sources"]["get"]["responses"]["200"]["content"]
            ["application/json"]["schema"]["$ref"],
        "#/components/schemas/SourceConfigurationPage"
    );
    assert_eq!(
        document["paths"]["/api/admin/v1/sources/{source_id}"]["put"]["requestBody"]["content"]
            ["application/json"]["schema"]["$ref"],
        "#/components/schemas/SourceConfigurationV1"
    );
}
