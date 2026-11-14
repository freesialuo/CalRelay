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
    assert_eq!(
        empty,
        serde_json::json!({"items":[],"more":false,"next_before":null})
    );

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

    // #218: the cursor continues across the tie of 100 entries at the same millisecond.
    let first = collect(&mut admin, "/api/admin/v1/audit").await;
    assert_eq!(first.len(), 104);
    let keys: std::collections::HashSet<_> = first
        .iter()
        .map(|i| {
            (
                i["kind"].clone(),
                i["subject_id"].clone(),
                i["action"].clone(),
            )
        })
        .collect();
    assert_eq!(keys.len(), 104, "no entry repeats across pages");
    // The helper's source writes predate the bulk entries, so the 100-entry tie at
    // millisecond 10 straddles the page boundary; later writes come first within it.
    let order: Vec<_> = first
        .iter()
        .map(|i| i["subject_id"].as_str().unwrap())
        .collect();
    assert_eq!(order[..2], ["push", token.as_str()]);
    assert!(first[2..102].iter().all(|i| i["at"] == 10));
    assert_eq!((order[2], order[101]), ("bulk99", "bulk0"));
    assert_eq!(
        first[102..]
            .iter()
            .map(|i| &i["action"])
            .collect::<Vec<_>>(),
        ["update", "create"]
    );

    // Filtering by kind composes with the cursor.
    let sources = collect(&mut admin, "/api/admin/v1/audit?kind=source").await;
    assert_eq!(sources.len(), 102);
    assert!(sources.iter().all(|i| i["kind"] == "source"));
    let (_, tokens) = admin
        .call(
            "GET",
            "/api/admin/v1/audit?kind=producer_token",
            None,
            false,
        )
        .await;
    assert_eq!(tokens["items"].as_array().unwrap().len(), 1);
    assert_eq!(tokens["next_before"], Value::Null);
    assert_eq!(tokens["more"], Value::Bool(false));

    // Entries written after the first page was read do not shift the next page.
    let (_, head) = admin.call("GET", "/api/admin/v1/audit", None, false).await;
    let cursor = head["next_before"].as_str().unwrap().to_owned();
    let revoke = format!("/api/admin/v1/sources/push/tokens/{token}/revoke");
    assert_eq!(
        admin.call("POST", &revoke, None, true).await.0,
        StatusCode::NO_CONTENT
    );
    let (_, rest) = admin
        .call(
            "GET",
            &format!("/api/admin/v1/audit?before={cursor}"),
            None,
            false,
        )
        .await;
    assert_eq!(rest["items"].as_array().unwrap()[..], first[100..]);

    for bad in [
        "before=x",
        "before=10.source",
        "before=10.nope.1",
        "before=%2B10.source.1",
        "before=010.source.1",
        "before=10.source.0",
        "before=10.source.1.2",
        "kind=nope",
        "kind=",
    ] {
        let (status, _) = admin
            .call("GET", &format!("/api/admin/v1/audit?{bad}"), None, false)
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    }
    let (status, _) = admin
        .call("GET", "/api/admin/v1/audit?after=x", None, false)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    server.services.drain().await;
}

/// Follows `next_before` until the last page; every page is bounded and consistent with `more`.
async fn collect(admin: &mut Admin, path: &str) -> Vec<Value> {
    let mut items = Vec::new();
    let mut next = path.to_owned();
    loop {
        let (status, page) = admin.call("GET", &next, None, false).await;
        assert_eq!(status, StatusCode::OK);
        let batch = page["items"].as_array().unwrap();
        assert!(batch.len() <= 100);
        items.extend(batch.iter().cloned());
        assert_eq!(page["more"], Value::Bool(!page["next_before"].is_null()));
        match page["next_before"].as_str() {
            Some(cursor) => {
                let sep = if path.contains('?') { '&' } else { '?' };
                next = format!("{path}{sep}before={cursor}");
            }
            None => return items,
        }
    }
}
