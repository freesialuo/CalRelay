//! Producer token management for push-only sources over the real admin guard and SQLite (#178).
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
use std::time::Duration;
use tower::ServiceExt;

async fn batch(
    server: &Server,
    record: &SourceConfigurationRecord,
    secret: &str,
    key: &str,
) -> StatusCode {
    let body = json!({"protocol_version":"standard-calendar-source/v1","batch_id":"b","mode":"snapshot",
        "base_source_revision":record.source_revision,"base_config_revision":record.config_revision,
        "base_checkpoint":null,"proposed_checkpoint":null,"complete":true,"pages":1,"collection_tag":null,
        "operations":[]});
    let path = format!(
        "/api/ingest/standard-calendar/v1/sources/{}/batches",
        record.source_id
    );
    server
        .router()
        .oneshot(
            Request::post(path)
                .header("authorization", format!("Bearer {secret}"))
                .header("idempotency-key", key)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn producer_tokens_are_issued_once_listed_without_secrets_and_revoked_with_audit() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::initialize(Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        database: dir.path().join("db"),
        max_request_bytes: 65536,
        max_response_bytes: 65536,
        request_timeout: Duration::from_secs(10),
    })
    .await
    .unwrap();
    let push = source(&server, "push", SourceMode::PushOnly).await;
    source(&server, "pull", SourceMode::Pull).await;
    let mut admin = Admin::login(&server).await;
    let list = "/api/admin/v1/sources/push/tokens";
    assert_eq!(
        admin.call("GET", list, None, false).await,
        (StatusCode::OK, json!({"items":[],"more":false}))
    );

    // The write guard applies: no CSRF/nonce, no token.
    let both = json!({"can_upsert":true,"can_delete":true,"expires_at":null});
    assert_eq!(
        admin.call("POST", list, Some(both.clone()), false).await.0,
        StatusCode::FORBIDDEN
    );
    let (status, issued) = admin.issue("push", both.clone()).await;
    assert_eq!(status, StatusCode::CREATED);
    let token = issued["token"].clone();
    let id = token["token_id"].as_str().unwrap().to_owned();
    assert!(valid_id(&id));
    assert_eq!(
        (
            &token["can_upsert"],
            &token["can_delete"],
            &token["expires_at"],
            &token["revoked_at"]
        ),
        (&json!(true), &json!(true), &Value::Null, &Value::Null)
    );
    assert!(token["issued_at"].as_i64().unwrap() > 0);
    let secret = issued["secret"].as_str().unwrap().to_owned();
    assert_eq!(secret.len(), 64);
    // The secret is usable at once by the machine entry points.
    assert_eq!(batch(&server, &push, &secret, "k1").await, StatusCode::OK);

    // Listing never returns the secret or its digest.
    let (status, page) = admin.call("GET", list, None, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["items"], json!([token]));
    assert!(!page.to_string().contains(&secret));

    // Invalid requests and sources that cannot take producer tokens.
    for body in [
        json!({"can_upsert":false,"can_delete":false,"expires_at":null}),
        json!({"can_upsert":true,"can_delete":false,"expires_at":1}),
        json!({"can_upsert":true,"can_delete":false,"expires_at":null,"secret":"chosen"}),
        json!({"can_upsert":true}),
    ] {
        assert_eq!(
            admin.issue("push", body).await.0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(
        admin.issue("pull", both.clone()).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        admin.issue("missing", both.clone()).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        admin
            .call("GET", "/api/admin/v1/sources/missing/tokens", None, false)
            .await
            .0,
        StatusCode::NOT_FOUND
    );

    // At most 16 live tokens per source; revoked ones do not count.
    for _ in 1..16 {
        assert_eq!(
            admin
                .issue(
                    "push",
                    json!({"can_upsert":true,"can_delete":false,"expires_at":null})
                )
                .await
                .0,
            StatusCode::CREATED
        );
    }
    assert_eq!(
        admin.issue("push", both.clone()).await.0,
        StatusCode::CONFLICT
    );

    let revoke = format!("/api/admin/v1/sources/push/tokens/{id}/revoke");
    assert_eq!(
        admin.call("POST", &revoke, None, true).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        batch(&server, &push, &secret, "k2").await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        admin.call("POST", &revoke, None, true).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        admin.issue("push", both.clone()).await.0,
        StatusCode::CREATED
    );
    for path in [
        "/api/admin/v1/sources/push/tokens/unknown/revoke".to_owned(),
        format!("/api/admin/v1/sources/pull/tokens/{id}/revoke"),
    ] {
        assert_eq!(
            admin.call("POST", &path, None, true).await.0,
            StatusCode::NOT_FOUND
        );
    }
    let (_, page) = admin.call("GET", list, None, false).await;
    assert_eq!(page["items"].as_array().unwrap().len(), 17);
    assert!(page["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["token_id"] == json!(id) && t["revoked_at"].is_i64()));

    // Issue and revoke are audited once each, without secrets.
    let audit: Vec<(String, String)> =
        sqlx::query_as("SELECT token_id,action FROM producer_token_audit ORDER BY rowid")
            .fetch_all(server.services.storage.pool())
            .await
            .unwrap();
    assert_eq!(audit.len(), 18);
    assert_eq!(
        audit
            .iter()
            .filter(|(t, a)| *t == id && a == "revoke")
            .count(),
        1
    );

    // Live tokens are always listed first, however many newer revoked tokens exist: keep one
    // long-lived token and issue and revoke 100 replacements.
    let mut live: Vec<String> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["revoked_at"].is_null())
        .map(|t| t["token_id"].as_str().unwrap().to_owned())
        .collect();
    let kept = live.pop().unwrap();
    for other in live {
        let path = format!("/api/admin/v1/sources/push/tokens/{other}/revoke");
        assert_eq!(
            admin.call("POST", &path, None, true).await.0,
            StatusCode::NO_CONTENT
        );
    }
    for _ in 0..100 {
        let (_, issued) = admin
            .issue(
                "push",
                json!({"can_upsert":true,"can_delete":false,"expires_at":null}),
            )
            .await;
        let fresh = issued["token"]["token_id"].as_str().unwrap().to_owned();
        let path = format!("/api/admin/v1/sources/push/tokens/{fresh}/revoke");
        assert_eq!(
            admin.call("POST", &path, None, true).await.0,
            StatusCode::NO_CONTENT
        );
    }
    let (_, page) = admin.call("GET", list, None, false).await;
    assert_eq!(page["more"], json!(true));
    assert_eq!(page["items"][0]["token_id"], json!(kept));
    assert!(page["items"][0]["revoked_at"].is_null());

    // A retired source cannot take new tokens.
    let db = &server.services.storage;
    let current = db.source_configuration("push").await.unwrap();
    db.retire_source_configuration("push", current.config_revision, "test", 2)
        .await
        .unwrap();
    assert_eq!(admin.issue("push", both).await.0, StatusCode::CONFLICT);
    server.services.drain().await;
}
