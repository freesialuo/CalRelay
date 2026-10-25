#[path = "../../calendar-adapters/tests/support/admin.rs"]
mod support;
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use calendar_application::configuration::{
    ConfigurationCondition as Condition, SourceConfigurationV1,
};
use calendar_server::{
    admin::{self, AdminState},
    Config, Server,
};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use support::*;
use tower::ServiceExt;
struct Client {
    app: Router,
    jwt: String,
    cookie: String,
    csrf: String,
    nonce: String,
}
impl Client {
    async fn send(
        &mut self,
        method: &str,
        path: &str,
        condition: Option<&str>,
        body: Option<Value>,
    ) -> axum::response::Response {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header("cf-access-jwt-assertion", &self.jwt)
            .header("cookie", &self.cookie)
            .header("origin", "https://admin.example.test")
            .header("x-csrf-token", &self.csrf)
            .header("x-admin-write-nonce", &self.nonce)
            .header("content-type", "application/json");
        if let Some(c) = condition {
            b = b.header(
                if c == "*" {
                    "if-none-match"
                } else {
                    "if-match"
                },
                c,
            );
        }
        let r = self
            .app
            .clone()
            .oneshot(
                b.body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
                    .unwrap(),
            )
            .await
            .unwrap();
        if let Some(n) = r.headers().get("x-admin-write-nonce") {
            self.nonce = n.to_str().unwrap().into();
        }
        r
    }
}
async fn value(r: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(r.into_body(), 100000).await.unwrap()).unwrap()
}
async fn get(app: &Router, path: &str, etag: Option<&str>) -> axum::response::Response {
    let mut b = Request::builder().uri(path);
    if let Some(e) = etag {
        b = b.header("if-none-match", e);
    }
    app.clone()
        .oneshot(b.body(Body::empty()).unwrap())
        .await
        .unwrap()
}
#[tokio::test]
async fn management_crud_and_anonymous_aggregate_share_real_guards_and_revision_fences() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::initialize(Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        database: dir.path().join("db"),
        max_request_bytes: 65536,
        max_response_bytes: 200000,
        request_timeout: Duration::from_secs(10),
    })
    .await
    .unwrap();
    let db = &server.services.storage;
    for (id, calendar) in [("s1", "c1"), ("s2", "c2")] {
        let mut c: SourceConfigurationV1 = serde_json::from_str(include_str!(
            "../../../examples/source-configuration-v1.json"
        ))
        .unwrap();
        c.calendars[0].id = calendar.into();
        db.put_source_configuration(id, Condition::Absent, c.clone(), "test", 1)
            .await
            .unwrap();
        c.enabled = true;
        db.put_source_configuration(id, Condition::Revision(1), c, "test", 2)
            .await
            .unwrap();
        let doc=json!({"schema_version":"1.0","external_uid":"same-upstream","title":format!("title-{id}"),"description":"hidden-details","status":"confirmed","start":{"kind":"utc","value":"2026-01-01T00:00:00Z"},"duration":"PT1H"}).to_string();
        sqlx::query("INSERT INTO events(id,source_id,calendar_id,external_uid,revision,document_json,content_hash) VALUES (?,?,?,'same-upstream',1,?,'hash')").bind(format!("event-{id}")).bind(id).bind(calendar).bind(&doc).execute(db.pool()).await.unwrap();
        sqlx::query("INSERT INTO event_revisions(event_id,revision,document_json,change_kind,changed_at) VALUES (?,1,?,'upsert',1767225600000)").bind(format!("event-{id}")).bind(&doc).execute(db.pool()).await.unwrap();
    }
    let app = server.router().merge(admin::router(AdminState::with_clock(
        server.services.clone(),
        verifier(Arc::new(Fake::default())),
        Arc::new(|| NOW),
    )));
    let path = "/api/admin/v1/subscriptions/sub";
    assert_eq!(get(&app, path, None).await.status(), StatusCode::FORBIDDEN);
    let jwt = sign(claims());
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/v1/session")
                .header("cf-access-jwt-assertion", &jwt)
                .header("origin", "https://admin.example.test")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"turnstile_token":"synthetic"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let cookie = r.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let tokens = value(r).await;
    let mut c = Client {
        app: app.clone(),
        jwt,
        cookie,
        csrf: tokens["csrf_token"].as_str().unwrap().into(),
        nonce: tokens["write_nonce"].as_str().unwrap().into(),
    };
    let mut config = json!({"version":1,"name":"Combined","enabled":false,"sources":[{"source_id":"s1","calendar_ids":["c1"]},{"source_id":"s2","calendar_ids":["c2"]}],"visible_fields":["summary"],"stale_policy":"reject","expires_at":null});
    let saved = c.csrf.clone();
    c.csrf = "forged".into();
    assert_eq!(
        c.send("PUT", path, Some("*"), Some(config.clone()))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    c.csrf = saved;
    assert!(db.aggregate("sub").await.is_err());
    let r = c.send("PUT", path, Some("*"), Some(config.clone())).await;
    assert_eq!(r.status(), StatusCode::CREATED);
    assert_eq!(r.headers()["cache-control"], "no-store");
    let grant = value(r).await;
    let url = grant["capability_url"].as_str().unwrap();
    assert!(url.starts_with("/feeds/aggregates/"));
    assert_eq!(get(&app, url, None).await.status(), StatusCode::NOT_FOUND);
    config["enabled"] = json!(true);
    let r = c
        .send("PUT", path, Some("\"1\""), Some(config.clone()))
        .await;
    assert_eq!(r.status(), StatusCode::OK);
    assert!(value(r).await.get("capability_url").is_none());
    let r = get(&app, url, None).await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["cache-control"], "private, no-cache");
    let etag = r.headers()["etag"].to_str().unwrap().to_owned();
    let body = String::from_utf8(to_bytes(r.into_body(), 100000).await.unwrap().to_vec()).unwrap();
    assert_eq!(body.matches("BEGIN:VEVENT").count(), 2);
    let uids: Vec<_> = body
        .lines()
        .filter(|l| l.starts_with("UID:"))
        .map(str::to_owned)
        .collect();
    assert_eq!(uids.len(), 2);
    assert_ne!(uids[0], uids[1]);
    assert!(!body.contains("hidden-details"));
    assert_eq!(
        get(&app, url, Some(&etag)).await.status(),
        StatusCode::NOT_MODIFIED
    );
    assert_eq!(
        c.send("PUT", path, Some("\"1\""), Some(config.clone()))
            .await
            .status(),
        StatusCode::CONFLICT
    );
    config["visible_fields"] = json!([]);
    let r = c
        .send("PUT", path, Some("\"2\""), Some(config.clone()))
        .await;
    assert_eq!(r.status(), StatusCode::OK);
    let r = get(&app, url, Some(&etag)).await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_ne!(r.headers()["etag"], etag);
    let redacted =
        String::from_utf8(to_bytes(r.into_body(), 100000).await.unwrap().to_vec()).unwrap();
    assert!(!redacted.contains("title-s"));
    assert_eq!(
        redacted
            .lines()
            .filter(|l| l.starts_with("UID:"))
            .collect::<Vec<_>>(),
        uids.iter().map(String::as_str).collect::<Vec<_>>()
    );
    for p in [path, "/api/admin/v1/subscriptions"] {
        let r = c.send("GET", p, None, None).await;
        assert_eq!(r.status(), StatusCode::OK);
        let text = value(r).await.to_string();
        assert!(!text.contains(url));
        assert!(!text.contains("capability_url"));
    }
    // The shared encoder must charge the original documents across all members,
    // even though the current policy hides summary. Each member fits by itself.
    let mut writer = db.pool().begin().await.unwrap();
    sqlx::query("UPDATE events SET document_json=json_set(document_json,'$.title',printf('%.*c',25000,'x'))").execute(&mut *writer).await.unwrap();
    sqlx::query("UPDATE sources SET revision=revision+1")
        .execute(&mut *writer)
        .await
        .unwrap();
    writer.commit().await.unwrap();
    let too_large = get(&app, url, Some(&etag)).await;
    assert_eq!(too_large.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(!too_large.headers().contains_key("etag"));
    sqlx::query("UPDATE events SET document_json=json_set(document_json,'$.title','restored')")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE sources SET revision=revision+1")
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(get(&app, url, None).await.status(), StatusCode::OK);
    let source = db.source_configuration("s1").await.unwrap();
    let mut disabled = source.configuration;
    disabled.enabled = false;
    db.put_source_configuration(
        "s1",
        Condition::Revision(source.config_revision),
        disabled.clone(),
        "test",
        3,
    )
    .await
    .unwrap();
    assert_eq!(
        get(&app, url, Some(&etag)).await.status(),
        StatusCode::NOT_FOUND
    );
    disabled.enabled = true;
    db.put_source_configuration("s1", Condition::Revision(3), disabled, "test", 4)
        .await
        .unwrap();
    assert_eq!(get(&app, url, None).await.status(), StatusCode::OK);
    let r = c
        .send("POST", &format!("{path}/token"), Some("\"3\""), None)
        .await;
    assert_eq!(r.status(), StatusCode::OK);
    let fresh = value(r).await;
    let fresh_url = fresh["capability_url"].as_str().unwrap();
    assert_ne!(fresh_url, url);
    assert_eq!(
        get(&app, url, Some(&etag)).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(get(&app, fresh_url, None).await.status(), StatusCode::OK);
    assert_eq!(
        c.send("DELETE", path, Some("\"4\""), None).await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        get(&app, fresh_url, Some("*")).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        c.send("PUT", path, Some("\"5\""), Some(config))
            .await
            .status(),
        StatusCode::CONFLICT
    );
    let audit: String =
        sqlx::query_scalar("SELECT group_concat(actor_digest) FROM aggregate_subscription_audit")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(!audit.contains(url));
    assert!(!audit.contains("test"));
    server.services.drain().await;
}
