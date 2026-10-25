use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use calendar_server::{Config, Server};
use serde_json::{json, Value};
use std::time::Duration;
use tempfile::tempdir;
use tower::ServiceExt;
fn body(uid: &str, title: &str) -> String {
    json!({"calendar_id":"c","event":{"schema_version":"1.0","external_uid":uid,"title":title,"status":"confirmed","start":{"kind":"utc","value":"2026-11-02T17:00:00Z"}}}).to_string()
}
async fn setup() -> (tempfile::TempDir, Server, String) {
    let dir = tempdir().unwrap();
    let server = Server::initialize(Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        database: dir.path().join("db"),
        max_request_bytes: 1048576,
        max_response_bytes: 1048576,
        request_timeout: Duration::from_secs(30),
    })
    .await
    .unwrap();
    sqlx::raw_sql("INSERT INTO sources(id,kind) VALUES('s','api'),('t','api'); INSERT INTO calendars(id,source_id,external_id) VALUES('c','s','main'),('d','t','main');").execute(server.services.storage.pool()).await.unwrap();
    let token = server
        .services
        .storage
        .issue_api_token("token", "s", true, true, None)
        .await
        .unwrap();
    let token = String::from_utf8(token.expose().to_vec()).unwrap();
    (dir, server, token)
}
fn request(
    token: &str,
    method: &str,
    source: &str,
    uid: &str,
    key: &str,
    condition: (&str, &str),
    body: String,
) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(format!("/api/ingest/v1/sources/{source}/events/{uid}"))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .header("idempotency-key", key)
        .header(condition.0, condition.1)
        .body(Body::from(body))
        .unwrap()
}
async fn send(server: &Server, request: Request<Body>, status: StatusCode) -> Value {
    let response = server.router().oneshot(request).await.unwrap();
    assert_eq!(response.status(), status);
    let etag = response.headers().get("etag").cloned();
    if status.is_success() {
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    let id = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let result: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1048576).await.unwrap()).unwrap();
    if status.is_success() {
        assert_eq!(
            etag.unwrap().to_str().unwrap(),
            format!("\"{}\"", result["event_revision"])
        );
    }
    if status.is_client_error() || status.is_server_error() {
        assert_eq!(result["error"]["request_id"], id);
    }
    result
}
#[tokio::test]
async fn conditional_retry_delete_and_tombstone_contract() {
    let (_dir, s, t) = setup().await;
    let first = send(
        &s,
        request(
            &t,
            "PUT",
            "s",
            "a",
            "create",
            ("if-none-match", "*"),
            body("a", "A"),
        ),
        StatusCode::CREATED,
    )
    .await;
    let replay = send(
        &s,
        request(
            &t,
            "PUT",
            "s",
            "a",
            "create",
            ("if-none-match", "*"),
            body("a", "A"),
        ),
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(first, replay);
    send(
        &s,
        request(
            &t,
            "PUT",
            "s",
            "a",
            "create",
            ("if-none-match", "*"),
            body("a", "B"),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    send(
        &s,
        request(
            &t,
            "PUT",
            "s",
            "a",
            "stale",
            ("if-match", "\"2\""),
            body("a", "B"),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    let updated = send(
        &s,
        request(
            &t,
            "PUT",
            "s",
            "a",
            "update",
            ("if-match", "\"1\""),
            body("a", "B"),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(updated["event_revision"], 2);
    let deleted = send(
        &s,
        request(
            &t,
            "DELETE",
            "s",
            "a",
            "delete",
            ("if-match", "\"2\""),
            String::new(),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(deleted["event_revision"], 3);
    assert_eq!(deleted["deleted"], true);
    send(
        &s,
        request(
            &t,
            "PUT",
            "s",
            "a",
            "resurrect",
            ("if-none-match", "*"),
            body("a", "A"),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    let live = send(
        &s,
        request(
            &t,
            "PUT",
            "s",
            "a",
            "resurrect",
            ("if-match", "\"3\""),
            body("a", "A"),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(live["event_revision"], 4);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM source_changes")
            .fetch_one(s.services.storage.pool())
            .await
            .unwrap(),
        4
    );
}
#[tokio::test]
async fn authorization_validation_and_revocation_before_replay() {
    let (_dir, s, t) = setup().await;
    for (source, uid, b, status) in [
        ("t", "a", body("a", "A"), StatusCode::UNAUTHORIZED),
        ("s", "b", body("a", "A"), StatusCode::UNPROCESSABLE_ENTITY),
        ("s", "a", "{}".into(), StatusCode::UNPROCESSABLE_ENTITY),
    ] {
        send(
            &s,
            request(&t, "PUT", source, uid, "key", ("if-none-match", "*"), b),
            status,
        )
        .await;
    }
    let mut wrong = serde_json::from_str::<Value>(&body("a", "A")).unwrap();
    wrong["calendar_id"] = json!("d");
    send(
        &s,
        request(
            &t,
            "PUT",
            "s",
            "a",
            "key",
            ("if-none-match", "*"),
            wrong.to_string(),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    send(
        &s,
        request(
            &t,
            "PUT",
            "s",
            "a",
            "key",
            ("if-none-match", "*"),
            body("a", "A"),
        ),
        StatusCode::CREATED,
    )
    .await;
    sqlx::query("UPDATE api_tokens SET revoked_at=1 WHERE id='token'")
        .execute(s.services.storage.pool())
        .await
        .unwrap();
    send(
        &s,
        request(
            &t,
            "PUT",
            "s",
            "a",
            "key",
            ("if-none-match", "*"),
            body("a", "A"),
        ),
        StatusCode::UNAUTHORIZED,
    )
    .await;
}
#[tokio::test]
async fn concurrent_cas_has_one_winner_and_same_key_replays() {
    let (_dir, s, t) = setup().await;
    let (a, b) = tokio::join!(
        s.router().oneshot(request(
            &t,
            "PUT",
            "s",
            "a",
            "same",
            ("if-none-match", "*"),
            body("a", "A")
        )),
        s.router().oneshot(request(
            &t,
            "PUT",
            "s",
            "a",
            "same",
            ("if-none-match", "*"),
            body("a", "A")
        ))
    );
    assert_eq!(a.unwrap().status(), 201);
    assert_eq!(b.unwrap().status(), 201);
    let (a, b) = tokio::join!(
        s.router().oneshot(request(
            &t,
            "PUT",
            "s",
            "a",
            "one",
            ("if-match", "\"1\""),
            body("a", "B")
        )),
        s.router().oneshot(request(
            &t,
            "PUT",
            "s",
            "a",
            "two",
            ("if-match", "\"1\""),
            body("a", "C")
        ))
    );
    let mut statuses = [a.unwrap().status().as_u16(), b.unwrap().status().as_u16()];
    statuses.sort();
    assert_eq!(statuses, [200, 409]);
}

#[tokio::test]
async fn expired_disabled_action_and_malformed_conditions_fail_closed() {
    let (_dir, s, t) = setup().await;
    for sql in [
        "UPDATE api_tokens SET expires_at=0",
        "UPDATE api_tokens SET expires_at=NULL,can_upsert=0",
        "UPDATE api_tokens SET can_upsert=1; UPDATE sources SET enabled=0 WHERE id='s'",
    ] {
        sqlx::raw_sql(sql)
            .execute(s.services.storage.pool())
            .await
            .unwrap();
        send(
            &s,
            request(
                &t,
                "PUT",
                "s",
                "a",
                "key",
                ("if-none-match", "*"),
                body("a", "A"),
            ),
            StatusCode::UNAUTHORIZED,
        )
        .await;
    }
    sqlx::raw_sql("UPDATE sources SET enabled=1; UPDATE api_tokens SET can_delete=0;")
        .execute(s.services.storage.pool())
        .await
        .unwrap();
    send(
        &s,
        request(
            &t,
            "DELETE",
            "s",
            "a",
            "key",
            ("if-match", "\"1\""),
            String::new(),
        ),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    for condition in [
        ("if-match", "*"),
        ("if-match", "W/\"1\""),
        ("if-match", "\"1\", \"2\""),
        ("if-match", "\"0\""),
        ("if-none-match", "\"1\""),
    ] {
        send(
            &s,
            request(&t, "PUT", "s", "a", "key", condition, body("a", "A")),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    }
    let mut duplicate = request(
        &t,
        "PUT",
        "s",
        "a",
        "key",
        ("if-none-match", "*"),
        body("a", "A"),
    );
    duplicate
        .headers_mut()
        .append("if-none-match", "*".parse().unwrap());
    send(&s, duplicate, StatusCode::UNPROCESSABLE_ENTITY).await;
    let mut unknown = serde_json::from_str::<Value>(&body("a", "A")).unwrap();
    unknown["event"]["source_id"] = json!("t");
    send(
        &s,
        request(
            &t,
            "PUT",
            "s",
            "a",
            "key",
            ("if-none-match", "*"),
            unknown.to_string(),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let mut invalid = serde_json::from_str::<Value>(&body("a", "A")).unwrap();
    invalid["event"]["end"] = json!({"kind":"utc","value":"2025-11-02T17:00:00Z"});
    send(
        &s,
        request(
            &t,
            "PUT",
            "s",
            "a",
            "key",
            ("if-none-match", "*"),
            invalid.to_string(),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM events")
            .fetch_one(s.services.storage.pool())
            .await
            .unwrap(),
        0
    );
}
