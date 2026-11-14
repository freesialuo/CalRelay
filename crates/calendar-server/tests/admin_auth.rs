#[path = "../../calendar-adapters/tests/support/admin.rs"]
mod support;
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    routing::post,
    Router,
};
use calendar_server::{
    admin::{self, AdminState},
    Config, Server,
};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicI64, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use support::*;
use tower::ServiceExt;
fn server_config(path: &std::path::Path) -> Config {
    Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        database: path.into(),
        max_request_bytes: 8192,
        max_response_bytes: 8192,
        request_timeout: Duration::from_secs(10),
    }
}
#[allow(clippy::too_many_arguments)] // Explicit independent headers keep security-negative cases reviewable.
fn request(
    method: &str,
    path: &str,
    jwt: &str,
    cookie: Option<&str>,
    csrf: Option<&str>,
    nonce: Option<&str>,
    origin: Option<&str>,
    body: Value,
) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header("cf-access-jwt-assertion", jwt)
        .header("content-type", "application/json");
    for (name, value) in [
        ("cookie", cookie),
        ("x-csrf-token", csrf),
        ("x-admin-write-nonce", nonce),
        ("origin", origin),
    ] {
        if let Some(v) = value {
            b = b.header(name, v);
        }
    }
    b.body(Body::from(body.to_string())).unwrap()
}
const SESSION: &str = "/api/admin/v1/session";
const WRITE: &str = "/api/admin/v1/test-write";
const ORIGIN: &str = "https://admin.example.test";
#[tokio::test]
async fn router_binds_access_session_origin_csrf_and_single_use_writes() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::initialize(server_config(&dir.path().join("db")))
        .await
        .unwrap();
    let clock = Arc::new(AtomicI64::new(NOW));
    let time = clock.clone();
    let fake = Arc::new(Fake::default());
    let state = AdminState::with_clock(
        server.services.clone(),
        verifier(fake.clone()),
        Arc::new(move || time.load(Ordering::SeqCst)),
    );
    let writes = Arc::new(AtomicUsize::new(0));
    let counter = writes.clone();
    let app = admin::router(state.clone()).merge(admin::protect(
        state,
        Router::new().route(
            WRITE,
            post(move || {
                let c = counter.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    StatusCode::NO_CONTENT
                }
            }),
        ),
    ));
    let jwt = sign(claims());
    // A direct origin request with a forged identity header cannot perform a write.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(WRITE)
                .header("cf-access-authenticated-user-email", "alice@example.test")
                .header("origin", ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        app.clone()
            .oneshot(request(
                "POST",
                WRITE,
                &jwt,
                None,
                None,
                None,
                Some(ORIGIN),
                json!({})
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            SESSION,
            &jwt,
            None,
            None,
            None,
            Some(ORIGIN),
            json!({"turnstile_token":"challenge-1"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let set_cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(set_cookie.contains("Secure; HttpOnly; SameSite=Strict; Path=/"));
    let cookie = set_cookie.split(';').next().unwrap();
    let value: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    let csrf = value["csrf_token"].as_str().unwrap();
    let nonce = value["write_nonce"].as_str().unwrap();
    assert_eq!(
        app.clone()
            .oneshot(request(
                "POST",
                SESSION,
                &jwt,
                None,
                None,
                None,
                Some(ORIGIN),
                json!({"turnstile_token":"challenge-1"})
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let mut bob = claims();
    bob["sub"] = json!("bob-sub");
    bob["email"] = json!("bob@example.test");
    for (jwt, origin, csrf, nonce) in [
        (sign(bob), Some(ORIGIN), Some(csrf), Some(nonce)),
        (jwt.clone(), None, Some(csrf), Some(nonce)),
        (
            jwt.clone(),
            Some("https://evil.test"),
            Some(csrf),
            Some(nonce),
        ),
        (jwt.clone(), Some("null"), Some(csrf), Some(nonce)),
        (jwt.clone(), Some(ORIGIN), None, Some(nonce)),
        (jwt.clone(), Some(ORIGIN), Some("forged"), Some(nonce)),
        (jwt.clone(), Some(ORIGIN), Some(csrf), Some("forged")),
    ] {
        assert_eq!(
            app.clone()
                .oneshot(request(
                    "POST",
                    WRITE,
                    &jwt,
                    Some(cookie),
                    csrf,
                    nonce,
                    origin,
                    json!({})
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    let mut duplicate = request(
        "POST",
        WRITE,
        &jwt,
        Some(cookie),
        Some(csrf),
        Some(nonce),
        Some(ORIGIN),
        json!({}),
    );
    duplicate
        .headers_mut()
        .append("origin", ORIGIN.parse().unwrap());
    assert_eq!(
        app.clone().oneshot(duplicate).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let mut duplicate = request(
        "GET",
        SESSION,
        &jwt,
        Some(cookie),
        None,
        None,
        None,
        json!({}),
    );
    duplicate
        .headers_mut()
        .append("cookie", cookie.parse().unwrap());
    assert_eq!(
        app.clone().oneshot(duplicate).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let (a, b) = tokio::join!(
        app.clone().oneshot(request(
            "POST",
            WRITE,
            &jwt,
            Some(cookie),
            Some(csrf),
            Some(nonce),
            Some(ORIGIN),
            json!({})
        )),
        app.clone().oneshot(request(
            "POST",
            WRITE,
            &jwt,
            Some(cookie),
            Some(csrf),
            Some(nonce),
            Some(ORIGIN),
            json!({})
        ))
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_ne!(a.status(), b.status());
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    let success = if a.status() == StatusCode::NO_CONTENT {
        a
    } else {
        b
    };
    let next = success.headers()["x-admin-write-nonce"].to_str().unwrap();
    assert_eq!(
        app.clone()
            .oneshot(request(
                "DELETE",
                SESSION,
                &jwt,
                Some(cookie),
                Some(csrf),
                Some(next),
                Some(ORIGIN),
                json!({})
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        app.clone()
            .oneshot(request(
                "GET",
                SESSION,
                &jwt,
                Some(cookie),
                None,
                None,
                None,
                json!({})
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    // A fresh challenge issues an independent session, expired at the exact TTL boundary.
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            SESSION,
            &jwt,
            None,
            None,
            None,
            Some(ORIGIN),
            json!({"turnstile_token":"challenge-2"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    clock.store(NOW + 120, Ordering::SeqCst);
    assert_eq!(
        app.clone()
            .oneshot(request(
                "GET",
                SESSION,
                &jwt,
                Some(&cookie),
                None,
                None,
                None,
                json!({})
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    clock.store(NOW, Ordering::SeqCst);
    *fake.fail.lock().unwrap() = true;
    assert_eq!(
        app.clone()
            .oneshot(request(
                "GET",
                SESSION,
                &jwt,
                Some(&cookie),
                None,
                None,
                None,
                json!({})
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    server.services.drain().await;
}
#[tokio::test]
async fn default_binary_mounts_no_management_permission_and_enabled_boundary_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::initialize(server_config(&dir.path().join("db")))
        .await
        .unwrap();
    let jwt = sign(claims());
    assert_eq!(
        server
            .router()
            .oneshot(request(
                "POST",
                SESSION,
                &jwt,
                None,
                None,
                None,
                Some(ORIGIN),
                json!({"turnstile_token":"challenge"})
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let server = server.with_admin(verifier(Arc::new(Fake::default())));
    let response = server
        .router()
        .oneshot(request(
            "POST",
            SESSION,
            "forged",
            None,
            None,
            None,
            Some(ORIGIN),
            json!({"turnstile_token":"challenge"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(response.headers().contains_key("x-request-id"));
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
    assert_eq!(body["error"]["code"], "forbidden");
    assert!(!body.to_string().contains("forged"));
    server.services.drain().await;
}

#[tokio::test]
async fn source_configuration_crud_is_guarded_persistent_and_conditional() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.db");
    let server = Server::initialize(server_config(&path)).await.unwrap();
    let state = AdminState::with_clock(
        server.services.clone(),
        verifier(Arc::new(Fake::default())),
        Arc::new(|| NOW),
    );
    let app = admin::router(state);
    let jwt = sign(claims());
    let path = "/api/admin/v1/sources/source-a";
    let mut config: Value = serde_json::from_str(include_str!(
        "../../../examples/source-configuration-v1.json"
    ))
    .unwrap();
    // Many calendar mappings: only standard-calendar sources may map more than one calendar.
    config["adapter"] = json!("standard-calendar");
    config["calendars"] = (0..50).map(|n|json!({"id":format!("calendar-{n}"),"external_id":format!("main-{n}"),"selected":true,"timezone":"UTC"})).collect();
    assert!(config.to_string().len() > 4096);
    let mut forged = config.clone();
    forged["source_id"] = json!("forged");
    let mut multi_calendar_ics = config.clone();
    multi_calendar_ics["adapter"] = json!("ics");
    let login = app
        .clone()
        .oneshot(request(
            "POST",
            SESSION,
            &jwt,
            None,
            None,
            None,
            Some(ORIGIN),
            json!({"turnstile_token":"configuration"}),
        ))
        .await
        .unwrap();
    let cookie = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let tokens: Value =
        serde_json::from_slice(&to_bytes(login.into_body(), 4096).await.unwrap()).unwrap();
    let csrf = tokens["csrf_token"].as_str().unwrap();
    let mut nonce = tokens["write_nonce"].as_str().unwrap().to_owned();
    for (cookie, csrf, origin) in [
        (None, Some(csrf), Some(ORIGIN)),
        (Some(cookie.as_str()), None, Some(ORIGIN)),
        (Some(cookie.as_str()), Some(csrf), Some("https://evil.test")),
    ] {
        let mut req = request(
            "PUT",
            path,
            &jwt,
            cookie,
            csrf,
            Some(&nonce),
            origin,
            config.clone(),
        );
        req.headers_mut()
            .insert("if-none-match", "*".parse().unwrap());
        assert_eq!(
            app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
    assert!(server
        .services
        .storage
        .source_configuration("source-a")
        .await
        .is_err());
    for (condition, expected, body) in [
        ("*", StatusCode::CREATED, config.clone()),
        ("\"1\"", StatusCode::OK, config.clone()),
        ("\"1\"", StatusCode::CONFLICT, config.clone()),
        ("\"2\"", StatusCode::UNPROCESSABLE_ENTITY, forged),
        (
            "\"2\"",
            StatusCode::UNPROCESSABLE_ENTITY,
            multi_calendar_ics,
        ),
    ] {
        let mut req = request(
            "PUT",
            path,
            &jwt,
            Some(&cookie),
            Some(csrf),
            Some(&nonce),
            Some(ORIGIN),
            body,
        );
        req.headers_mut().insert(
            if condition == "*" {
                "if-none-match"
            } else {
                "if-match"
            },
            condition.parse().unwrap(),
        );
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), expected);
        nonce = response.headers()["x-admin-write-nonce"]
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(response.headers()["cache-control"], "no-store");
        if expected.is_success() {
            assert!(response.headers().contains_key("etag"));
            let receipt: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap())
                    .unwrap();
            assert_eq!(receipt["source_id"], "source-a");
        }
    }
    let response = app
        .clone()
        .oneshot(request(
            "GET",
            path,
            &jwt,
            Some(&cookie),
            None,
            None,
            None,
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["etag"], "\"2\"");
    let persisted = server
        .services
        .storage
        .source_configuration("source-a")
        .await
        .unwrap();
    assert_eq!(persisted.config_revision, 2);
    let page = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/admin/v1/sources",
            &jwt,
            Some(&cookie),
            None,
            None,
            None,
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    let page: Value =
        serde_json::from_slice(&to_bytes(page.into_body(), 8192).await.unwrap()).unwrap();
    assert_eq!(page["items"][0]["source_id"], "source-a");
    assert!(page["items"][0].get("configuration").is_none());
    assert!(page["next_after"].is_null());
    // Machine credentials cannot be used in place of the Access/session identity.
    let machine = server
        .services
        .storage
        .issue_api_token("producer", "source-a", true, true, None)
        .await
        .unwrap();
    let denied = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header(
                    "authorization",
                    format!("Bearer {}", std::str::from_utf8(machine.expose()).unwrap()),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    let mut retire = request(
        "DELETE",
        path,
        &jwt,
        Some(&cookie),
        Some(csrf),
        Some(&nonce),
        Some(ORIGIN),
        Value::Null,
    );
    *retire.body_mut() = Body::empty();
    retire
        .headers_mut()
        .insert("if-match", "\"2\"".parse().unwrap());
    assert_eq!(
        app.oneshot(retire).await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    server.services.drain().await;
    let reopened =
        calendar_storage::Storage::open(&server_config(&dir.path().join("config.db")).database)
            .await
            .unwrap();
    let retired = reopened.source_configuration("source-a").await.unwrap();
    assert_eq!(retired.config_revision, 3);
    assert!(retired.retired_at.is_some());
    reopened.close().await;
}

#[tokio::test]
async fn manual_refresh_is_write_guarded_and_only_accepts_active_pull_sources() {
    use calendar_application::configuration::{
        ConfigurationCondition, SourceConfigurationV1, SourceMode,
    };
    let dir = tempfile::tempdir().unwrap();
    let server = Server::initialize(server_config(&dir.path().join("refresh.db")))
        .await
        .unwrap();
    let db = &server.services.storage;
    for (id, mode) in [
        ("pull", SourceMode::Pull),
        ("static", SourceMode::ManualStatic),
    ] {
        let mut config: SourceConfigurationV1 = serde_json::from_str(include_str!(
            "../../../examples/source-configuration-v1.json"
        ))
        .unwrap();
        config.name = id.into();
        config.calendars[0].id = format!("c_{id}");
        if mode == SourceMode::Pull {
            config.endpoint = Some("https://source.example/pull".into());
            config.refresh_interval_ms = Some(60000);
            config.freshness_sla_ms = Some(60000);
        }
        config.mode = mode;
        let record = db
            .put_source_configuration(id, ConfigurationCondition::Absent, config, "test", 1000)
            .await
            .unwrap();
        let mut config = record.configuration;
        config.enabled = true;
        let revision = ConfigurationCondition::Revision(record.config_revision);
        db.put_source_configuration(id, revision, config, "test", 1000)
            .await
            .unwrap();
    }
    let state = AdminState::with_clock(
        server.services.clone(),
        verifier(Arc::new(Fake::default())),
        Arc::new(|| NOW),
    );
    let app = admin::router(state);
    let jwt = sign(claims());
    let login = app
        .clone()
        .oneshot(request(
            "POST",
            SESSION,
            &jwt,
            None,
            None,
            None,
            Some(ORIGIN),
            json!({"turnstile_token":"refresh"}),
        ))
        .await
        .unwrap();
    let cookie = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let tokens: Value =
        serde_json::from_slice(&to_bytes(login.into_body(), 4096).await.unwrap()).unwrap();
    let csrf = tokens["csrf_token"].as_str().unwrap();
    let mut nonce = tokens["write_nonce"].as_str().unwrap().to_owned();
    let post = |id: &str, csrf: Option<&str>, nonce: &str, body: &'static str| {
        let mut req = request(
            "POST",
            &format!("/api/admin/v1/sources/{id}/refresh"),
            &jwt,
            Some(&cookie),
            csrf,
            Some(nonce),
            Some(ORIGIN),
            Value::Null,
        );
        *req.body_mut() = Body::from(body);
        req
    };
    let denied = app
        .clone()
        .oneshot(post("pull", None, &nonce, ""))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert_eq!(db.source_refresh_request("pull").await.unwrap(), None);
    for (id, body, expected) in [
        ("pull", "", StatusCode::ACCEPTED),
        ("pull", "", StatusCode::ACCEPTED),
        ("pull", "{}", StatusCode::UNPROCESSABLE_ENTITY),
        ("static", "", StatusCode::CONFLICT),
        ("missing", "", StatusCode::NOT_FOUND),
        ("bad.id", "", StatusCode::UNPROCESSABLE_ENTITY),
    ] {
        let response = app
            .clone()
            .oneshot(post(id, Some(csrf), &nonce, body))
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{id} {body:?}");
        nonce = response.headers()["x-admin-write-nonce"]
            .to_str()
            .unwrap()
            .to_owned();
        if expected == StatusCode::ACCEPTED {
            let receipt: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap())
                    .unwrap();
            assert_eq!(receipt["source_id"], "pull");
            assert!(receipt["requested_at"].is_i64());
            assert_eq!(receipt.as_object().unwrap().len(), 2);
        }
    }
    let pending = db.source_refresh_request("pull").await.unwrap().unwrap();
    assert_eq!(pending.sequence, 2, "repeated requests merge into one row");
    assert_eq!(db.source_refresh_request("static").await.unwrap(), None);
}
#[tokio::test]
async fn ics_upload_replaces_only_manual_static_snapshots_atomically() {
    use calendar_application::{
        configuration::{ConfigurationCondition, SourceConfigurationV1, SourceMode},
        SourceRepository,
    };
    let dir = tempfile::tempdir().unwrap();
    let server = Server::initialize(server_config(&dir.path().join("upload.db")))
        .await
        .unwrap();
    let db = &server.services.storage;
    for (id, mode) in [
        ("static", SourceMode::ManualStatic),
        ("pull", SourceMode::Pull),
    ] {
        let mut config: SourceConfigurationV1 = serde_json::from_str(include_str!(
            "../../../examples/source-configuration-v1.json"
        ))
        .unwrap();
        config.name = id.into();
        config.calendars[0].id = format!("c_{id}");
        if mode == SourceMode::Pull {
            config.endpoint = Some("https://source.example/pull".into());
            config.refresh_interval_ms = Some(60000);
            config.freshness_sla_ms = Some(60000);
        }
        config.mode = mode;
        let record = db
            .put_source_configuration(id, ConfigurationCondition::Absent, config, "test", 1000)
            .await
            .unwrap();
        let mut config = record.configuration;
        config.enabled = true;
        let revision = ConfigurationCondition::Revision(record.config_revision);
        db.put_source_configuration(id, revision, config, "test", 1000)
            .await
            .unwrap();
    }
    let state = AdminState::with_clock(
        server.services.clone(),
        verifier(Arc::new(Fake::default())),
        Arc::new(|| NOW),
    );
    let app = admin::router(state);
    let jwt = sign(claims());
    let login = app
        .clone()
        .oneshot(request(
            "POST",
            SESSION,
            &jwt,
            None,
            None,
            None,
            Some(ORIGIN),
            json!({"turnstile_token":"upload"}),
        ))
        .await
        .unwrap();
    let cookie = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let tokens: Value =
        serde_json::from_slice(&to_bytes(login.into_body(), 4096).await.unwrap()).unwrap();
    let csrf = tokens["csrf_token"].as_str().unwrap();
    let mut nonce = tokens["write_nonce"].as_str().unwrap().to_owned();
    let ics = |events: &[&str]| {
        let mut text = String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\n");
        for uid in events {
            text += &format!("BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260101T000000Z\r\nDTSTART:20261102T170000Z\r\nSUMMARY:Private {uid}\r\nEND:VEVENT\r\n");
        }
        text + "END:VCALENDAR\r\n"
    };
    let put = |id: &str, csrf: Option<&str>, nonce: &str, media: &str, tag: &str, body: String| {
        let mut req = request(
            "PUT",
            &if id.contains("/snapshot") {
                format!("/api/admin/v1/sources/{id}")
            } else {
                format!("/api/admin/v1/sources/{id}/snapshot")
            },
            &jwt,
            Some(&cookie),
            csrf,
            Some(nonce),
            Some(ORIGIN),
            Value::Null,
        );
        req.headers_mut()
            .insert("content-type", media.parse().unwrap());
        req.headers_mut().insert("if-match", tag.parse().unwrap());
        *req.body_mut() = Body::from(body);
        req
    };
    let calendar = "text/calendar; charset=utf-8";
    let denied = app
        .clone()
        .oneshot(put(
            "static",
            None,
            &nonce,
            calendar,
            "\"2\"",
            ics(&["one"]),
        ))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    let initial = db.source_state("static").await.unwrap();
    assert!(!initial.has_snapshot);
    let oversized = format!("{}{}", ics(&["one"]), " ".repeat(1_048_576));
    let many: Vec<String> = (0..102).map(|n| format!("bulk-{n}")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    // (source, media type, If-Match, body, expected status, changed events when committed)
    for (id, media, tag, body, expected, changed) in [
        (
            "static",
            "application/json",
            "\"2\"",
            ics(&["one"]),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            0,
        ),
        (
            "static",
            calendar,
            "\"1\"",
            ics(&["one"]),
            StatusCode::CONFLICT,
            0,
        ),
        (
            "pull",
            calendar,
            "\"2\"",
            ics(&["one"]),
            StatusCode::CONFLICT,
            0,
        ),
        (
            "missing",
            calendar,
            "\"2\"",
            ics(&["one"]),
            StatusCode::NOT_FOUND,
            0,
        ),
        (
            "static",
            calendar,
            "\"2\"",
            "BEGIN:VCALENDAR\r\nnot ics".into(),
            StatusCode::UNPROCESSABLE_ENTITY,
            0,
        ),
        (
            "static",
            calendar,
            "\"2\"",
            oversized,
            StatusCode::PAYLOAD_TOO_LARGE,
            0,
        ),
        (
            "static",
            calendar,
            "\"2\"",
            ics(&["one", "two"]),
            StatusCode::OK,
            2,
        ),
        // Replacement: "one" is deleted, "two" is unchanged, "three" is new.
        (
            "static",
            calendar,
            "\"2\"",
            ics(&["two", "three"]),
            StatusCode::OK,
            2,
        ),
        (
            "static/snapshot",
            calendar,
            "\"2\"",
            ics(&many),
            StatusCode::OK,
            104,
        ),
        // Dropping 102 events at once exceeds the deletion limit and changes nothing until the
        // same upload is explicitly approved; other query values and unknown keys are rejected.
        (
            "static/snapshot",
            calendar,
            "\"2\"",
            ics(&["only"]),
            StatusCode::PRECONDITION_REQUIRED,
            0,
        ),
        (
            "static/snapshot?approve_deletions=false",
            calendar,
            "\"2\"",
            ics(&["only"]),
            StatusCode::PRECONDITION_REQUIRED,
            0,
        ),
        (
            "static/snapshot?approve=true",
            calendar,
            "\"2\"",
            ics(&["only"]),
            StatusCode::BAD_REQUEST,
            0,
        ),
        (
            "static/snapshot?approve_deletions=true",
            calendar,
            "\"2\"",
            ics(&["only"]),
            StatusCode::OK,
            103,
        ),
    ] {
        let before = db.source_state("static").await.unwrap();
        let response = app
            .clone()
            .oneshot(put(id, Some(csrf), &nonce, media, tag, body))
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{id} {media} {tag}");
        nonce = response.headers()["x-admin-write-nonce"]
            .to_str()
            .unwrap()
            .to_owned();
        let after = db.source_state("static").await.unwrap();
        if expected == StatusCode::OK {
            let text = to_bytes(response.into_body(), 4096).await.unwrap();
            assert!(!String::from_utf8_lossy(&text).contains("Private"));
            let receipt: Value = serde_json::from_slice(&text).unwrap();
            assert_eq!(receipt.as_object().unwrap().len(), 3);
            assert_eq!(receipt["source_id"], "static");
            assert_eq!(receipt["changed_events"], changed);
            assert_eq!(receipt["source_revision"], after.revision);
            assert!(after.has_snapshot && after.revision > before.revision);
        } else {
            assert_eq!(after, before, "a rejected upload changes nothing");
        }
    }
    assert!(!db.source_state("pull").await.unwrap().has_snapshot);
    // A multi-calendar ICS row saved before #172 still cannot receive an upload.
    sqlx::query("UPDATE sources SET config_json=json_insert(config_json,'$.calendars[#]',json('{\"id\":\"c_second\",\"external_id\":\"second\",\"selected\":true,\"timezone\":null}')) WHERE id='static'")
        .execute(db.pool())
        .await
        .unwrap();
    let before = db.source_state("static").await.unwrap();
    let response = app
        .clone()
        .oneshot(put(
            "static/snapshot",
            Some(csrf),
            &nonce,
            calendar,
            "\"2\"",
            ics(&["late"]),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(db.source_state("static").await.unwrap(), before);
}
#[tokio::test]
async fn source_configuration_budget_does_not_expand_session_login_body_budget() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::initialize(server_config(&dir.path().join("db")))
        .await
        .unwrap();
    let state = AdminState::with_clock(
        server.services.clone(),
        verifier(Arc::new(Fake::default())),
        Arc::new(|| NOW),
    );
    let response = admin::router(state)
        .oneshot(request(
            "POST",
            SESSION,
            &sign(claims()),
            None,
            None,
            None,
            Some(ORIGIN),
            json!({"turnstile_token":"x".repeat(4096)}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    server.services.drain().await;
}

#[tokio::test]
async fn rule_and_bark_management_share_guards_cas_and_secret_free_persistence() {
    use calendar_application::{configuration::*, SecretBytes};
    use calendar_storage::security::{KeyRing, SecurityStore};
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db");
    let server = Server::initialize(server_config(&db_path)).await.unwrap();
    let key = dir.path().join("mock-key");
    std::fs::write(&key, [11; 32]).unwrap();
    let keys = KeyRing::from_files(1, &std::collections::BTreeMap::from([(1, key)])).unwrap();
    SecurityStore {
        storage: &server.services.storage,
        keys: &keys,
    }
    .create_credential(
        "bark-credential",
        "bark",
        &SecretBytes::new(b"synthetic-http-secret".to_vec()),
        NOW * 1000,
    )
    .await
    .unwrap();
    let source: SourceConfigurationV1 = serde_json::from_str(include_str!(
        "../../../examples/source-configuration-v1.json"
    ))
    .unwrap();
    server
        .services
        .storage
        .put_source_configuration(
            "source-a",
            ConfigurationCondition::Absent,
            source,
            "test",
            NOW * 1000,
        )
        .await
        .unwrap();
    let state = AdminState::with_clock(
        server.services.clone(),
        verifier(Arc::new(Fake::default())),
        Arc::new(|| NOW),
    );
    let app = admin::router(state);
    let jwt = sign(claims());
    let login = app
        .clone()
        .oneshot(request(
            "POST",
            SESSION,
            &jwt,
            None,
            None,
            None,
            Some(ORIGIN),
            json!({"turnstile_token":"reminder-configuration"}),
        ))
        .await
        .unwrap();
    let cookie = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let tokens: Value =
        serde_json::from_slice(&to_bytes(login.into_body(), 4096).await.unwrap()).unwrap();
    let csrf = tokens["csrf_token"].as_str().unwrap();
    let mut nonce = tokens["write_nonce"].as_str().unwrap().to_owned();
    let output: Value = serde_json::from_str(include_str!(
        "../../../examples/bark-output-configuration-v1.json"
    ))
    .unwrap();
    let rule: Value =
        serde_json::from_str(include_str!("../../../examples/rule-configuration-v1.json")).unwrap();
    for (path, body, expected) in [
        (
            "/api/admin/v1/outputs/bark-a",
            output.clone(),
            StatusCode::CREATED,
        ),
        (
            "/api/admin/v1/rules/rule-a",
            rule.clone(),
            StatusCode::CREATED,
        ),
    ] {
        // Invalid management identities/Origin/CSRF have no resource side effects.
        for (cookie, csrf, origin) in [
            (None, Some(csrf), Some(ORIGIN)),
            (Some(cookie.as_str()), None, Some(ORIGIN)),
            (Some(cookie.as_str()), Some(csrf), Some("https://evil.test")),
        ] {
            let mut req = request(
                "PUT",
                path,
                &jwt,
                cookie,
                csrf,
                Some(&nonce),
                origin,
                body.clone(),
            );
            req.headers_mut()
                .insert("if-none-match", "*".parse().unwrap());
            assert_eq!(
                app.clone().oneshot(req).await.unwrap().status(),
                StatusCode::FORBIDDEN
            );
        }
        let mut req = request(
            "PUT",
            path,
            &jwt,
            Some(&cookie),
            Some(csrf),
            Some(&nonce),
            Some(ORIGIN),
            body,
        );
        req.headers_mut()
            .insert("if-none-match", "*".parse().unwrap());
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), expected);
        assert_eq!(response.headers()["etag"], "\"1\"");
        assert_eq!(response.headers()["cache-control"], "no-store");
        nonce = response.headers()["x-admin-write-nonce"]
            .to_str()
            .unwrap()
            .to_owned();
        let body = to_bytes(response.into_body(), 8192).await.unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("synthetic-http-secret"));
    }
    for (path, mut body, condition, expected) in [
        (
            "/api/admin/v1/outputs/bark-a",
            output.clone(),
            "\"1\"",
            StatusCode::OK,
        ),
        (
            "/api/admin/v1/outputs/bark-a",
            output.clone(),
            "\"1\"",
            StatusCode::CONFLICT,
        ),
        (
            "/api/admin/v1/outputs/bark-a",
            output.clone(),
            "\"2\"",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "/api/admin/v1/rules/rule-a",
            rule.clone(),
            "\"1\"",
            StatusCode::OK,
        ),
    ] {
        if expected == StatusCode::UNPROCESSABLE_ENTITY {
            body["device_key"] = json!("synthetic-http-secret");
        }
        let mut req = request(
            "PUT",
            path,
            &jwt,
            Some(&cookie),
            Some(csrf),
            Some(&nonce),
            Some(ORIGIN),
            body,
        );
        req.headers_mut()
            .insert("if-match", condition.parse().unwrap());
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), expected);
        nonce = response.headers()["x-admin-write-nonce"]
            .to_str()
            .unwrap()
            .to_owned();
    }
    let mut duplicate = request(
        "PUT",
        "/api/admin/v1/outputs/bark-a",
        &jwt,
        Some(&cookie),
        Some(csrf),
        Some(&nonce),
        Some(ORIGIN),
        output.clone(),
    );
    duplicate
        .headers_mut()
        .insert("if-match", "\"2\"".parse().unwrap());
    *duplicate.body_mut() = Body::from(
        output
            .to_string()
            .replace("\"enabled\":false", "\"enabled\":false,\"enabled\":true"),
    );
    let duplicate = app.clone().oneshot(duplicate).await.unwrap();
    assert_eq!(duplicate.status(), StatusCode::UNPROCESSABLE_ENTITY);
    nonce = duplicate.headers()["x-admin-write-nonce"]
        .to_str()
        .unwrap()
        .to_owned();
    for path in [
        "/api/admin/v1/rules",
        "/api/admin/v1/outputs",
        "/api/admin/v1/rules/rule-a",
        "/api/admin/v1/outputs/bark-a",
    ] {
        let response = app
            .clone()
            .oneshot(request(
                "GET",
                path,
                &jwt,
                Some(&cookie),
                None,
                None,
                None,
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 8192).await.unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("synthetic-http-secret"));
        assert!(!text.contains("ciphertext"));
    }
    for path in ["/api/admin/v1/rules/rule-a", "/api/admin/v1/outputs/bark-a"] {
        let mut req = request(
            "DELETE",
            path,
            &jwt,
            Some(&cookie),
            Some(csrf),
            Some(&nonce),
            Some(ORIGIN),
            Value::Null,
        );
        *req.body_mut() = Body::empty();
        req.headers_mut()
            .insert("if-match", "\"2\"".parse().unwrap());
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        nonce = response.headers()["x-admin-write-nonce"]
            .to_str()
            .unwrap()
            .to_owned();
    }
    server.services.drain().await;
    let reopened = calendar_storage::Storage::open(&db_path).await.unwrap();
    assert!(reopened
        .rule_configuration("rule-a")
        .await
        .unwrap()
        .retired_at
        .is_some());
    assert!(reopened
        .bark_output_configuration("bark-a")
        .await
        .unwrap()
        .retired_at
        .is_some());
    reopened.close().await;
}
