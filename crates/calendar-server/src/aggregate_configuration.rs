//! Aggregate management, mounted under the existing identity/session/Origin/nonce guard.
use crate::{
    admin::{protect, AdminState},
    source_configuration::condition,
};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Extension, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use calendar_adapters::admin_auth::AdminIdentity;
use calendar_application::{
    aggregate::*, configuration::ConfigurationCondition as Condition, SecretBytes,
};
use serde_json::json;
use std::sync::Arc;
pub(crate) fn router(state: AdminState) -> Router {
    let routes = Router::new()
        .route("/api/admin/v1/subscriptions", get(list))
        .route(
            "/api/admin/v1/subscriptions/{id}",
            get(read).put(write).delete(retire),
        )
        .route("/api/admin/v1/subscriptions/{id}/token", post(rotate))
        .with_state(state.clone())
        .layer(DefaultBodyLimit::max(65536));
    protect(state, routes)
}
fn error(e: AggregateError) -> StatusCode {
    match e {
        AggregateError::Invalid | AggregateError::Denied => StatusCode::UNPROCESSABLE_ENTITY,
        AggregateError::NotFound => StatusCode::NOT_FOUND,
        AggregateError::Conflict => StatusCode::CONFLICT,
        AggregateError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        AggregateError::Storage => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
fn receipt(record: AggregateRecord, secret: Option<SecretBytes>, created: bool) -> Response {
    let revision = record.revision;
    let mut response = if let Some(secret) = secret {
        let path = format!(
            "/feeds/aggregates/{}/{}.ics",
            record.public_id,
            std::str::from_utf8(secret.expose()).expect("generated ASCII token")
        );
        (
            if created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            },
            Json(json!({"subscription":record,"capability_url":path})),
        )
            .into_response()
    } else {
        Json(record).into_response()
    };
    response.headers_mut().insert(
        "etag",
        HeaderValue::from_str(&format!("\"{revision}\"")).unwrap(),
    );
    response
}
fn time(s: &AdminState) -> Result<i64, StatusCode> {
    // Business subscription expiry follows the explicit harness clock; identity/session
    // verification still uses AdminState.clock and production retains its original path.
    #[cfg(feature = "test-harness")]
    if let Some(clock) = s.services.test_business_clock.get() {
        let now = clock.utc_now_ms();
        return (now >= 0)
            .then_some(now)
            .ok_or(StatusCode::SERVICE_UNAVAILABLE);
    }
    (s.clock)()
        .checked_mul(1000)
        .filter(|v| *v >= 0)
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)
}
async fn read(State(s): State<AdminState>, Path(id): Path<String>) -> Result<Response, StatusCode> {
    let r = s
        .services
        .run_task(async |db| db.aggregate(&id).await)
        .await
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(error)?;
    Ok(receipt(r, None, false))
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    after: Option<String>,
}
async fn list(
    State(s): State<AdminState>,
    Query(q): Query<Cursor>,
) -> Result<Json<AggregatePage>, StatusCode> {
    let p = s
        .services
        .run_task(async |db| db.aggregates(q.after.as_deref()).await)
        .await
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(error)?;
    Ok(Json(p))
}
async fn write(
    State(s): State<AdminState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Extension(actor): Extension<Arc<AdminIdentity>>,
    Json(config): Json<AggregateConfiguration>,
) -> Result<Response, StatusCode> {
    let c = condition(&headers).ok_or(StatusCode::UNPROCESSABLE_ENTITY)?;
    let now = time(&s)?;
    let (r, secret) = s
        .services
        .run_task(async |db| db.put_aggregate(&id, c, config, actor.binding(), now).await)
        .await
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(error)?;
    Ok(receipt(r, secret, c == Condition::Absent))
}
fn revision(headers: &HeaderMap) -> Result<i64, StatusCode> {
    match condition(headers) {
        Some(Condition::Revision(n)) => Ok(n),
        _ => Err(StatusCode::UNPROCESSABLE_ENTITY),
    }
}
async fn rotate(
    State(s): State<AdminState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Extension(actor): Extension<Arc<AdminIdentity>>,
    body: Bytes,
) -> Result<Response, StatusCode> {
    if !body.is_empty() {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    let rev = revision(&headers)?;
    let now = time(&s)?;
    let grant = s
        .services
        .run_task(async |db| db.rotate_aggregate(&id, rev, actor.binding(), now).await)
        .await
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(error)?;
    Ok(receipt(grant.record, Some(grant.secret), false))
}
async fn retire(
    State(s): State<AdminState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Extension(actor): Extension<Arc<AdminIdentity>>,
    body: Bytes,
) -> Result<StatusCode, StatusCode> {
    if !body.is_empty() {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    let rev = revision(&headers)?;
    let now = time(&s)?;
    s.services
        .run_task(async |db| db.retire_aggregate(&id, rev, actor.binding(), now).await)
        .await
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(error)?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) fn describe(document: &mut serde_json::Value) {
    let config: serde_json::Value = serde_json::from_str(include_str!(
        "../../../schemas/aggregate-subscription-v1.schema.json"
    ))
    .expect("embedded aggregate schema");
    document["components"]["schemas"]["AggregateConfigurationV1"] = config;
    document["components"]["schemas"]["AggregateRecord"] = json!({"type":"object","additionalProperties":false,"required":["subscription_id","public_id","revision","revoked_at","configuration"],"properties":{"subscription_id":{"type":"string"},"public_id":{"type":"string"},"revision":{"type":"integer","minimum":1},"revoked_at":{"type":["integer","null"]},"configuration":{"$ref":"#/components/schemas/AggregateConfigurationV1"}}});
    document["components"]["schemas"]["AggregateGrant"] = json!({"type":"object","additionalProperties":false,"required":["subscription","capability_url"],"properties":{"subscription":{"$ref":"#/components/schemas/AggregateRecord"},"capability_url":{"type":"string","readOnly":true,"description":"One-time relative capability URL; resolve against the public service origin. Never returned by GET/list or ordinary edit, never persist/log in UI."}}});
    document["components"]["schemas"]["AggregateSummary"] = json!({"type":"object","required":["subscription_id","public_id","revision","name","enabled","expires_at","revoked_at"],"properties":{"subscription_id":{"type":"string"},"public_id":{"type":"string"},"revision":{"type":"integer","minimum":1},"name":{"type":"string"},"enabled":{"type":"boolean"},"expires_at":{"type":["integer","null"]},"revoked_at":{"type":["integer","null"]}},"additionalProperties":false});
    document["components"]["schemas"]["AggregatePage"] = json!({"type":"object","required":["items","next_after"],"properties":{"items":{"type":"array","maxItems":100,"items":{"$ref":"#/components/schemas/AggregateSummary"}},"next_after":{"type":["string","null"]}},"additionalProperties":false});
    let parameter = |name: &str, place: &str, required: bool| json!({"name":name,"in":place,"required":required,"schema":{"type":"string"}});
    let result = |schema: &str| json!({"description":"No-store response; no capability except explicit grant","headers":{"ETag":{"schema":{"type":"string"}},"X-Admin-Write-Nonce":{"schema":{"type":"string"}}},"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{schema}")}}}});
    let mut detail = serde_json::Map::new();
    for method in ["get", "put", "delete", "post"] {
        let mut params = vec![
            parameter("id", "path", true),
            parameter("Cf-Access-Jwt-Assertion", "header", true),
            parameter("__Host-calrelay_admin", "cookie", true),
        ];
        if method != "get" {
            for name in ["Origin", "X-CSRF-Token", "X-Admin-Write-Nonce"] {
                params.push(parameter(name, "header", true));
            }
            params.push(parameter("If-Match", "header", method != "put"));
            if method == "put" {
                params.push(parameter("If-None-Match", "header", false));
            }
        }
        let mut op = json!({"summary":"Manage a stored aggregate subscription under the existing admin guard","description":"PUT creation requires disabled policy and If-None-Match: *; edit, rotation and retirement require quoted positive If-Match revision. Unsafe requests consume/rotate the existing nonce even on business failure. All responses no-store. No plaintext capability accepted.","parameters":params,"responses":{"400":{"description":"Malformed request"},"403":{"description":"Management authentication denied"},"404":{"description":"Not found"},"409":{"description":"CAS conflict or retired"},"413":{"description":"Body budget exceeded"},"422":{"description":"Invalid policy, selection or condition"},"500":{"description":"Persistence failed"},"503":{"description":"Stopping/unavailable"}}});
        match method {
            "put" => {
                op["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/AggregateConfigurationV1"}}}});
                op["responses"]["201"] = result("AggregateGrant");
                op["responses"]["200"] = result("AggregateRecord");
            }
            "get" => op["responses"]["200"] = result("AggregateRecord"),
            "delete" => op["responses"]["204"] = json!({"description":"Permanently revoked"}),
            _ => op["responses"]["200"] = result("AggregateGrant"),
        }
        if method == "post" {
            document["paths"]["/api/admin/v1/subscriptions/{id}/token"] = json!({"post":op});
        } else {
            detail.insert(method.into(), op);
        }
    }
    document["paths"]["/api/admin/v1/subscriptions/{id}"] = serde_json::Value::Object(detail);
    document["paths"]["/api/admin/v1/subscriptions"] = json!({"get":{"summary":"Discover at most 100 nonsecret summaries, including retired records","parameters":[parameter("Cf-Access-Jwt-Assertion","header",true),parameter("__Host-calrelay_admin","cookie",true),parameter("after","query",false)],"responses":{"200":result("AggregatePage"),"400":{"description":"Invalid query"},"403":{"description":"Denied"},"422":{"description":"Invalid cursor"},"500":{"description":"Persistence failed"},"503":{"description":"Stopping"}}}});
}

#[cfg(all(test, feature = "test-harness"))]
mod clock_tests {
    use super::*;
    use calendar_adapters::admin_auth::{AdminConfig, CloudflareVerifier};
    struct Business;
    impl calendar_application::Clock for Business {
        fn utc_now_ms(&self) -> i64 {
            123456
        }
        fn monotonic_now_ms(&self) -> u64 {
            0
        }
    }
    #[tokio::test]
    async fn business_expiry_clock_does_not_replace_identity_session_clock() {
        let dir = tempfile::tempdir().unwrap();
        let server = crate::Server::initialize(crate::Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            database: dir.path().join("db"),
            max_request_bytes: 1024,
            max_response_bytes: 16384,
            request_timeout: std::time::Duration::from_secs(1),
        })
        .await
        .unwrap();
        let verifier = CloudflareVerifier::new(
            AdminConfig {
                team: "synthetic-team".into(),
                audience: "synthetic-aud".into(),
                origin: "https://localhost:4443".into(),
                hostname: "localhost".into(),
                action: "admin-login".into(),
                administrators: [("test".into(), "test@example.test".into())].into(),
                session_seconds: 120,
            },
            SecretBytes::new(b"synthetic-secret".to_vec()),
        )
        .unwrap();
        let state =
            AdminState::with_clock(server.services.clone(), Arc::new(verifier), Arc::new(|| 42));
        assert_eq!(time(&state).unwrap(), 42000);
        assert!(server
            .services
            .test_business_clock
            .set(Arc::new(Business))
            .is_ok());
        assert_eq!(time(&state).unwrap(), 123456);
        assert_eq!((state.clock)(), 42);
        server.services.drain().await;
    }
}
