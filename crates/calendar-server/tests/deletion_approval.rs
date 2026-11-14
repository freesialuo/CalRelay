//! One-time administrator approval of over-limit snapshot deletions (#186).
mod admin_support;
#[path = "../../calendar-adapters/tests/support/admin.rs"]
mod support;
use admin_support::{source, Admin};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use calendar_application::configuration::*;
use calendar_server::{Config, Server};
use serde_json::{json, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tower::ServiceExt;

async fn server(dir: &tempfile::TempDir) -> Server {
    Server::initialize(Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        database: dir.path().join("db"),
        max_request_bytes: 1_048_576,
        max_response_bytes: 65536,
        request_timeout: Duration::from_secs(10),
    })
    .await
    .unwrap()
}
async fn snapshot(server: &Server, secret: &str, key: &str, count: usize) -> StatusCode {
    let state: Value = {
        let response = server
            .router()
            .oneshot(
                Request::get("/api/ingest/standard-calendar/v1/sources/push/state")
                    .header("authorization", format!("Bearer {secret}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    };
    let operations: Vec<Value> = (0..count)
        .map(|n| {
            json!({"op":"upsert","calendar_id":"c_push","event":{"schema_version":"1.0",
            "external_uid":format!("e{n}"),"title":"x","status":"confirmed",
            "start":{"kind":"utc","value":"2030-01-01T09:00:00Z"}}})
        })
        .collect();
    let body = json!({"protocol_version":"standard-calendar-source/v1","batch_id":key,"mode":"snapshot",
        "base_source_revision":state["source_revision"],"base_config_revision":state["config_revision"],
        "base_checkpoint":null,"proposed_checkpoint":null,"complete":true,"pages":1,"collection_tag":null,
        "operations":operations});
    server
        .router()
        .oneshot(
            Request::post("/api/ingest/standard-calendar/v1/sources/push/batches")
                .header("authorization", format!("Bearer {secret}"))
                .header("idempotency-key", key)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}
async fn live(server: &Server) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM events WHERE source_id='push' AND deleted_at IS NULL")
        .fetch_one(server.services.storage.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn an_approval_unblocks_exactly_the_next_over_limit_push_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let server = server(&dir).await;
    let push = source(&server, "push", SourceMode::PushOnly).await;
    source(&server, "pull", SourceMode::Pull).await;
    let db = &server.services.storage;
    let mut manual: SourceConfigurationV1 = serde_json::from_str(include_str!(
        "../../../examples/source-configuration-v1.json"
    ))
    .unwrap();
    manual.calendars[0].id = "c_static".into();
    db.put_source_configuration("static", ConfigurationCondition::Absent, manual, "test", 1)
        .await
        .unwrap();
    let secret = db
        .issue_api_token("t", "push", true, true, None)
        .await
        .unwrap();
    let secret = String::from_utf8(secret.expose().to_vec()).unwrap();
    let mut admin = Admin::login(&server).await;
    let approve = |id: &str| format!("/api/admin/v1/sources/{id}/deletion-approval");

    assert_eq!(
        snapshot(&server, &secret, "seed", 102).await,
        StatusCode::OK
    );
    // Keeping one event would delete 101: refused until approved.
    assert_eq!(
        snapshot(&server, &secret, "drop", 1).await,
        StatusCode::CONFLICT
    );
    assert_eq!(live(&server).await, 102);

    // The write guard applies; only enabled pull or push-only managed sources can be approved.
    assert_eq!(
        admin.call("POST", &approve("push"), None, false).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        admin.call("POST", &approve("missing"), None, true).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        admin.call("POST", &approve("static"), None, true).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        admin
            .call("POST", &approve("push"), Some(json!({"max":1})), true)
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        admin.call("POST", &approve("pull"), None, true).await.0,
        StatusCode::CREATED
    );
    let before = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let (status, approval) = admin.call("POST", &approve("push"), None, true).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(approval["source_id"], "push");
    assert_eq!(approval["config_revision"], json!(push.config_revision));
    assert_eq!(approval["source_revision"], json!(push.source_revision + 1));
    let expires = approval["expires_at"].as_i64().unwrap();
    assert!(expires >= before + 86_400_000 && expires < before + 86_400_000 + 60_000);

    assert_eq!(
        snapshot(&server, &secret, "drop-approved", 1).await,
        StatusCode::OK
    );
    assert_eq!(live(&server).await, 1);
    // The commit advanced the source revision, so the approval is spent.
    assert_eq!(
        snapshot(&server, &secret, "reseed", 102).await,
        StatusCode::OK
    );
    assert_eq!(
        snapshot(&server, &secret, "drop-again", 1).await,
        StatusCode::CONFLICT
    );
    // Emptying a non-empty source also needs, and accepts, an approval.
    assert_eq!(
        snapshot(&server, &secret, "empty", 0).await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        admin.call("POST", &approve("push"), None, true).await.0,
        StatusCode::CREATED
    );
    assert_eq!(
        snapshot(&server, &secret, "empty-approved", 0).await,
        StatusCode::OK
    );
    assert_eq!(live(&server).await, 0);

    let audited: i64 = sqlx::query_scalar("SELECT count(*) FROM deletion_approval_audit")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(audited, 3);
    server.services.drain().await;
}

#[tokio::test]
async fn an_approval_expires_after_a_day_and_only_matches_its_revisions() {
    let dir = tempfile::tempdir().unwrap();
    let server = server(&dir).await;
    let push = source(&server, "push", SourceMode::PushOnly).await;
    let db = &server.services.storage;
    let approval = db
        .approve_source_deletions("push", "admin", 1_000)
        .await
        .unwrap();
    assert_eq!(approval.expires_at, 1_000 + 86_400_000);
    let (config, revision) = (push.config_revision, push.source_revision);
    assert!(db
        .source_deletions_approved("push", config, revision, 86_400_999)
        .await
        .unwrap());
    assert!(!db
        .source_deletions_approved("push", config, revision, 86_401_000)
        .await
        .unwrap());
    assert!(!db
        .source_deletions_approved("push", config, revision + 1, 2_000)
        .await
        .unwrap());
    assert!(!db
        .source_deletions_approved("push", config + 1, revision, 2_000)
        .await
        .unwrap());
    server.services.drain().await;
}
