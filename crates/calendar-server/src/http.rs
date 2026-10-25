use crate::{Config, Services};
use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

#[derive(Clone, Debug)]
pub struct RequestId(pub String);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn router(services: Arc<Services>, config: Config) -> Router {
    router_with_admin(services, config, None)
}

pub fn router_with_admin(
    services: Arc<Services>,
    config: Config,
    admin: Option<crate::admin::AdminState>,
) -> Router {
    let bootstrap = crate::admin::bootstrap(admin.as_ref());
    #[cfg(feature = "production-web")]
    let assets = crate::web::router();
    #[cfg(not(feature = "production-web"))]
    let assets = Router::new();
    let management = admin.map(crate::admin::router).unwrap_or_default();
    Router::new()
        .route("/api/admin/v1/bootstrap", get(move || { let value=bootstrap.clone(); async move { ([ ("cache-control","no-store") ],Json(value)) } }))
        .merge(crate::ingest::router())
        .route("/health/live", get(|| async { Json(json!({"status":"live"})) }))
        .route("/health/ready", get(|State(s): State<Arc<Services>>| async move {
            if s.ready() { (StatusCode::OK, Json(json!({"status":"ready"}))).into_response() } else { StatusCode::SERVICE_UNAVAILABLE.into_response() }
        }))
        .route("/api/v1/version", get(|| async { Json(json!({"name":"CalRelay", "version":env!("CARGO_PKG_VERSION"), "api_version":"v1"})) }))
        .route("/api/v1/openapi.json", get(|| async { Json(openapi()) }))
        .fallback(|| async { StatusCode::NOT_FOUND })
        .with_state(services.clone())
        .merge(crate::feed::router(services.clone(), config.max_response_bytes))
        .merge(crate::aggregate_feed::router(services, config.max_response_bytes))
        .merge(management)
        .merge(assets)
        .layer(middleware::from_fn_with_state(config, boundary))
}

/// Apply this boundary after all future routes are mounted, so fallbacks and rejections
/// have the same limits, identifiers and error envelope as handler responses.
async fn boundary(State(config): State<Config>, request: Request, next: Next) -> Response {
    let id = RequestId(format!(
        "{:x}-{:x}-{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let operation = async {
        let (mut parts, body) = request.into_parts();
        parts.extensions.insert(id.clone());
        let bytes = match to_bytes(body, config.max_request_bytes).await {
            Ok(bytes) => bytes,
            Err(_) => return error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large", &id),
        };
        let response = next
            .run(Request::from_parts(parts, Body::from(bytes)))
            .await;
        let status = response.status();
        if status.is_client_error() || status.is_server_error() {
            let (mut parts, _) = response.into_parts();
            // Keep protocol headers (Allow, WWW-Authenticate, Retry-After, etc.)
            // while invalidating metadata that described the replaced representation.
            for header in [
                "content-type",
                "content-length",
                "content-encoding",
                "transfer-encoding",
                "content-range",
                "etag",
                "last-modified",
                "content-md5",
                "digest",
            ] {
                parts.headers.remove(header);
            }
            let (envelope, body) = error(status, error_code(status), &id).into_parts();
            parts.headers.extend(envelope.headers);
            return Response::from_parts(parts, body);
        }
        let (parts, body) = response.into_parts();
        match to_bytes(body, config.max_response_bytes).await {
            Ok(bytes) => Response::from_parts(parts, Body::from(bytes)),
            Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "response_too_large", &id),
        }
    };
    let mut response = tokio::time::timeout(config.request_timeout, operation)
        .await
        .unwrap_or_else(|_| error(StatusCode::REQUEST_TIMEOUT, "request_timeout", &id));
    response.headers_mut().insert(
        "x-request-id",
        HeaderValue::from_str(&id.0).expect("generated ASCII id"),
    );
    response
}
fn error_code(status: StatusCode) -> &'static str {
    match status {
        StatusCode::NOT_FOUND => "not_found",
        StatusCode::METHOD_NOT_ALLOWED => "method_not_allowed",
        StatusCode::SERVICE_UNAVAILABLE => "not_ready",
        StatusCode::BAD_REQUEST => "bad_request",
        StatusCode::UNAUTHORIZED => "unauthorized",
        StatusCode::FORBIDDEN => "forbidden",
        StatusCode::CONFLICT => "conflict",
        StatusCode::UNPROCESSABLE_ENTITY => "validation_failed",
        _ => "request_failed",
    }
}
fn error(status: StatusCode, code: &'static str, id: &RequestId) -> Response {
    (
        status,
        Json(json!({"error":{"code":code,"request_id":id.0}})),
    )
        .into_response()
}

pub fn openapi() -> Value {
    let error = json!({"description":"Bounded error; request_id matches X-Request-Id", "headers":{"X-Request-Id":{"schema":{"type":"string"}}}, "content":{"application/json":{"schema":{"$ref":"#/components/schemas/Error"}}}});
    let mut paths = serde_json::Map::new();
    for (path, schema) in [
        (
            "/health/live",
            json!({"type":"object","required":["status"],"properties":{"status":{"type":"string","enum":["live"]}}}),
        ),
        (
            "/health/ready",
            json!({"type":"object","required":["status"],"properties":{"status":{"type":"string","enum":["ready"]}}}),
        ),
        (
            "/api/v1/version",
            json!({"type":"object","required":["name","version","api_version"],"properties":{"name":{"type":"string"},"version":{"type":"string"},"api_version":{"type":"string","enum":["v1"]}}}),
        ),
        ("/api/v1/openapi.json", json!({"type":"object"})),
    ] {
        let mut responses = serde_json::Map::new();
        responses.insert("200".into(), json!({"description":"Success", "headers":{"X-Request-Id":{"schema":{"type":"string"}}}, "content":{"application/json":{"schema":schema}}}));
        for code in ["400", "404", "405", "408", "413", "500", "503"] {
            responses.insert(code.into(), error.clone());
        }
        paths.insert(path.into(), json!({"get":{"responses":responses}}));
    }
    paths.insert("/api/admin/v1/bootstrap".into(), json!({"get":{"summary":"Public challenge metadata; never grants authority","responses":{"200":{"description":"Disabled or public Turnstile metadata only","headers":{"Cache-Control":{"schema":{"const":"no-store"}}},"content":{"application/json":{"schema":{"oneOf":[{"type":"object","additionalProperties":false,"required":["enabled"],"properties":{"enabled":{"const":false}}},{"type":"object","additionalProperties":false,"required":["enabled","turnstile"],"properties":{"enabled":{"const":true},"turnstile":{"type":"object","additionalProperties":false,"required":["site_key","hostname","action"],"properties":{"site_key":{"type":"string"},"hostname":{"type":"string"},"action":{"type":"string"}}}}}]}}}}}}}));
    paths.extend(crate::admin::openapi_paths());
    let mut document = json!({"openapi":"3.1.0","info":{"title":"CalRelay service API","version":env!("CARGO_PKG_VERSION")},
        "paths":paths,"components":{"schemas":{"Error":{"type":"object","additionalProperties":false,"required":["error"],"properties":{"error":{"type":"object","additionalProperties":false,"required":["code","request_id"],"properties":{"code":{"type":"string"},"request_id":{"type":"string"}}}}}}}});
    crate::ingest::describe(&mut document);
    crate::feed::describe(&mut document);
    crate::aggregate_feed::describe(&mut document);
    crate::aggregate_configuration::describe(&mut document);
    crate::source_configuration::describe(&mut document);
    crate::reminder_configuration::describe(&mut document);
    document
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tower::ServiceExt;
    fn config() -> Config {
        Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            database: "unused.db".into(),
            max_request_bytes: 8,
            max_response_bytes: 512,
            request_timeout: Duration::from_millis(50),
        }
    }
    #[tokio::test]
    async fn error_envelopes_preserve_protocol_headers_and_replace_body_metadata() {
        let app = Router::new()
            .route(
                "/auth",
                get(|| async {
                    (
                        StatusCode::UNAUTHORIZED,
                        [
                            ("www-authenticate", "Bearer"),
                            ("retry-after", "30"),
                            ("content-type", "text/plain"),
                            ("content-length", "3"),
                            ("etag", "old"),
                        ],
                        "old",
                    )
                }),
            )
            .layer(middleware::from_fn_with_state(config(), boundary));
        let response = app
            .oneshot(Request::builder().uri("/auth").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers().get("www-authenticate").unwrap(),
            "Bearer"
        );
        assert_eq!(response.headers().get("retry-after").unwrap(), "30");
        assert_eq!(response.headers()["content-type"], "application/json");
        assert!(!response.headers().contains_key("etag"));
        assert_ne!(
            response
                .headers()
                .get("content-length")
                .and_then(|v| v.to_str().ok()),
            Some("3")
        );
        let body = to_bytes(response.into_body(), 512).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["error"]["code"],
            "unauthorized"
        );
    }

    #[tokio::test]
    async fn response_limit_and_timeout_are_atomic_error_envelopes() {
        let app = Router::new()
            .route("/large", get(|| async { "x".repeat(513) }))
            .route("/exact", get(|| async { "x".repeat(512) }))
            .route(
                "/slow",
                get(|| async { std::future::pending::<String>().await }),
            )
            .layer(middleware::from_fn_with_state(config(), boundary));
        for (path, expected, code) in [
            ("/large", 500, "response_too_large"),
            ("/slow", 408, "request_timeout"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header("x-request-id", "untrusted")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), expected);
            let id = response.headers()["x-request-id"]
                .to_str()
                .unwrap()
                .to_owned();
            assert_ne!(id, "untrusted");
            let bytes = to_bytes(response.into_body(), 512).await.unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&bytes).unwrap(),
                json!({"error":{"code":code,"request_id":id}})
            );
        }
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/exact")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(response.into_body(), 512).await.unwrap().len(),
            512
        );
    }
}
