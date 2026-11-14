//! Read-only view of recent management changes (#196).
mod admin_support;
#[path = "../../calendar-adapters/tests/support/admin.rs"]
mod support;
use admin_support::{source, Admin};
use axum::http::StatusCode;
use calendar_application::configuration::*;
use calendar_server::{Config, Server};
use serde_json::Value;
use std::time::Duration;

#[tokio::test]
async fn recent_management_changes_are_listed_newest_first_without_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::initialize(Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        database: dir.path().join("db"),
        max_request_bytes: 65536,
        max_response_bytes: 262144,
        request_timeout: Duration::from_secs(10),
    })
    .await
    .unwrap();
    let mut admin = Admin::login(&server).await;
    let (status, empty) = admin.call("GET", "/api/admin/v1/audit", None, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(empty, serde_json::json!({"items":[],"more":false}));

    // Source create + enable (two config audits), a token issue and a deletion approval.
    source(&server, "push", SourceMode::PushOnly).await;
    let (_, issued) = admin
        .issue(
            "push",
            serde_json::json!({"can_upsert":true,"can_delete":true,"expires_at":null}),
        )
        .await;
    let token = issued["token"]["token_id"].as_str().unwrap().to_owned();
    let secret = issued["secret"].as_str().unwrap().to_owned();
    // Distinct milliseconds keep the expected order independent of tie-breaking.
    tokio::time::sleep(Duration::from_millis(5)).await;
    let approve = "/api/admin/v1/sources/push/deletion-approval";
    assert_eq!(
        admin.call("POST", approve, None, true).await.0,
        StatusCode::CREATED
    );

    let (status, page) = admin.call("GET", "/api/admin/v1/audit", None, false).await;
    assert_eq!(status, StatusCode::OK);
    let items = page["items"].as_array().unwrap();
    let summary: Vec<(String, String, String)> = items
        .iter()
        .map(|i| {
            (
                i["kind"].as_str().unwrap().into(),
                i["subject_id"].as_str().unwrap().into(),
                i["action"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("deletion_approval".into(), "push".into(), "approve".into()),
            ("producer_token".into(), token.clone(), "issue".into()),
            ("source".into(), "push".into(), "update".into()),
            ("source".into(), "push".into(), "create".into()),
        ]
    );
    assert_eq!(items[1]["source_id"], "push");
    assert_eq!(items[2]["revision"], 2);
    let actor = items[0]["actor"].as_str().unwrap();
    assert!(actor.len() == 12 && actor.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(items
        .windows(2)
        .all(|w| w[0]["at"].as_i64() >= w[1]["at"].as_i64()));
    let text = page.to_string();
    assert!(!text.contains(&secret));
    for item in items {
        assert_eq!(item.as_object().unwrap().len(), 7);
    }

    // Only the latest 100 are returned; `more` reports the rest.
    for n in 0..100 {
        let db = &server.services.storage;
        let mut config: SourceConfigurationV1 = serde_json::from_str(include_str!(
            "../../../examples/source-configuration-v1.json"
        ))
        .unwrap();
        config.calendars[0].id = format!("c_bulk{n}");
        db.put_source_configuration(
            &format!("bulk{n}"),
            ConfigurationCondition::Absent,
            config,
            "admin",
            10,
        )
        .await
        .unwrap();
    }
    let (_, page) = admin.call("GET", "/api/admin/v1/audit", None, false).await;
    assert_eq!(page["items"].as_array().unwrap().len(), 100);
    assert_eq!(page["more"], Value::Bool(true));
    server.services.drain().await;
}
