//! Management source routes, mounted only inside the existing #34 guard.
use crate::admin::AdminState;
use axum::{
    extract::{Extension, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use calendar_adapters::admin_auth::AdminIdentity;
use calendar_application::configuration::{
    ConfigurationCondition as Condition, ConfigurationError as Error,
    SourceConfigurationRecord as Record, SourceConfigurationV1 as Configuration,
};
use std::sync::Arc;

pub(crate) fn routes() -> Router<AdminState> {
    Router::new().route(
        "/api/admin/v1/sources/{source_id}",
        get(read).put(write).delete(retire),
    )
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
fn condition(headers: &HeaderMap) -> Option<Condition> {
    match (
        single(headers, "if-match"),
        single(headers, "if-none-match"),
    ) {
        (None, Some("*")) if !headers.contains_key("if-match") => Some(Condition::Absent),
        (Some(s), None) if !headers.contains_key("if-none-match") => {
            let text = s.strip_prefix('"')?.strip_suffix('"')?;
            let n: i64 = text.parse().ok()?;
            (n > 0 && text == n.to_string()).then_some(Condition::Revision(n))
        }
        _ => None,
    }
}
fn error(error: Error) -> StatusCode {
    match error {
        Error::Invalid => StatusCode::UNPROCESSABLE_ENTITY,
        Error::NotFound => StatusCode::NOT_FOUND,
        Error::Conflict => StatusCode::CONFLICT,
        Error::Storage => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
fn receipt(record: Record, created: bool) -> Response {
    let tag = format!("\"{}\"", record.config_revision);
    let mut response = (
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(record),
    )
        .into_response();
    response
        .headers_mut()
        .insert("etag", HeaderValue::from_str(&tag).expect("integer ETag"));
    response
}
async fn read(
    State(state): State<AdminState>,
    Path(id): Path<String>,
) -> Result<Response, StatusCode> {
    let record = state
        .services
        .run_task(async |db| db.source_configuration(&id).await)
        .await
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(error)?;
    Ok(receipt(record, false))
}
async fn write(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Extension(actor): Extension<Arc<AdminIdentity>>,
    Json(config): Json<Configuration>,
) -> Result<Response, StatusCode> {
    let condition = condition(&headers).ok_or(StatusCode::UNPROCESSABLE_ENTITY)?;
    let now = (state.clock)()
        .checked_mul(1000)
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let record = state
        .services
        .run_task(async |db| {
            db.put_source_configuration(&id, condition, config, actor.binding(), now)
                .await
        })
        .await
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(error)?;
    Ok(receipt(record, condition == Condition::Absent))
}
async fn retire(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Extension(actor): Extension<Arc<AdminIdentity>>,
    body: axum::body::Bytes,
) -> Result<StatusCode, StatusCode> {
    if !body.is_empty() {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    let Some(Condition::Revision(revision)) = condition(&headers) else {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    };
    let now = (state.clock)()
        .checked_mul(1000)
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .services
        .run_task(async |db| {
            db.retire_source_configuration(&id, revision, actor.binding(), now)
                .await
        })
        .await
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(error)?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) fn describe(document: &mut serde_json::Value) {
    use serde_json::json;
    let config: serde_json::Value = serde_json::from_str(include_str!(
        "../../../schemas/source-configuration-v1.schema.json"
    ))
    .expect("embedded source schema");
    document["components"]["schemas"]["SourceConfigurationV1"] = config;
    document["components"]["schemas"]["SourceConfigurationRecord"] = json!({"type":"object","additionalProperties":false,"required":["source_id","config_revision","source_revision","retired_at","configuration"],"properties":{"source_id":{"type":"string"},"config_revision":{"type":"integer","minimum":1},"source_revision":{"type":"integer","minimum":1},"retired_at":{"type":["integer","null"],"description":"Unix milliseconds"},"configuration":{"$ref":"#/components/schemas/SourceConfigurationV1"}}});
    let parameter = |name: &str, location: &str, required: bool| json!({"name":name,"in":location,"required":required,"schema":{"type":"string"}});
    let mut ops = serde_json::Map::new();
    for method in ["get", "put", "delete"] {
        let mut parameters = vec![
            parameter("source_id", "path", true),
            parameter("Cf-Access-Jwt-Assertion", "header", true),
            parameter("__Host-calrelay_admin", "cookie", true),
        ];
        if method != "get" {
            for name in ["Origin", "X-CSRF-Token", "X-Admin-Write-Nonce"] {
                parameters.push(parameter(name, "header", true));
            }
            parameters.push(parameter("If-Match", "header", method == "delete"));
            if method == "put" {
                parameters.push(parameter("If-None-Match", "header", false));
            }
        }
        let mut op = json!({"summary":"Read, conditionally save, or retire source configuration (optional admin assembly)","description":"GET/PUT ETag is config_revision, never event/source revision. PUT requires exactly one of If-None-Match: * (disabled creation) or If-Match: quoted positive config revision. DELETE requires If-Match and empty body. No secret material accepted. All responses no-store. Authorized unsafe requests consume and rotate the existing #34 nonce even on downstream failure; read X-Admin-Write-Nonce before retry.","parameters":parameters,"responses":{}});
        for code in [
            "200", "201", "204", "403", "404", "409", "413", "422", "500", "503",
        ] {
            if (method == "get" && ["201", "204"].contains(&code))
                || (method == "put" && code == "204")
                || (method == "delete" && ["200", "201"].contains(&code))
            {
                continue;
            }
            op["responses"][code] = json!({"description":match code{"200"|"201"=>"Committed configuration receipt","204"=>"Source retired; history retained","403"=>"Access/session/Origin/CSRF denied","404"=>"No managed configuration; legacy sources are not reinterpreted","409"=>"Stale condition, identity conflict, existing or retired source","422"=>"Invalid configuration or condition","413"=>"Request budget exceeded","503"=>"Service stopping",_=>"Persistence failed"},"headers":{"Cache-Control":{"schema":{"const":"no-store"}},"X-Admin-Write-Nonce":{"schema":{"type":"string"},"description":"Rotated after an authorized unsafe request"}}});
            if ["200", "201"].contains(&code) {
                op["responses"][code]["headers"]["ETag"] = json!({"schema":{"type":"string"}});
                op["responses"][code]["content"] = json!({"application/json":{"schema":{"$ref":"#/components/schemas/SourceConfigurationRecord"}}});
            }
        }
        if method == "put" {
            op["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/SourceConfigurationV1"}}}});
        }
        ops.insert(method.into(), op);
    }
    document["paths"]["/api/admin/v1/sources/{source_id}"] = serde_json::Value::Object(ops);
}
