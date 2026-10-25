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
    config["calendars"] = (0..50).map(|n|json!({"id":format!("calendar-{n}"),"external_id":format!("main-{n}"),"selected":true,"timezone":"UTC"})).collect();
    assert!(config.to_string().len() > 4096);
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
    for (condition, expected, mut body) in [
        ("*", StatusCode::CREATED, config.clone()),
        ("\"1\"", StatusCode::OK, config.clone()),
        ("\"1\"", StatusCode::CONFLICT, config.clone()),
        ("\"2\"", StatusCode::UNPROCESSABLE_ENTITY, config.clone()),
    ] {
        if expected == StatusCode::UNPROCESSABLE_ENTITY {
            body["source_id"] = json!("forged");
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
