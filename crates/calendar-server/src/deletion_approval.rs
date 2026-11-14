//! Administrator approval of the next over-limit snapshot deletion (#186), behind the existing
//! Access + session write guard. Producers can never approve their own deletions.
use crate::{admin::AdminState, source_configuration::error};
use axum::{
    body::Bytes,
    extract::{Extension, Path, State},
    http::StatusCode,
    routing::post,
    Json, Router,
};
use calendar_adapters::admin_auth::AdminIdentity;
use calendar_application::configuration::valid_id;
use serde_json::{json, Value};
use std::sync::Arc;

const APPROVAL: &str = "/api/admin/v1/sources/{source_id}/deletion-approval";

pub(crate) fn routes() -> Router<AdminState> {
    Router::new().route(APPROVAL, post(approve))
}
async fn approve(
    State(state): State<AdminState>,
    Path(source): Path<String>,
    Extension(actor): Extension<Arc<AdminIdentity>>,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    if !valid_id(&source) || !body.is_empty() {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    let now = state.services.business_now_ms();
    let approval = state
        .services
        .run_task(async |db| {
            db.approve_source_deletions(&source, actor.binding(), now)
                .await
        })
        .await
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(error)?;
    Ok((
        StatusCode::CREATED,
        Json(
            json!({"source_id":source,"config_revision":approval.config_revision,
            "source_revision":approval.source_revision,"expires_at":approval.expires_at}),
        ),
    ))
}
pub(crate) fn describe(document: &mut Value) {
    let parameter = |name: &str, location: &str| json!({"name":name,"in":location,"required":true,"schema":{"type":"string"}});
    document["components"]["schemas"]["DeletionApproval"] = json!({"type":"object","additionalProperties":false,
        "required":["source_id","config_revision","source_revision","expires_at"],
        "properties":{"source_id":{"type":"string"},"config_revision":{"type":"integer"},
            "source_revision":{"type":"integer"},"expires_at":{"type":"integer","description":"Unix milliseconds"}}});
    document["paths"][APPROVAL] = json!({"post":{
        "summary":"Approve the next snapshot commit of a pull or push-only source to exceed the deletion limit",
        "description":"Empty body. The approval is bound to the source's current configuration and source revisions and expires after 24 hours; any commit advances the source revision, so it authorizes at most the next commit. It lets that commit delete more than 100 events or empty a non-empty source. Repeating it replaces the pending approval. Manual-static ICS uploads keep their per-request approve_deletions.",
        "parameters":[parameter("source_id","path"),parameter("Cf-Access-Jwt-Assertion","header"),
            parameter("__Host-calrelay_admin","cookie"),parameter("Origin","header"),
            parameter("X-CSRF-Token","header"),parameter("X-Admin-Write-Nonce","header")],
        "responses":{"201":{"description":"Approval recorded","content":{"application/json":{"schema":{"$ref":"#/components/schemas/DeletionApproval"}}}},
            "403":{"description":"Access/session/Origin/CSRF denied"},"404":{"description":"No managed source"},
            "409":{"description":"Source is not an enabled, unretired pull or push-only source"},
            "422":{"description":"Invalid source ID or non-empty body"},
            "500":{"description":"Storage unavailable"},"503":{"description":"Service stopping"}}
    }});
}
