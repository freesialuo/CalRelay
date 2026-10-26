//! Read-only management diagnostics behind the existing Access + session guard.
use crate::{admin::AdminState, source_configuration::error};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use calendar_application::configuration::{valid_id, SourceMode};
use serde_json::{json, Value};

pub(crate) fn routes() -> Router<AdminState> {
    Router::new().route("/api/admin/v1/sources/{source_id}/diagnostics", get(read))
}
async fn read(
    State(state): State<AdminState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    if !valid_id(&id) {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    let now = state.services.business_now_ms();
    let row = state
        .services
        .run_task(async |db| db.source_diagnostic(&id).await)
        .await
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(error)?;
    let config = &row.configuration;
    let freshness = if config.mode == SourceMode::ManualStatic
        || (config.mode == SourceMode::PushOnly && config.freshness_sla_ms.is_none())
    {
        "not_applicable"
    } else if row.last_verified_at.is_none() {
        "never_verified"
    } else if row.last_verified_at.is_some_and(|n| n > now) {
        "future_timestamp"
    } else if config.freshness_satisfied(row.last_verified_at, now) {
        "fresh"
    } else {
        "stale"
    };
    // Explicit allowlist: no event text, endpoint, credential handle, error body or capability.
    let deliveries: Vec<_> = row.deliveries.into_iter().map(|d| json!({
        "job_id":d.job_id,"rule_id":d.rule_id,"output_id":d.output_id,"state":d.state,
        "due_at":d.due_at,"next_attempt_at":d.next_attempt_at,"accepted_at":d.accepted_at,
        "last_outcome":d.last_outcome,"started_at":d.started_at,"sent_at":d.sent_at,"finished_at":d.finished_at
    })).collect();
    Ok(Json(
        json!({"source_id":id,"observed_at":now,"mode":config.mode,"enabled":config.enabled,
        "retired_at":row.retired_at,"last_verified_at":row.last_verified_at,"snapshot_revision":row.snapshot_revision,
        "freshness":freshness,"deliveries":deliveries,"more_deliveries":row.more_deliveries}),
    ))
}
pub(crate) fn describe(document: &mut Value) {
    let nullable_time = json!({"type":["integer","null"],"description":"Unix milliseconds"});
    let mut delivery = json!({"type":"object","additionalProperties":false,"properties":{
        "job_id":{"type":"string"},"rule_id":{"type":"string"},"output_id":{"type":"string"},
        "state":{"type":"string","enum":["pending","leased","accepted","retry_wait","failed_permanent","expired","superseded","cancelled"]},
        "last_outcome":{"type":["string","null"],"enum":[null,"accepted","permanent_failure","retryable","unknown","not_sent"]},
        "due_at":{"type":"integer"},"next_attempt_at":{"type":"integer"},
        "accepted_at":nullable_time,"started_at":nullable_time,"sent_at":nullable_time,"finished_at":nullable_time
    }});
    delivery["required"] = json!(delivery["properties"]
        .as_object()
        .unwrap()
        .keys()
        .collect::<Vec<_>>());
    document["components"]["schemas"]["DeliveryDiagnostic"] = delivery;
    let mut source = json!({"type":"object","additionalProperties":false,"properties":{
        "source_id":{"type":"string"},"observed_at":{"type":"integer"},
        "mode":{"type":"string","enum":["pull","manual-static","push-only"]},"enabled":{"type":"boolean"},
        "retired_at":nullable_time,"last_verified_at":nullable_time,"snapshot_revision":{"type":["integer","null"]},
        "freshness":{"type":"string","enum":["not_applicable","never_verified","future_timestamp","fresh","stale"]},
        "deliveries":{"type":"array","maxItems":20,"items":{"$ref":"#/components/schemas/DeliveryDiagnostic"}},"more_deliveries":{"type":"boolean"}
    }});
    source["required"] = json!(source["properties"]
        .as_object()
        .unwrap()
        .keys()
        .collect::<Vec<_>>());
    document["components"]["schemas"]["SourceDiagnostic"] = source;
    document["paths"]["/api/admin/v1/sources/{source_id}/diagnostics"] = json!({"get":{
        "summary":"Read source snapshot/freshness and latest 20 created jobs; sync error reasons are not persisted",
        "parameters":[{"name":"source_id","in":"path","required":true,"schema":{"type":"string"}},
            {"name":"Cf-Access-Jwt-Assertion","in":"header","required":true,"schema":{"type":"string"}},
            {"name":"__Host-calrelay_admin","in":"cookie","required":true,"schema":{"type":"string"}}],
        "responses":{"200":{"description":"Read-only snapshot; accepted is upstream receipt, not device display", "content":{"application/json":{"schema":{"$ref":"#/components/schemas/SourceDiagnostic"}}}},
            "403":{"description":"Management authentication denied"},"404":{"description":"Configured source not found"},"422":{"description":"Invalid source ID"},"500":{"description":"Storage unavailable"},"503":{"description":"Service stopping or unavailable"}}
    }});
}
