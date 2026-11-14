//! Producer token management for push-only sources (#178), behind the existing Access + session
//! write guard. The secret is returned once on issue and never logged, listed or re-derived.
use crate::{admin::AdminState, source_configuration::error};
use axum::{
    body::Bytes,
    extract::{Extension, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use calendar_adapters::admin_auth::AdminIdentity;
use calendar_application::configuration::valid_id;
use calendar_storage::producer_tokens::ProducerToken;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

const TOKENS: &str = "/api/admin/v1/sources/{source_id}/tokens";
const REVOKE: &str = "/api/admin/v1/sources/{source_id}/tokens/{token_id}/revoke";

pub(crate) fn routes() -> Router<AdminState> {
    Router::new()
        .route(TOKENS, get(list).post(issue))
        .route(REVOKE, post(revoke))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IssueRequest {
    can_upsert: bool,
    can_delete: bool,
    expires_at: Option<i64>,
}
fn token(t: &ProducerToken) -> Value {
    json!({"token_id":t.token_id,"can_upsert":t.can_upsert,"can_delete":t.can_delete,
        "expires_at":t.expires_at,"revoked_at":t.revoked_at,"issued_at":t.issued_at})
}
async fn list(
    State(state): State<AdminState>,
    Path(source): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    if !valid_id(&source) {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    let page = state
        .services
        .run_task(async |db| db.list_producer_tokens(&source).await)
        .await
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(error)?;
    Ok(Json(
        json!({"items":page.items.iter().map(token).collect::<Vec<_>>(),"more":page.more}),
    ))
}
async fn issue(
    State(state): State<AdminState>,
    Path(source): Path<String>,
    Extension(actor): Extension<Arc<AdminIdentity>>,
    body: Bytes,
) -> Result<Response, StatusCode> {
    let request: IssueRequest =
        serde_json::from_slice(&body).map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;
    if !valid_id(&source) {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    let now = state.services.business_now_ms();
    let issued = state
        .services
        .run_task(async |db| {
            db.issue_producer_token(
                &source,
                request.can_upsert,
                request.can_delete,
                request.expires_at,
                actor.binding(),
                now,
            )
            .await
        })
        .await
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(error)?;
    let secret = std::str::from_utf8(issued.secret.expose())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"token":token(&issued.token),"secret":secret})),
    )
        .into_response())
}
async fn revoke(
    State(state): State<AdminState>,
    Path((source, token_id)): Path<(String, String)>,
    Extension(actor): Extension<Arc<AdminIdentity>>,
    body: Bytes,
) -> Result<StatusCode, StatusCode> {
    if !valid_id(&source) || !valid_id(&token_id) || !body.is_empty() {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    let now = state.services.business_now_ms();
    state
        .services
        .run_task(async |db| {
            db.revoke_producer_token(&source, &token_id, actor.binding(), now)
                .await
        })
        .await
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(error)?;
    Ok(StatusCode::NO_CONTENT)
}
pub(crate) fn describe(document: &mut Value) {
    let parameter = |name: &str, location: &str| json!({"name":name,"in":location,"required":true,"schema":{"type":"string"}});
    let read = || {
        vec![
            parameter("source_id", "path"),
            parameter("Cf-Access-Jwt-Assertion", "header"),
            parameter("__Host-calrelay_admin", "cookie"),
        ]
    };
    let write = |extra: Option<Value>| {
        let mut list = read();
        list.extend(extra);
        list.extend([
            parameter("Origin", "header"),
            parameter("X-CSRF-Token", "header"),
            parameter("X-Admin-Write-Nonce", "header"),
        ]);
        list
    };
    let nullable_time = json!({"type":["integer","null"],"description":"Unix milliseconds"});
    let schemas = &mut document["components"]["schemas"];
    schemas["ProducerToken"] = json!({"type":"object","additionalProperties":false,
        "required":["token_id","can_upsert","can_delete","expires_at","revoked_at","issued_at"],
        "properties":{"token_id":{"type":"string"},"can_upsert":{"type":"boolean"},"can_delete":{"type":"boolean"},
            "expires_at":nullable_time,"revoked_at":nullable_time,
            "issued_at":{"type":["integer","null"],"description":"Unix milliseconds; null for tokens issued before management existed"}}});
    schemas["ProducerTokenPage"] = json!({"type":"object","additionalProperties":false,"required":["items","more"],
        "properties":{"items":{"type":"array","maxItems":100,"items":{"$ref":"#/components/schemas/ProducerToken"}},"more":{"type":"boolean"}}});
    schemas["ProducerTokenIssueRequest"] = json!({"type":"object","additionalProperties":false,
        "required":["can_upsert","can_delete","expires_at"],
        "properties":{"can_upsert":{"type":"boolean"},"can_delete":{"type":"boolean"},
            "expires_at":{"type":["integer","null"],"description":"Unix milliseconds in the future, or null for no expiry"}}});
    schemas["ProducerTokenGrant"] = json!({"type":"object","additionalProperties":false,"required":["token","secret"],
        "properties":{"token":{"$ref":"#/components/schemas/ProducerToken"},
            "secret":{"type":"string","readOnly":true,"description":"Bearer secret for the ingest entry points. Returned only here; never stored, listed or logged."}}});
    let common = |ok: Value| {
        let mut responses = json!({"403":{"description":"Access/session/Origin/CSRF denied"},
            "404":{"description":"No managed source, or no such token of this source"},
            "422":{"description":"Invalid identifier or body"},
            "500":{"description":"Storage unavailable"},"503":{"description":"Service stopping"}});
        for (code, value) in ok.as_object().unwrap() {
            responses[code] = value.clone();
        }
        responses
    };
    document["paths"][TOKENS] = json!({
        "get":{"summary":"List producer tokens of a source, newest first, without secrets",
            "parameters":read(),
            "responses":common(json!({"200":{"description":"Token metadata","content":{"application/json":{"schema":{"$ref":"#/components/schemas/ProducerTokenPage"}}}}}))},
        "post":{"summary":"Issue a source-scoped producer token for a push-only standard-calendar source",
            "description":"At least one permission. Snapshot batches need both upsert and delete. At most 16 unrevoked, unexpired tokens per source. The secret is returned once.",
            "parameters":write(None),
            "requestBody":{"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/ProducerTokenIssueRequest"}}}},
            "responses":common(json!({"201":{"description":"Token issued","content":{"application/json":{"schema":{"$ref":"#/components/schemas/ProducerTokenGrant"}}}},
                "409":{"description":"Source is retired or not push-only standard-calendar, or the live token limit is reached"}}))}
    });
    document["paths"][REVOKE] = json!({"post":{
        "summary":"Revoke a producer token; repeating it changes nothing",
        "parameters":write(Some(parameter("token_id","path"))),
        "responses":common(json!({"204":{"description":"Revoked"}}))
    }});
}
