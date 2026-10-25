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
