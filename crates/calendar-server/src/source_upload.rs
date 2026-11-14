//! ICS upload replacing a manual-static source snapshot, behind the existing write guard.
use crate::{
    admin::AdminState,
    source_configuration::{condition, single},
    source_worker::Pinned,
};
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::put,
    Json, Router,
};
use calendar_adapters::ingestion::IcsUpload;
use calendar_application::{
    configuration::{
        valid_id, AdapterKind, ConfigurationCondition as Condition, ConfigurationError, SourceMode,
    },
    synchronize, DeletionPolicy, IngestionError, SyncLimits, SyncOutcome,
};
use serde_json::{json, Value};

pub(crate) fn routes() -> Router<AdminState> {
    Router::new().route("/api/admin/v1/sources/{source_id}/snapshot", put(upload))
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Approval {
    /// Explicit administrator approval to delete more existing events than the default limit.
    #[serde(default)]
    approve_deletions: bool,
}
/// The document is normalized and committed as one complete snapshot or rejected as a whole.
async fn upload(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    Query(approval): Query<Approval>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<Value>, StatusCode> {
    if !valid_id(&id) {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    let media = single(&headers, "content-type")
        .and_then(|value| value.split(';').next())
        .map(str::trim);
    if !media.is_some_and(|media| media.eq_ignore_ascii_case("text/calendar")) {
        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let Some(Condition::Revision(revision)) = condition(&headers) else {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    };
    let text = std::str::from_utf8(&body).map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;
    let now = state.services.business_now_ms();
    let limits = state.services.ics_limits;
    let committed = state
        .services
        .run_task(async |db| {
            let record = db
                .source_configuration(&id)
                .await
                .map_err(|error| match error {
                    ConfigurationError::NotFound => StatusCode::NOT_FOUND,
                    _ => StatusCode::INTERNAL_SERVER_ERROR,
                })?;
            let config = &record.configuration;
            if record.retired_at.is_some()
                || !config.enabled
                || config.mode != SourceMode::ManualStatic
                || config.adapter != AdapterKind::Ics
                || config.calendars.len() != 1
                || record.config_revision != revision
            {
                return Err(StatusCode::CONFLICT);
            }
            let adapter = Pinned {
                revision: record.source_revision,
                conditional: false,
                adapter: IcsUpload {
                    text,
                    calendar_id: config.calendars[0].id.clone(),
                    limits,
                },
            };
            let policy = if approval.approve_deletions {
                DeletionPolicy::Approved
            } else {
                DeletionPolicy::Limit(100)
            };
            synchronize(db, &adapter, &id, now, policy, SyncLimits::default())
                .await
                .map_err(|error| match error {
                    IngestionError::Adapter | IngestionError::InvalidSnapshot => {
                        StatusCode::UNPROCESSABLE_ENTITY
                    }
                    IngestionError::ReviewRequired { .. } => StatusCode::PRECONDITION_REQUIRED,
                    IngestionError::Budget => StatusCode::PAYLOAD_TOO_LARGE,
                    IngestionError::Conflict | IngestionError::Unavailable => StatusCode::CONFLICT,
                    IngestionError::Timeout => StatusCode::SERVICE_UNAVAILABLE,
                    _ => StatusCode::INTERNAL_SERVER_ERROR,
                })
        })
        .await
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)??;
    let SyncOutcome::Committed(committed) = committed else {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    };
    // Explicit allowlist: no event text or upstream detail is echoed.
    Ok(Json(
        json!({"source_id":id,"source_revision":committed.source_revision,
        "changed_events":committed.changed_events}),
    ))
}
pub(crate) fn describe(document: &mut Value) {
    let parameter = |name: &str, location: &str| json!({"name":name,"in":location,"required":true,"schema":{"type":"string"}});
    document["components"]["schemas"]["SourceSnapshotReceipt"] = json!({"type":"object","additionalProperties":false,
        "required":["source_id","source_revision","changed_events"],
        "properties":{"source_id":{"type":"string"},"source_revision":{"type":"integer"},"changed_events":{"type":"integer","minimum":0}}});
    document["paths"]["/api/admin/v1/sources/{source_id}/snapshot"] = json!({"put":{
        "summary":"Replace the snapshot of an enabled manual-static ICS source with one uploaded ICS document",
        "description":"If-Match is the quoted config_revision. The document is normalized to EventV1 and committed atomically as a complete snapshot or rejected as a whole; events absent from it are deleted. Deleting more than 100 existing events returns 428 and changes nothing unless the same upload is repeated with approve_deletions=true. The document itself is not stored and nothing is written to a remote calendar.",
        "parameters":[parameter("source_id","path"),
            {"name":"approve_deletions","in":"query","required":false,"schema":{"type":"boolean","default":false},"description":"Explicit approval to delete more than 100 existing events"},
            parameter("If-Match","header"),
            parameter("Cf-Access-Jwt-Assertion","header"),parameter("__Host-calrelay_admin","cookie"),
            parameter("Origin","header"),parameter("X-CSRF-Token","header"),parameter("X-Admin-Write-Nonce","header")],
        "requestBody":{"required":true,"content":{"text/calendar":{"schema":{"type":"string"}}}},
        "responses":{"200":{"description":"Snapshot committed","content":{"application/json":{"schema":{"$ref":"#/components/schemas/SourceSnapshotReceipt"}}}},
            "403":{"description":"Access/session/Origin/CSRF denied"},"404":{"description":"No managed source"},
            "409":{"description":"Not an enabled, unretired, single-calendar manual-static ICS source, stale config_revision, or concurrent source change"},
            "413":{"description":"Document exceeds the ICS byte or event budget"},
            "415":{"description":"Content-Type is not text/calendar"},
            "422":{"description":"Invalid source ID, missing If-Match, or invalid ICS"},
            "428":{"description":"More than 100 existing events would be deleted; nothing changed, approval required"},
            "500":{"description":"Storage unavailable"},"503":{"description":"Service stopping or sync deadline exceeded"}}
    }});
}
