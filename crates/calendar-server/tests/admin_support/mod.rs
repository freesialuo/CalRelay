//! Shared admin-session harness for HTTP tests against the real guard and SQLite.
#![allow(dead_code)]
use super::support::*;
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use calendar_application::configuration::*;
use calendar_server::{
    admin::{self, AdminState},
    Server,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

const ORIGIN: &str = "https://admin.example.test";

pub struct Admin {
    app: Router,
    jwt: String,
    cookie: String,
    csrf: String,
    nonce: String,
}
impl Admin {
    pub async fn login(server: &Server) -> Self {
        let state = AdminState::with_clock(
            server.services.clone(),
            verifier(Arc::new(Fake::default())),
            Arc::new(|| NOW),
        );
        let app = admin::router(state);
        let jwt = sign(claims());
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/admin/v1/session")
                    .header("cf-access-jwt-assertion", &jwt)
                    .header("origin", ORIGIN)
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"turnstile_token":"tokens"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let cookie = response.headers()["set-cookie"].to_str().unwrap();
        let cookie = cookie.split(';').next().unwrap().to_owned();
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        Self {
            app,
            jwt,
            cookie,
            csrf: body["csrf_token"].as_str().unwrap().into(),
            nonce: body["write_nonce"].as_str().unwrap().into(),
        }
    }
    pub async fn call(
        &mut self,
        method: &str,
        path: &str,
        body: Option<Value>,
        guarded: bool,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("cf-access-jwt-assertion", &self.jwt)
            .header("cookie", &self.cookie)
            .header("origin", ORIGIN)
            .header("content-type", "application/json");
        if guarded {
            request = request
                .header("x-csrf-token", &self.csrf)
                .header("x-admin-write-nonce", &self.nonce);
        }
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        if let Some(next) = response.headers().get("x-admin-write-nonce") {
            self.nonce = next.to_str().unwrap().into();
        }
        assert_eq!(response.headers()["cache-control"], "no-store");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    pub async fn issue(&mut self, source: &str, body: Value) -> (StatusCode, Value) {
        self.call(
            "POST",
            &format!("/api/admin/v1/sources/{source}/tokens"),
            Some(body),
            true,
        )
        .await
    }
}
pub async fn source(server: &Server, id: &str, mode: SourceMode) -> SourceConfigurationRecord {
    let db = &server.services.storage;
    let mut config: SourceConfigurationV1 = serde_json::from_str(include_str!(
        "../../../../examples/source-configuration-v1.json"
    ))
    .unwrap();
    config.calendars[0].id = format!("c_{id}");
    config.mode = mode.clone();
    if mode == SourceMode::PushOnly {
        config.adapter = AdapterKind::StandardCalendar;
    } else {
        config.endpoint = Some("https://source.example/pull".into());
        config.refresh_interval_ms = Some(60000);
        config.freshness_sla_ms = Some(60000);
    }
    let created = db
        .put_source_configuration(id, ConfigurationCondition::Absent, config, "test", 1)
        .await
        .unwrap();
    let mut config = created.configuration;
    config.enabled = true;
    db.put_source_configuration(
        id,
        ConfigurationCondition::Revision(created.config_revision),
        config,
        "test",
        1,
    )
    .await
    .unwrap()
}
