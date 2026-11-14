//! Manual refresh request for pull sources, behind the existing Access + session write guard.
use crate::{admin::AdminState, source_configuration::error};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::post,
    Json, Router,
};
use calendar_application::configuration::valid_id;
use serde_json::{json, Value};

pub(crate) fn routes() -> Router<AdminState> {
    Router::new().route("/api/admin/v1/sources/{source_id}/refresh", post(request))
}
/// Only records the request; the source scanner performs the bounded sync on its next round.
async fn request(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    if !valid_id(&id) || !body.is_empty() {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    let now = state.services.business_now_ms();
    state
        .services
        .run_task(async |db| db.request_source_refresh(&id, now).await)
        .await
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(error)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"source_id":id,"requested_at":now})),
    ))
}
pub(crate) fn describe(document: &mut Value) {
    let parameter = |name: &str, location: &str| json!({"name":name,"in":location,"required":true,"schema":{"type":"string"}});
    document["components"]["schemas"]["SourceRefreshReceipt"] = json!({"type":"object","additionalProperties":false,
        "required":["source_id","requested_at"],
        "properties":{"source_id":{"type":"string"},"requested_at":{"type":"integer","description":"Unix milliseconds"}}});
    document["paths"]["/api/admin/v1/sources/{source_id}/refresh"] = json!({"post":{
        "summary":"Request one manual refresh of an active pull source; repeats merge while pending",
        "description":"Empty body. The scanner syncs on its next round, skipping the configured period and local failure backoff but never an upstream Retry-After or the local floor between requests. 202 does not mean the sync ran or succeeded.",
        "parameters":[parameter("source_id","path"),parameter("Cf-Access-Jwt-Assertion","header"),
            parameter("__Host-calrelay_admin","cookie"),parameter("Origin","header"),
            parameter("X-CSRF-Token","header"),parameter("X-Admin-Write-Nonce","header")],
        "responses":{"202":{"description":"Request recorded","content":{"application/json":{"schema":{"$ref":"#/components/schemas/SourceRefreshReceipt"}}}},
            "403":{"description":"Access/session/Origin/CSRF denied"},"404":{"description":"No managed source"},
            "409":{"description":"Source is not an enabled, unretired pull source"},
            "422":{"description":"Invalid source ID or non-empty body"},
            "500":{"description":"Storage unavailable"},"503":{"description":"Service stopping"}}
    }});
}
