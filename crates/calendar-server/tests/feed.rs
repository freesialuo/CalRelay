//! #32: synthetic storage and in-process HTTP only; never call external services.
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use calendar_server::{Config, Server};
use std::time::Duration;
use tower::ServiceExt;

async fn fixture() -> (tempfile::TempDir, Server, String) {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::initialize(Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        database: dir.path().join("db"),
        max_request_bytes: 1024,
        max_response_bytes: 16384,
        request_timeout: Duration::from_secs(3),
    })
    .await
    .unwrap();
    let db = &server.services.storage;
    sqlx::query("INSERT INTO sources(id,kind,revision) VALUES ('s','test',1),('other','test',0)")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO calendars(id,source_id,external_id) VALUES ('c','s','c')")
        .execute(db.pool())
        .await
        .unwrap();
    let doc = serde_json::json!({"schema_version":"1.0","external_uid":"private-source-id","title":"visible title","description":"private description","status":"confirmed","classification":"private","transparency":"opaque","start":{"kind":"utc","value":"2026-10-25T10:00:00Z"},"duration":"PT1H"}).to_string();
    sqlx::query("INSERT INTO events(id,source_id,calendar_id,external_uid,revision,document_json,content_hash) VALUES ('event','s','c','private-source-id',1,?,'hash')").bind(&doc).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO event_revisions(event_id,revision,document_json,change_kind,changed_at) VALUES ('event',1,?,'upsert',1792922400000)").bind(&doc).execute(db.pool()).await.unwrap();
    let token = db
        .issue_feed_token("sub", "public", "s", None)
        .await
        .unwrap();
    let url = format!(
        "/feeds/public/{}.ics",
        std::str::from_utf8(token.expose()).unwrap()
    );
    (dir, server, url)
}
async fn request(app: axum::Router, url: &str, tag: Option<&str>) -> axum::response::Response {
    let mut r = Request::builder().uri(url);
    if let Some(tag) = tag {
        r = r.header("if-none-match", tag);
    }
    app.oneshot(r.body(Body::empty()).unwrap()).await.unwrap()
}
#[tokio::test]
async fn feed_requires_no_browser_auth_and_rechecks_revocation_before_304() {
    let (_dir, server, url) = fixture().await;
    let app = server.router();
    let response = request(app.clone(), &url, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "text/calendar; charset=utf-8"
    );
    assert_eq!(response.headers()["cache-control"], "private, no-cache");
    let etag = response.headers()["etag"].to_str().unwrap().to_owned();
    let body = to_bytes(response.into_body(), 16384).await.unwrap();
    let body = std::str::from_utf8(&body).unwrap();
    assert!(body.contains("SUMMARY:visible title"));
    assert!(!body.contains("private-source-id"));
    let response = request(app.clone(), &url, Some(&etag)).await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(response.headers()["cache-control"], "private, no-cache");
    assert!(to_bytes(response.into_body(), 16384)
        .await
        .unwrap()
        .is_empty());
    for candidate in [
        format!("W/{etag}"),
        format!("\"different\", W/{etag}"),
        "*".into(),
    ] {
        assert_eq!(
            request(app.clone(), &url, Some(&candidate)).await.status(),
            StatusCode::NOT_MODIFIED
        );
    }
    assert_eq!(
        request(app.clone(), &url, Some("\"different\""))
            .await
            .status(),
        StatusCode::OK
    );
    server
        .services
        .storage
        .revoke_feed_token("sub", 1)
        .await
        .unwrap();
    let response = request(app, &url, Some(&etag)).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(!response.headers().contains_key("etag"));
    assert!(
        !String::from_utf8_lossy(&to_bytes(response.into_body(), 16384).await.unwrap())
            .contains(&url)
    );
}
#[tokio::test]
async fn feed_config_data_and_token_changes_invalidate_cache() {
    let (_dir, server, url) = fixture().await;
    let app = server.router();
    let first = request(app.clone(), &url, None).await;
    assert_eq!(first.status(), StatusCode::OK);
    let etag = first.headers()["etag"].to_str().unwrap().to_owned();
    sqlx::query("UPDATE sources SET revision=revision+1 WHERE id='s'")
        .execute(server.services.storage.pool())
        .await
        .unwrap();
    let next = request(app.clone(), &url, Some(&etag)).await;
    assert_eq!(next.status(), StatusCode::OK);
    assert_ne!(next.headers()["etag"], etag);
    let token = server
        .services
        .storage
        .rotate_feed_token("sub", 0)
        .await
        .unwrap();
    assert_eq!(
        request(app.clone(), &url, Some(&etag)).await.status(),
        StatusCode::NOT_FOUND
    );
    let new_url = format!(
        "/feeds/public/{}.ics",
        std::str::from_utf8(token.expose()).unwrap()
    );
    let response = request(app.clone(), &new_url, Some(&etag)).await;
    assert_eq!(response.status(), StatusCode::OK);
    sqlx::query("UPDATE sources SET enabled=0 WHERE id='s'")
        .execute(server.services.storage.pool())
        .await
        .unwrap();
    assert_eq!(
        request(app, &new_url, None).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn scope_shrink_invalidates_old_etag_and_redacts_exception_content() {
    let (_dir, server, url) = fixture().await;
    let db = &server.services.storage;
    let raw: String = sqlx::query_scalar("SELECT document_json FROM events WHERE id='event'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let mut doc: serde_json::Value = serde_json::from_str(&raw).unwrap();
    doc["location"] = "secret location".into();
    doc["url"] = "https://private.invalid/base".into();
    doc["categories"] = serde_json::json!(["secret category"]);
    doc["recurrence"] = serde_json::json!({"rrule":"FREQ=DAILY;COUNT=2","exceptions":[{"recurrence_id":{"kind":"utc","value":"2026-10-26T10:00:00Z"},"range":"single","change":{"kind":"cancelled"}}]});
    for sql in [
        "UPDATE events SET document_json=? WHERE id='event'",
        "UPDATE event_revisions SET document_json=? WHERE event_id='event'",
    ] {
        sqlx::query(sql)
            .bind(doc.to_string())
            .execute(db.pool())
            .await
            .unwrap();
    }
    let app = server.router();
    let first = request(app.clone(), &url, None).await;
    assert_eq!(first.status(), StatusCode::OK);
    let old_etag = first.headers()["etag"].to_str().unwrap().to_owned();
    sqlx::query("UPDATE feed_subscriptions SET scope_json=? WHERE id='sub'")
        .bind(r#"{"version":1,"calendar_ids":["c"],"visible_fields":[]}"#)
        .execute(db.pool())
        .await
        .unwrap();
    let next = request(app.clone(), &url, Some(&old_etag)).await;
    assert_eq!(next.status(), StatusCode::OK);
    assert_ne!(next.headers()["etag"], old_etag);
    let body =
        String::from_utf8(to_bytes(next.into_body(), 16384).await.unwrap().to_vec()).unwrap();
    assert_eq!(body.matches("SUMMARY:Busy").count(), 2);
    for hidden in [
        "visible title",
        "private description",
        "secret",
        "private.invalid",
        "DESCRIPTION:",
        "LOCATION:",
        "URL:",
        "CATEGORIES:",
    ] {
        assert!(!body.contains(hidden), "leaked {hidden}");
    }
    for preserved in [
        "UID:",
        "DTSTART:",
        "STATUS:CANCELLED",
        "RRULE:FREQ=DAILY;COUNT=2",
        "RECURRENCE-ID:",
    ] {
        assert!(body.contains(preserved), "missing {preserved}");
    }
    sqlx::query("UPDATE feed_subscriptions SET scope_json=? WHERE id='sub'")
        .bind(r#"{"version":1,"calendar_ids":[],"visible_fields":["summary"]}"#)
        .execute(db.pool())
        .await
        .unwrap();
    let next = request(app, &url, Some(&old_etag)).await;
    assert_eq!(next.status(), StatusCode::OK);
    assert!(
        !String::from_utf8_lossy(&to_bytes(next.into_body(), 16384).await.unwrap())
            .contains("BEGIN:VEVENT")
    );
}

#[tokio::test]
async fn invalid_tokens_scopes_and_unsupported_documents_never_return_cached_data() {
    let (_dir, server, url) = fixture().await;
    let app = server.router();
    let db = &server.services.storage;
    let first = request(app.clone(), &url, None).await;
    let tag = first.headers()["etag"].to_str().unwrap().to_owned();
    for bad in [
        r#"{"version":2,"calendar_ids":["c"],"visible_fields":[]}"#,
        r#"{"version":1,"calendar_ids":["missing"],"visible_fields":[]}"#,
        r#"{"version":1,"calendar_ids":["c"],"visible_fields":["password"]}"#,
        r#"{"version":1,"calendar_ids":["c"],"visible_fields":[],"filter":true}"#,
    ] {
        sqlx::query("UPDATE feed_subscriptions SET scope_json=?")
            .bind(bad)
            .execute(db.pool())
            .await
            .unwrap();
        let response = request(app.clone(), &url, Some(&tag)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(!response.headers().contains_key("etag"));
    }
    sqlx::query("UPDATE feed_subscriptions SET scope_json='{}',expires_at=0")
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        request(app.clone(), &url, Some(&tag)).await.status(),
        StatusCode::NOT_FOUND
    );
    sqlx::query("UPDATE feed_subscriptions SET expires_at=NULL")
        .execute(db.pool())
        .await
        .unwrap();
    let api = db
        .issue_api_token("a", "s", true, true, None)
        .await
        .unwrap();
    let api_url = format!(
        "/feeds/public/{}.ics",
        std::str::from_utf8(api.expose()).unwrap()
    );
    for bad in [
        api_url,
        "/feeds/foreign/not-a-token.ics".into(),
        url.replace(".ics", ".json"),
        url.replace("public", "foreign"),
    ] {
        assert_eq!(
            request(app.clone(), &bad, Some(&tag)).await.status(),
            StatusCode::NOT_FOUND
        );
    }
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&url)
                .method("POST")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    // Redaction must not widen the existing renderer's rejection profile.
    sqlx::query("UPDATE feed_subscriptions SET scope_json=?")
        .bind(r#"{"version":1,"calendar_ids":["c"],"visible_fields":[]}"#)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE events SET document_json=json_set(document_json,'$.organizer',json('{\"address\":\"mailto:private@example.invalid\"}'))").execute(db.pool()).await.unwrap();
    sqlx::query("UPDATE sources SET revision=revision+1 WHERE id='s'")
        .execute(db.pool())
        .await
        .unwrap();
    let response = request(app, &url, Some(&tag)).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!response.headers().contains_key("etag"));
    let body =
        String::from_utf8_lossy(&to_bytes(response.into_body(), 16384).await.unwrap()).into_owned();
    for secret in [&url, "private@example.invalid", "organizer"] {
        assert!(!body.contains(secret));
    }
}

#[tokio::test]
async fn deleted_event_retains_uid_document_and_latest_revision_timestamp() {
    let (_dir, server, url) = fixture().await;
    let app = server.router();
    let db = &server.services.storage;
    let first = request(app.clone(), &url, None).await;
    let etag = first.headers()["etag"].to_str().unwrap().to_owned();
    let body =
        String::from_utf8(to_bytes(first.into_body(), 16384).await.unwrap().to_vec()).unwrap();
    let uid = body.lines().find(|line| line.starts_with("UID:")).unwrap();
    let mut tx = db.pool().begin().await.unwrap();
    sqlx::query("UPDATE events SET revision=2,deleted_at=1793008800000 WHERE id='event'")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO event_revisions(event_id,revision,change_kind,changed_at) VALUES ('event',2,'delete',1793008800000)").execute(&mut *tx).await.unwrap();
    sqlx::query("UPDATE sources SET revision=2 WHERE id='s'")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let response = request(app, &url, Some(&etag)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(
        to_bytes(response.into_body(), 16384)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    for retained in [
        uid,
        "STATUS:CANCELLED",
        "SEQUENCE:1",
        "SUMMARY:visible title",
        "DTSTART:20261025T100000Z",
    ] {
        assert!(body.contains(retained), "missing {retained}");
    }
}

#[tokio::test]
async fn integrated_ingest_keeps_feed_read_only_and_invalidates_its_representation() {
    let (_dir, server, url) = fixture().await;
    let app = server.router();
    let db = &server.services.storage;
    let document = calendar_server::openapi();
    assert!(
        document["paths"]["/api/ingest/v1/sources/{source_id}/events/{uid}"]["delete"].is_object()
    );
    let first = request(app.clone(), &url, None).await;
    assert_eq!(first.status(), StatusCode::OK);
    let old_tag = first.headers()["etag"].to_str().unwrap().to_owned();
    let feed_token = url
        .rsplit('/')
        .next()
        .unwrap()
        .strip_suffix(".ics")
        .unwrap();
    let write = |token: &str| {
        Request::builder()
            .uri("/api/ingest/v1/sources/s/events/private-source-id")
            .method("DELETE")
            .header("authorization", format!("Bearer {token}"))
            .header("idempotency-key", "feed-integration-delete")
            .header("if-match", "\"1\"")
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        app.clone()
            .oneshot(write(feed_token))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(app.clone(), &url, Some(&old_tag)).await.status(),
        StatusCode::NOT_MODIFIED
    );
    let api = db
        .issue_api_token("writer", "s", false, true, None)
        .await
        .unwrap();
    assert_eq!(
        app.clone()
            .oneshot(write(std::str::from_utf8(api.expose()).unwrap()))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let response = request(app, &url, Some(&old_tag)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_ne!(response.headers()["etag"], old_tag);
    let body = String::from_utf8(
        to_bytes(response.into_body(), 16384)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("STATUS:CANCELLED"));
    assert!(body.contains("SEQUENCE:1"));
}
