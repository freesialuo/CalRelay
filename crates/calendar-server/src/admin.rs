//! Browser management routes. Never mounted onto machine ingest or feed routers.
use crate::Services;
use axum::{
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use calendar_adapters::admin_auth::{AdminIdentity, CloudflareVerifier};
use calendar_application::SecretBytes;
use serde::Deserialize;
use serde_json::json;
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

const COOKIE: &str = "__Host-calrelay_admin";
#[derive(Clone)]
pub struct AdminState {
    services: Arc<Services>,
    verifier: Arc<CloudflareVerifier>,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
}
impl AdminState {
    pub fn new(services: Arc<Services>, verifier: Arc<CloudflareVerifier>) -> Self {
        Self::with_clock(
            services,
            verifier,
            Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(-1)
            }),
        )
    }
    /// Trusted assembly/test clock, returning Unix seconds. Never take time from a request.
    pub fn with_clock(
        services: Arc<Services>,
        verifier: Arc<CloudflareVerifier>,
        clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    ) -> Self {
        Self {
            services,
            verifier,
            clock,
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    turnstile_token: String,
}
fn single<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, StatusCode> {
    let mut values = headers.get_all(name).iter();
    let value = values
        .next()
        .ok_or(StatusCode::FORBIDDEN)?
        .to_str()
        .map_err(|_| StatusCode::FORBIDDEN)?;
    if values.next().is_some() || value.is_empty() {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(value)
}
fn cookie(headers: &HeaderMap) -> Result<&str, StatusCode> {
    let mut found = None;
    for value in headers.get_all("cookie") {
        for part in value
            .to_str()
            .map_err(|_| StatusCode::FORBIDDEN)?
            .split(';')
        {
            if let Some((name, value)) = part.trim().split_once('=') {
                if name == COOKIE && found.replace(value).is_some() {
                    return Err(StatusCode::FORBIDDEN);
                }
            }
        }
    }
    found.ok_or(StatusCode::FORBIDDEN)
}
fn secret_text(secret: &SecretBytes) -> Result<&str, StatusCode> {
    std::str::from_utf8(secret.expose()).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}
async fn identity(
    state: &AdminState,
    headers: &HeaderMap,
    now: i64,
) -> Result<AdminIdentity, StatusCode> {
    if !state.services.ready() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    state
        .verifier
        .access(single(headers, "cf-access-jwt-assertion")?, now)
        .await
        .map_err(|_| StatusCode::FORBIDDEN)
}
fn origin(state: &AdminState, headers: &HeaderMap) -> Result<(), StatusCode> {
    if single(headers, "origin")? != state.verifier.origin() {
        return Err(StatusCode::FORBIDDEN);
    }
    if headers
        .get_all("sec-fetch-site")
        .iter()
        .any(|v| v != "same-origin" && v != "none")
    {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(())
}
async fn login(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(input): Json<Login>,
) -> Result<Response, StatusCode> {
    origin(&state, &headers)?;
    let now = (state.clock)();
    let who = identity(&state, &headers, now).await?;
    state
        .verifier
        .turnstile(&input.turnstile_token, now)
        .await
        .map_err(|_| StatusCode::FORBIDDEN)?;
    // Re-read the clock after external I/O; do not mint an already expired Access session.
    let now = (state.clock)();
    let expires = who.expires_at().min(
        now.checked_add(state.verifier.session_seconds())
            .ok_or(StatusCode::FORBIDDEN)?,
    );
    if expires <= now {
        return Err(StatusCode::FORBIDDEN);
    }
    let grant = state
        .services
        .run_task(async |db| {
            db.create_admin_session(
                who.binding(),
                state.verifier.policy(),
                &input.turnstile_token,
                now,
                expires,
            )
            .await
        })
        .await
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(|_| StatusCode::FORBIDDEN)?;
    let mut response = Json(json!({"csrf_token":secret_text(&grant.csrf)?,"write_nonce":secret_text(&grant.nonce)?,"expires_at":grant.expires_at})).into_response();
    response.headers_mut().insert(
        "set-cookie",
        HeaderValue::from_str(&format!(
            "{COOKIE}={}; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age={}",
            secret_text(&grant.session)?,
            expires - now
        ))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    );
    Ok(response)
}
/// Reusable guard for future management handlers. It validates Access and session on every
/// request; every unsafe method atomically consumes its CSRF-bound nonce before the handler.
/// Consumed nonce stays consumed on downstream failure (retry requires a fresh session).
pub fn protect(state: AdminState, routes: Router) -> Router {
    routes.layer(middleware::from_fn_with_state(state, authorize))
}
async fn authorize(
    State(state): State<AdminState>,
    mut request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let now = (state.clock)();
    let who = identity(&state, request.headers(), now).await?;
    let now = (state.clock)();
    if who.expires_at() <= now {
        return Err(StatusCode::FORBIDDEN);
    }
    let session = cookie(request.headers())?;
    let mut new_nonce = None;
    let safe = matches!(*request.method(), Method::GET | Method::HEAD);
    if safe {
        state
            .services
            .run_task(async |db| {
                db.check_admin_session(session, who.binding(), state.verifier.policy(), now)
                    .await
            })
            .await
            .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
            .map_err(|_| StatusCode::FORBIDDEN)?;
    } else {
        origin(&state, request.headers())?;
        let csrf = single(request.headers(), "x-csrf-token")?;
        let nonce = single(request.headers(), "x-admin-write-nonce")?;
        let revoke =
            request.method() == Method::DELETE && request.uri().path() == "/api/admin/v1/session";
        new_nonce = Some(
            state
                .services
                .run_task(async |db| {
                    db.consume_admin_write(
                        session,
                        who.binding(),
                        state.verifier.policy(),
                        csrf,
                        nonce,
                        now,
                        revoke,
                    )
                    .await
                })
                .await
                .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
                .map_err(|_| StatusCode::FORBIDDEN)?,
        );
    }
    request.extensions_mut().insert(Arc::new(who));
    let mut response = next.run(request).await;
    if let Some(nonce) = new_nonce {
        response.headers_mut().insert(
            "x-admin-write-nonce",
            HeaderValue::from_str(secret_text(&nonce)?)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        );
    }
    Ok(response)
}
pub fn router(state: AdminState) -> Router {
    let protected = protect(state.clone(), Router::new().route("/api/admin/v1/session",get(|| async { StatusCode::NO_CONTENT }).delete(|| async {
        (StatusCode::NO_CONTENT,[("set-cookie","__Host-calrelay_admin=; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age=0")])
    })));
    Router::new()
        .route("/api/admin/v1/session", post(login))
        .with_state(state)
        .merge(protected)
        .layer(DefaultBodyLimit::max(4096))
        .layer(middleware::from_fn(
            |request: Request, next: Next| async move {
                let mut response = next.run(request).await;
                response
                    .headers_mut()
                    .insert("cache-control", HeaderValue::from_static("no-store"));
                response
            },
        ))
}

/// Describes the optional management API; routes remain absent until explicit assembly.
pub fn openapi_paths() -> serde_json::Map<String, serde_json::Value> {
    let mut operations = serde_json::Map::new();
    let jwt = json!({"name":"Cf-Access-Jwt-Assertion","in":"header","required":true,"schema":{"type":"string"},"description":"RS256 Access application JWT for an explicitly allowed administrator"});
    let origin = json!({"name":"Origin","in":"header","required":true,"schema":{"type":"string"},"description":"Exact configured HTTPS origin"});
    let session = json!({"name":"__Host-calrelay_admin","in":"cookie","required":true,"schema":{"type":"string"}});
    let denied = json!({"description":"Denied, unavailable, expired, revoked or replayed; no secret diagnostics"});
    operations.insert("post".into(),json!({"summary":"Create an identity-bound browser session (optional trusted assembly)","parameters":[jwt,origin],"requestBody":{"required":true,"content":{"application/json":{"schema":{"type":"object","additionalProperties":false,"required":["turnstile_token"],"properties":{"turnstile_token":{"type":"string","minLength":1,"maxLength":2048}}}}}},"responses":{"200":{"description":"Secure HttpOnly SameSite=Strict cookie; body tokens must stay in browser memory","headers":{"Set-Cookie":{"schema":{"type":"string"}},"Cache-Control":{"schema":{"const":"no-store"}}},"content":{"application/json":{"schema":{"type":"object","required":["csrf_token","write_nonce","expires_at"],"properties":{"csrf_token":{"type":"string"},"write_nonce":{"type":"string"},"expires_at":{"type":"integer","description":"Unix seconds"}}}}}},"403":denied,"503":denied}}));
    operations.insert("get".into(),json!({"summary":"Check current Access identity and application session","parameters":[jwt,session],"responses":{"204":{"description":"Authorized"},"403":denied,"503":denied}}));
    operations.insert("delete".into(),json!({"summary":"Atomically revoke session and consume write nonce","parameters":[jwt,origin,session,{"name":"X-CSRF-Token","in":"header","required":true,"schema":{"type":"string"}},{"name":"X-Admin-Write-Nonce","in":"header","required":true,"schema":{"type":"string"}}],"responses":{"204":{"description":"Revoked; cookie cleared"},"403":denied,"503":denied}}));
    [(
        "/api/admin/v1/session".into(),
        serde_json::Value::Object(operations),
    )]
    .into_iter()
    .collect()
}
