//! Machine write endpoints. Never log bearer bytes, event bodies or idempotency keys.
use crate::{RequestId, Services};
use axum::{
    body::Bytes,
    extract::{Extension, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::put,
    Json, Router,
};
use calendar_application::{Clock, IngestCondition, IngestOperation, IngestPolicy, IngestRequest};
use calendar_storage::ingest::IngestError;
use serde::Deserialize;
use serde_json::json;
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

struct SystemClock;
impl Clock for SystemClock {
    fn utc_now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(i64::MAX)
    }
    fn monotonic_now_ms(&self) -> u64 {
        0
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Upsert {
    calendar_id: String,
    event: Box<calendar_application::EventV1>,
}

pub(crate) fn router() -> Router<Arc<Services>> {
    Router::new().route(
        "/api/ingest/v1/sources/{source_id}/events/{uid}",
        put(upsert).delete(delete),
    )
}
async fn upsert(
    State(s): State<Arc<Services>>,
    Path((source, uid)): Path<(String, String)>,
    headers: HeaderMap,
    Extension(id): Extension<RequestId>,
    body: Bytes,
) -> Response {
    let body: Upsert = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return StatusCode::UNPROCESSABLE_ENTITY.into_response(),
    };
    execute(
        s,
        source,
        uid,
        headers,
        id,
        IngestOperation::Upsert {
            calendar_id: body.calendar_id,
            event: body.event,
        },
    )
    .await
}
async fn delete(
    State(s): State<Arc<Services>>,
    Path((source, uid)): Path<(String, String)>,
    headers: HeaderMap,
    Extension(id): Extension<RequestId>,
    body: Bytes,
) -> Response {
    if !body.is_empty() {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    }
    execute(s, source, uid, headers, id, IngestOperation::Delete).await
}
fn single<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        None
    } else {
        Some(value)
    }
}
fn condition(headers: &HeaderMap) -> Option<IngestCondition> {
    match (
        single(headers, "if-match"),
        single(headers, "if-none-match"),
    ) {
        (None, Some("*")) if !headers.contains_key("if-match") => Some(IngestCondition::Absent),
        (Some(value), None) if !headers.contains_key("if-none-match") => {
            let n = value.strip_prefix('"')?.strip_suffix('"')?;
            let parsed: i64 = n.parse().ok()?;
            if parsed > 0 && n == parsed.to_string() {
                Some(IngestCondition::Revision(parsed))
            } else {
                None
            }
        }
        _ => None,
    }
}
async fn execute(
    s: Arc<Services>,
    source: String,
    uid: String,
    headers: HeaderMap,
    id: RequestId,
    operation: IngestOperation,
) -> Response {
    let secret = match single(&headers, "authorization").and_then(|s| s.strip_prefix("Bearer ")) {
        Some(v) => v.as_bytes(),
        None => {
            return (StatusCode::UNAUTHORIZED, [("www-authenticate", "Bearer")]).into_response()
        }
    };
    let Some(condition) = condition(&headers) else {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    };
    let Some(key) = single(&headers, "idempotency-key") else {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    };
    let request = IngestRequest {
        source_id: source,
        uid,
        key: key.into(),
        condition,
        operation,
    };
    let result = s
        .run_task(async |db| {
            db.ingest(secret, request, s.ingest_policy, &SystemClock, &id.0)
                .await
        })
        .await;
    let result = match result {
        None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Some(Err(e)) => {
            return match e {
                IngestError::Denied => {
                    (StatusCode::UNAUTHORIZED, [("www-authenticate", "Bearer")]).into_response()
                }
                IngestError::Invalid => StatusCode::UNPROCESSABLE_ENTITY.into_response(),
                IngestError::Conflict => StatusCode::CONFLICT.into_response(),
                IngestError::Capacity => StatusCode::SERVICE_UNAVAILABLE.into_response(),
                IngestError::Database => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            }
        }
        Some(Ok(v)) => v,
    };
    let mut response=(if result.created{StatusCode::CREATED}else{StatusCode::OK},Json(json!({
        "event_revision":result.event_revision,"source_revision":result.source_revision,"deleted":result.deleted,
        "request_id":result.request_id,"idempotency_expires_at":result.expires_at
    }))).into_response();
    response.headers_mut().insert(
        "etag",
        HeaderValue::from_str(&format!("\"{}\"", result.event_revision)).unwrap(),
    );
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response
}

/// Validated separately from the shared server config so parallel auth/feed work need not
/// change struct literals. Environment is sampled once during initialization.
pub(crate) fn policy() -> Result<IngestPolicy, crate::ConfigError> {
    let mut policy = IngestPolicy::default();
    for (name, target) in [
        ("CALRELAY_INGEST_RETENTION_MS", &mut policy.retention_ms),
        ("CALRELAY_INGEST_MAX_RECORDS", &mut policy.max_records),
    ] {
        if let Some(value) = std::env::var_os(name) {
            *target = value
                .to_str()
                .and_then(|s| s.parse().ok())
                .ok_or(crate::ConfigError("invalid ingest policy"))?;
        }
    }
    if !policy.valid() {
        return Err(crate::ConfigError("invalid ingest policy"));
    }
    Ok(policy)
}

pub(crate) fn describe(document: &mut serde_json::Value) {
    let parameter = |name: &str, location: &str, required: bool| json!({"name":name,"in":location,"required":required,"schema":{"type":"string"}});
    let mut operations = serde_json::Map::new();
    for method in ["put", "delete"] {
        let mut operation = json!({
            "summary":"Source-scoped conditional machine write",
            "security":[{"IngestBearer":[]}],
            "description":"Idempotency-Key is token-wide. Exactly one of If-Match (quoted positive revision) or If-None-Match: * is required. DELETE requires If-Match. Tombstones require their revision to resurrect. Retention expiry permits re-execution; unchanged writes do not advance revisions.",
            "parameters":[parameter("source_id","path",true),parameter("uid","path",true),parameter("Idempotency-Key","header",true),parameter("If-Match","header",false),parameter("If-None-Match","header",false)],
            "responses":{}
        });
        for code in ["200", "201", "401", "409", "422", "500", "503"] {
            operation["responses"][code] = json!({"description":match code {"200"|"201"=>"Committed or identical receipt replay","401"=>"Token denied (source, action, expiry, revocation, disabled source)","409"=>"Conditional write, sequence or idempotency conflict","422"=>"Invalid request or event", "503"=>"Stopping or bounded receipt capacity exhausted",_=>"Persistence failed"}});
        }
        if method == "put" {
            operation["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"type":"object","additionalProperties":false,"required":["calendar_id","event"],"properties":{"calendar_id":{"type":"string"},"event":{"type":"object","description":"EventV1: schemas/event-v1.schema.json; path UID must equal external_uid"}}}}}});
        }
        operations.insert(method.into(), operation);
    }
    document["paths"]["/api/ingest/v1/sources/{source_id}/events/{uid}"] =
        serde_json::Value::Object(operations);
    document["components"]["securitySchemes"] =
        json!({"IngestBearer":{"type":"http","scheme":"bearer"}});
}
