//! Fixed rule and Bark resources, sharing the existing browser guard and conditional writes.
use crate::{
    admin::{protect, AdminState},
    source_configuration::{condition, error, single},
};
use axum::{
    extract::{DefaultBodyLimit, Extension, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use calendar_adapters::admin_auth::AdminIdentity;
use calendar_application::configuration::*;
use serde_json::{json, Value};
use std::sync::Arc;
#[derive(Clone)]
struct Resource {
    admin: AdminState,
    output: bool,
}
pub(crate) fn router(admin: AdminState) -> Router {
    let mut all = Router::new();
    for (prefix, output) in [("rules", false), ("outputs", true)] {
        let resource = Resource {
            admin: admin.clone(),
            output,
        };
        let routes = Router::new()
            .route(&format!("/api/admin/v1/{prefix}"), get(list))
            .route(
                &format!("/api/admin/v1/{prefix}/{{id}}"),
                get(read).put(write).delete(retire),
            )
            .with_state(resource)
            .layer(DefaultBodyLimit::max(65_536));
        all = all.merge(protect(admin.clone(), routes));
    }
    all
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    after: Option<String>,
}
async fn list(
    State(s): State<Resource>,
    Query(q): Query<Cursor>,
) -> Result<Json<ReminderConfigurationPage>, StatusCode> {
    s.admin
        .services
        .run_task(async |db| {
            db.list_reminder_configurations(q.after.as_deref(), s.output)
                .await
        })
        .await
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
        .map(Json)
        .map_err(error)
}
fn receipt(value: Value, generation: i64, created: bool) -> Response {
    let mut response = (
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(value),
    )
        .into_response();
    response.headers_mut().insert(
        "etag",
        HeaderValue::from_str(&format!("\"{generation}\"")).expect("integer ETag"),
    );
    response
}
async fn read(State(s): State<Resource>, Path(id): Path<String>) -> Result<Response, StatusCode> {
    let (value, generation) = s
        .admin
        .services
        .run_task(async |db| {
            if s.output {
                db.bark_output_configuration(&id).await.and_then(|r| {
                    Ok((
                        serde_json::to_value(&r).map_err(|_| ConfigurationError::Storage)?,
                        r.generation,
                    ))
                })
            } else {
                db.rule_configuration(&id).await.and_then(|r| {
                    Ok((
                        serde_json::to_value(&r).map_err(|_| ConfigurationError::Storage)?,
                        r.generation,
                    ))
                })
            }
        })
        .await
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(error)?;
    Ok(receipt(value, generation, false))
}
async fn write(
    State(s): State<Resource>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Extension(actor): Extension<Arc<AdminIdentity>>,
    body: axum::body::Bytes,
) -> Result<Response, StatusCode> {
    if !single(&headers, "content-type")
        .and_then(|s| s.split(';').next())
        .is_some_and(|s| s.trim().eq_ignore_ascii_case("application/json"))
    {
        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let condition = condition(&headers).ok_or(StatusCode::UNPROCESSABLE_ENTITY)?;
    let now = (s.admin.clock)()
        .checked_mul(1000)
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let (value, generation) = s
        .admin
        .services
        .run_task(async |db| {
            if s.output {
                let config =
                    serde_json::from_slice(&body).map_err(|_| ConfigurationError::Invalid)?;
                let r = db
                    .put_bark_output_configuration(&id, condition, config, actor.binding(), now)
                    .await?;
                Ok((
                    serde_json::to_value(&r).map_err(|_| ConfigurationError::Storage)?,
                    r.generation,
                ))
            } else {
                let config =
                    serde_json::from_slice(&body).map_err(|_| ConfigurationError::Invalid)?;
                let r = db
                    .put_rule_configuration(&id, condition, config, actor.binding(), now)
                    .await?;
                Ok((
                    serde_json::to_value(&r).map_err(|_| ConfigurationError::Storage)?,
                    r.generation,
                ))
            }
        })
        .await
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(error)?;
    Ok(receipt(
        value,
        generation,
        condition == ConfigurationCondition::Absent,
    ))
}
async fn retire(
    State(s): State<Resource>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Extension(actor): Extension<Arc<AdminIdentity>>,
    body: axum::body::Bytes,
) -> Result<StatusCode, StatusCode> {
    if !body.is_empty() {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    let Some(ConfigurationCondition::Revision(generation)) = condition(&headers) else {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    };
    let now = (s.admin.clock)()
        .checked_mul(1000)
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    s.admin
        .services
        .run_task(async |db| {
            if s.output {
                db.retire_bark_output_configuration(&id, generation, actor.binding(), now)
                    .await
            } else {
                db.retire_rule_configuration(&id, generation, actor.binding(), now)
                    .await
            }
        })
        .await
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(error)?;
    Ok(StatusCode::NO_CONTENT)
}
pub(crate) fn describe(doc: &mut Value) {
    let rules: Value = serde_json::from_str(include_str!(
        "../../../schemas/rule-configuration-v1.schema.json"
    ))
    .expect("embedded rule schema");
    let outputs: Value = serde_json::from_str(include_str!(
        "../../../schemas/bark-output-configuration-v1.schema.json"
    ))
    .expect("embedded output schema");
    doc["components"]["schemas"]["RulePolicy"] = rules["properties"]["policy"].clone();
    doc["components"]["schemas"]["RuleConfigurationV1"] = rules;
    doc["components"]["schemas"]["BarkOutputConfigurationV1"] = outputs;
    let summary = json!({"type":"object","additionalProperties":false,"required":["id","name","enabled","generation","retired_at"],"properties":{"id":{"type":"string"},"name":{"type":"string"},"enabled":{"type":"boolean"},"generation":{"type":"integer","minimum":1},"retired_at":{"type":["integer","null"]}}});
    doc["components"]["schemas"]["ReminderConfigurationSummary"] = summary;
    doc["components"]["schemas"]["ReminderConfigurationPage"] = json!({"type":"object","additionalProperties":false,"required":["items","next_after"],"properties":{"items":{"type":"array","maxItems":100,"items":{"$ref":"#/components/schemas/ReminderConfigurationSummary"}},"next_after":{"type":["string","null"]}}});
    let parameter = |name: &str, location: &str, required: bool| json!({"name":name,"in":location,"required":required,"schema":{"type":"string"}});
    for (path, id_key, config, record) in [
        (
            "rules",
            "rule_id",
            "RuleConfigurationV1",
            "RuleConfigurationRecord",
        ),
        (
            "outputs",
            "output_id",
            "BarkOutputConfigurationV1",
            "BarkOutputConfigurationRecord",
        ),
    ] {
        let mut properties = json!({"generation":{"type":"integer","minimum":1},"retired_at":{"type":["integer","null"]},"configuration":{"$ref":format!("#/components/schemas/{config}")}});
        properties[id_key] = json!({"type":"string"});
        doc["components"]["schemas"][record] = json!({"type":"object","additionalProperties":false,"required":[id_key,"generation","retired_at","configuration"],"properties":properties});
        let auth = vec![
            parameter("Cf-Access-Jwt-Assertion", "header", true),
            parameter("__Host-calrelay_admin", "cookie", true),
        ];
        let mut operations = serde_json::Map::new();
        for method in ["get", "put", "delete"] {
            let mut params = auth.clone();
            params.push(parameter("id", "path", true));
            if method != "get" {
                for name in ["Origin", "X-CSRF-Token", "X-Admin-Write-Nonce"] {
                    params.push(parameter(name, "header", true));
                }
                params.push(parameter("If-Match", "header", method == "delete"));
                if method == "put" {
                    params.push(parameter("If-None-Match", "header", false));
                }
            }
            let mut op = json!({"summary":format!("Managed {path}: conditional configuration; no secret values"),"description":"Optional trusted admin assembly. Creation requires disabled configuration and If-None-Match: *. Updates/retirement require If-Match: quoted current generation. DELETE has an empty body. A consumed management nonce rotates even on downstream failure. Credential references must already exist with kind bark; no plaintext credential writes. All responses no-store.","parameters":params,"responses":{}});
            for code in [
                "200", "201", "204", "400", "403", "404", "409", "413", "415", "422", "500", "503",
            ] {
                if (method == "get" && ["201", "204"].contains(&code))
                    || (method == "put" && code == "204")
                    || (method == "delete" && ["200", "201"].contains(&code))
                {
                    continue;
                }
                op["responses"][code] = json!({"description":match code{"200"|"201"=>"Configuration receipt","204"=>"Retired, history retained","403"=>"Management denied","404"=>"Unknown or unmanaged configuration","409"=>"Stale condition, existing, legacy or retired id","422"=>"Invalid configuration/reference/condition","413"=>"Body budget exceeded","503"=>"Stopping",_=>"Bounded failure"},"headers":{"Cache-Control":{"schema":{"const":"no-store"}},"X-Admin-Write-Nonce":{"schema":{"type":"string"}}}});
                if ["200", "201"].contains(&code) {
                    op["responses"][code]["content"] = json!({"application/json":{"schema":{"$ref":format!("#/components/schemas/{record}")}}});
                    op["responses"][code]["headers"]["ETag"] = json!({"schema":{"type":"string"}});
                }
            }
            if method == "put" {
                op["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{config}")}}}});
            }
            operations.insert(method.into(), op);
        }
        doc["paths"][format!("/api/admin/v1/{path}/{{id}}")] = Value::Object(operations);
        let mut params = auth.clone();
        params.push(parameter("after", "query", false));
        doc["paths"][format!("/api/admin/v1/{path}")] = json!({"get":{"summary":"Discover at most 100 managed summaries including retired records","parameters":params,"responses":{"200":{"description":"No-store page, binary id cursor; not a concurrent snapshot","content":{"application/json":{"schema":{"$ref":"#/components/schemas/ReminderConfigurationPage"}}}},"400":{"description":"Invalid query"},"403":{"description":"Denied"},"422":{"description":"Invalid cursor"},"500":{"description":"Persistence failed"},"503":{"description":"Stopping"}}}});
    }
}
