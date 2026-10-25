//! Capability-only calendar route. Never log request URI, tokens or renderer diagnostics.
use crate::Services;
use axum::{
    extract::Request,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use calendar_adapters::renderer::{render_ics, FeedEvent};
use calendar_application::{EventV1, SecretBytes, SecurityError};
use calendar_storage::feed::{FeedKey, FeedSnapshot};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

#[derive(Clone)]
struct FeedState {
    services: Arc<Services>,
    cache: Arc<Mutex<HashMap<FeedKey, Arc<Document>>>>,
    max_bytes: usize,
}
struct Document {
    body: String,
    etag: String,
}
pub(crate) fn router(services: Arc<Services>, max_bytes: usize) -> Router {
    Router::new()
        .route("/feeds/{public_id}/{secret}", get(feed))
        .layer(middleware::from_fn(privacy_headers))
        .with_state(FeedState {
            services,
            cache: Arc::default(),
            max_bytes,
        })
}
// Protocol errors never carry the capability or become shared-cache entries.
async fn privacy_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let policy = if response.status().is_success() || response.status() == StatusCode::NOT_MODIFIED
    {
        "private, no-cache"
    } else {
        "private, no-store"
    };
    response
        .headers_mut()
        .insert("cache-control", policy.parse().unwrap());
    response
}
/// Calendar capability is carried only in the path; never inherit management security.
pub(crate) fn describe(document: &mut serde_json::Value) {
    use serde_json::json;
    let mut responses = serde_json::Map::new();
    let headers = json!({"ETag":{"schema":{"type":"string"}},"Cache-Control":{"schema":{"type":"string","enum":["private, no-cache"]}},"X-Request-Id":{"schema":{"type":"string"}}});
    responses.insert("200".into(),json!({"description":"Complete single-source ICS representation","headers":headers,"content":{"text/calendar; charset=utf-8":{"schema":{"type":"string"}}}}));
    responses.insert("304".into(),json!({"description":"Unchanged authorized representation; no response body","headers":headers}));
    for code in ["400", "404", "405", "408", "413", "422", "500", "503"] {
        responses.insert(code.into(),json!({"description":"Fixed error envelope; no calendar content or ETag","headers":{"X-Request-Id":{"schema":{"type":"string"}}},"content":{"application/json":{"schema":{"$ref":"#/components/schemas/Error"}}}}));
    }
    let get = json!({"summary":"Read a scoped single-source calendar","description":"Revocable feed capability; no management session or human challenge. The full URL is secret and must not be logged.","security":[],"parameters":[
        {"name":"public_id","in":"path","required":true,"schema":{"type":"string","minLength":1,"maxLength":256}},
        {"name":"secret","in":"path","required":true,"schema":{"type":"string","pattern":"^[0-9a-f]{64}$","writeOnly":true}},
        {"name":"If-None-Match","in":"header","required":false,"schema":{"type":"string"}}
    ],"responses":responses});
    let mut head = get.clone();
    head["summary"] = json!("Inspect an authorized calendar representation");
    for response in head["responses"]
        .as_object_mut()
        .expect("constructed object")
        .values_mut()
    {
        response
            .as_object_mut()
            .expect("constructed response")
            .remove("content");
    }
    document["paths"]["/feeds/{public_id}/{secret}.ics"] = json!({"get":get,"head":head});
}
pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}
fn denied(error: SecurityError) -> StatusCode {
    if error == SecurityError::Denied {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}
async fn feed(
    State(state): State<FeedState>,
    Path((public_id, secret)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if !state.services.ready() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let Some(secret) = secret.strip_suffix(".ics") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let secret = SecretBytes::new(secret.as_bytes().to_vec());
    let db = &state.services.storage;
    // This durable check precedes every cache lookup and If-None-Match comparison.
    let mut key = match db
        .authorize_feed(
            &public_id,
            secret.expose(),
            state.services.business_now_ms(),
        )
        .await
    {
        Ok(k) => k.key,
        Err(e) => return denied(e).into_response(),
    };
    let cached = state.cache.lock().await.get(&key).cloned();
    let document = if let Some(doc) = cached {
        doc
    } else {
        let snapshot = match db
            .feed_snapshot(
                &public_id,
                secret.expose(),
                state.services.business_now_ms(),
            )
            .await
        {
            Ok(s) => s,
            Err(e) => return denied(e).into_response(),
        };
        key = snapshot.authorization.key.clone();
        let doc = match render(snapshot, state.max_bytes) {
            Ok(doc) => Arc::new(doc),
            Err(FeedRenderError::Invalid) => {
                return StatusCode::UNPROCESSABLE_ENTITY.into_response()
            }
            Err(FeedRenderError::Budget) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        let mut cache = state.cache.lock().await;
        // Bound retained output to at most sixteen max_response_bytes documents.
        if cache.len() >= 16 {
            cache.clear();
        }
        cache.insert(key.clone(), doc.clone());
        doc
    };
    finish_feed(&state, &public_id, &secret, &key, document, headers).await
}
async fn finish_feed(
    state: &FeedState,
    public_id: &str,
    secret: &SecretBytes,
    key: &FeedKey,
    document: Arc<Document>,
    headers: HeaderMap,
) -> Response {
    let db = &state.services.storage;
    // Recheck after rendering/awaits: concurrent revocation or revision changes cannot
    // serve a representation authorized under an earlier configuration.
    match db
        .authorize_feed(public_id, secret.expose(), state.services.business_now_ms())
        .await
    {
        Ok(current) if &current.key == key => {}
        Ok(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(e) => return denied(e).into_response(),
    }
    let matches = headers
        .get_all("if-none-match")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| {
            v.split(',').any(|v| {
                let v = v.trim();
                v == "*" || v.strip_prefix("W/").unwrap_or(v) == document.etag
            })
        });
    let mut response = if matches {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        document.body.clone().into_response()
    };
    let h = response.headers_mut();
    h.insert(
        "content-type",
        "text/calendar; charset=utf-8".parse().unwrap(),
    );
    h.insert("cache-control", "private, no-cache".parse().unwrap());
    h.insert("etag", document.etag.parse().expect("ASCII digest"));
    response
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FeedRenderError {
    Invalid,
    Budget,
}
fn render(snapshot: FeedSnapshot, max_bytes: usize) -> Result<Document, FeedRenderError> {
    render_with(snapshot, max_bytes, render_ics)
}
fn component_budget(input_bytes: usize) -> Option<usize> {
    input_bytes.checked_mul(4)?.checked_add(512)
}
fn render_with(
    snapshot: FeedSnapshot,
    max_bytes: usize,
    encode: impl FnMut(&[FeedEvent<'_>]) -> Result<String, calendar_adapters::renderer::RenderError>,
) -> Result<Document, FeedRenderError> {
    let body = encode_records(
        &snapshot.records,
        snapshot.authorization.scope.visible_fields(),
        max_bytes,
        encode,
    )?;
    use sha2::{Digest, Sha256};
    let key = serde_json::to_vec(&(
        &snapshot.authorization.key.subscription_id,
        snapshot.authorization.key.configuration_revision,
        &snapshot.authorization.key.source_id,
        snapshot.authorization.key.data_revision,
        snapshot.authorization.key.scope_digest,
    ))
    .map_err(|_| FeedRenderError::Invalid)?;
    let mut hash = Sha256::new();
    hash.update(key);
    hash.update(body.as_bytes());
    Ok(Document {
        body,
        etag: format!("\"{:x}\"", hash.finalize()),
    })
}

pub(crate) fn render_body(
    records: &[calendar_storage::feed::FeedRecord],
    fields: &[calendar_application::VisibleField],
    max_bytes: usize,
) -> Result<String, FeedRenderError> {
    encode_records(records, Some(fields), max_bytes, render_ics)
}
fn encode_records(
    records: &[calendar_storage::feed::FeedRecord],
    fields: Option<&[calendar_application::VisibleField]>,
    max_bytes: usize,
    mut encode: impl FnMut(&[FeedEvent<'_>]) -> Result<String, calendar_adapters::renderer::RenderError>,
) -> Result<String, FeedRenderError> {
    let mut documents: Vec<EventV1> = records
        .iter()
        .map(|r| serde_json::from_str(&r.document_json).map_err(|_| FeedRenderError::Invalid))
        .collect::<Result<_, _>>()?;
    // Bound the original document before either encoding pass. For #31's restricted
    // profile, 4x covers TEXT escaping, CRLF/folding and temporal/property prefixes;
    // 512 covers each component's fixed UID/stamps/status framing. Only SUMMARY and
    // recurrence identity are repeated for cancellation exceptions (not the entire JSON).
    // This intentionally conservative admission budget may reject an output that would fit.
    let mut remaining = max_bytes.checked_sub(256).ok_or(FeedRenderError::Budget)?;
    let mut charge = |bytes: usize| -> Result<(), FeedRenderError> {
        remaining = remaining
            .checked_sub(bytes)
            .ok_or(FeedRenderError::Budget)?;
        Ok(())
    };
    for (record, document) in records.iter().zip(&documents) {
        charge(component_budget(record.document_json.len()).ok_or(FeedRenderError::Budget)?)?;
        for exception in document
            .recurrence
            .iter()
            .flat_map(|r| r.exceptions.iter().flatten())
        {
            let identity = serde_json::to_vec(&exception.recurrence_id)
                .map_err(|_| FeedRenderError::Invalid)?;
            let repeated = document
                .title
                .len()
                .checked_add(identity.len())
                .ok_or(FeedRenderError::Budget)?;
            charge(component_budget(repeated).ok_or(FeedRenderError::Budget)?)?;
        }
    }
    let stamps: Vec<String> = records
        .iter()
        .map(|r| {
            chrono::DateTime::from_timestamp_millis(r.changed_at)
                .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
                .ok_or(FeedRenderError::Invalid)
        })
        .collect::<Result<_, _>>()?;
    // Validate the original input against #31's profile before removing content.
    // Redaction must not accidentally make unsupported/invalid input renderable.
    let mut render_documents = |documents: &[EventV1]| -> Result<String, FeedRenderError> {
        let events: Vec<_> = records
            .iter()
            .zip(documents)
            .zip(&stamps)
            .map(|((r, document), stamp)| {
                Ok(FeedEvent {
                    event_id: &r.event_id,
                    revision: r
                        .revision
                        .try_into()
                        .map_err(|_| FeedRenderError::Invalid)?,
                    changed_at: stamp,
                    deleted: r.deleted,
                    document,
                })
            })
            .collect::<Result<_, _>>()?;
        encode(&events).map_err(|_| FeedRenderError::Invalid)
    };
    let original = render_documents(&documents)?;
    let body = if let Some(fields) = fields {
        for document in &mut documents {
            calendar_application::project_visible_fields(document, fields);
        }
        render_documents(&documents)?
    } else {
        original
    };
    if body.len() > max_bytes {
        return Err(FeedRenderError::Budget);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, Server};
    use calendar_application::FeedScope;
    use std::time::Duration;
    #[tokio::test]
    async fn prepared_representation_fence_rejects_concurrent_policy_data_expiry_rotation_and_revoke(
    ) {
        for change in ["scope", "data", "expiry", "rotation", "revoke", "disable"] {
            let dir = tempfile::tempdir().unwrap();
            let server = Server::initialize(Config {
                bind: "127.0.0.1:0".parse().unwrap(),
                database: dir.path().join("db"),
                max_request_bytes: 1024,
                max_response_bytes: 16384,
                request_timeout: Duration::from_secs(3),
            })
            .await
            .unwrap();
            let db = &server.services.storage;
            sqlx::query("INSERT INTO sources(id,kind) VALUES ('s','test')")
                .execute(db.pool())
                .await
                .unwrap();
            let token = db
                .issue_feed_token("sub", "public", "s", None)
                .await
                .unwrap();
            let state = FeedState {
                services: server.services.clone(),
                cache: Arc::default(),
                max_bytes: 16384,
            };
            let snapshot = db
                .feed_snapshot("public", token.expose(), now())
                .await
                .unwrap();
            let key = snapshot.authorization.key.clone();
            let document = Arc::new(render(snapshot, 16384).unwrap());
            let mut headers = HeaderMap::new();
            headers.insert("if-none-match", document.etag.parse().unwrap());
            // Deliberately hold the already-rendered representation across a separate task's
            // committed mutation. No sleeps or race-probability assertions.
            let services = server.services.clone();
            tokio::spawn(async move {
                let db = &services.storage;
                match change {
                    "scope" => db
                        .configure_feed_scope(
                            "sub",
                            0,
                            &FeedScope::parse(
                                r#"{"version":1,"calendar_ids":[],"visible_fields":[]}"#,
                            )
                            .unwrap(),
                        )
                        .await
                        .unwrap(),
                    "data" => {
                        sqlx::query("UPDATE sources SET revision=revision+1")
                            .execute(db.pool())
                            .await
                            .unwrap();
                    }
                    "expiry" => {
                        sqlx::query("UPDATE feed_subscriptions SET expires_at=0")
                            .execute(db.pool())
                            .await
                            .unwrap();
                    }
                    "rotation" => {
                        db.rotate_feed_token("sub", 0).await.unwrap();
                    }
                    "revoke" => db.revoke_feed_token("sub", 0).await.unwrap(),
                    "disable" => {
                        sqlx::query("UPDATE sources SET enabled=0")
                            .execute(db.pool())
                            .await
                            .unwrap();
                    }
                    _ => unreachable!(),
                }
            })
            .await
            .unwrap();
            let response = finish_feed(&state, "public", &token, &key, document, headers).await;
            assert_eq!(
                response.status(),
                if ["scope", "data"].contains(&change) {
                    StatusCode::SERVICE_UNAVAILABLE
                } else {
                    StatusCode::NOT_FOUND
                },
                "{change}"
            );
            assert!(!response.headers().contains_key("etag"));
            assert!(axum::body::to_bytes(response.into_body(), 16384)
                .await
                .unwrap()
                .is_empty());
            db.close().await;
        }
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    use calendar_application::{EventV1, FeedScope};
    use calendar_storage::feed::{FeedAuthorization, FeedRecord};
    fn snapshot(restricted: bool) -> FeedSnapshot {
        let mut value = serde_json::json!({"schema_version":"1.0","external_uid":"upstream","title":"x".repeat(1024),"status":"confirmed","start":{"kind":"utc","value":"2026-01-01T00:00:00Z"},"duration":"PT1H","recurrence":{"rrule":"FREQ=DAILY;COUNT=257","exceptions":[]}});
        for day in 1..=256 {
            let at = chrono::DateTime::from_timestamp(1767225600 + day * 86400, 0)
                .unwrap()
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string();
            value["recurrence"]["exceptions"].as_array_mut().unwrap().push(serde_json::json!({"recurrence_id":{"kind":"utc","value":at},"range":"single","change":{"kind":"cancelled"}}));
        }
        let document: EventV1 = serde_json::from_value(value.clone()).unwrap();
        // Prove the test data is supported by the existing renderer.
        let output = render_ics(&[FeedEvent {
            event_id: "event",
            revision: 1,
            changed_at: "2026-01-01T00:00:00Z",
            deleted: false,
            document: &document,
        }])
        .unwrap();
        assert!(output.len() > 256 * 1024);
        assert!(component_budget(value.to_string().len()).unwrap() + 256 < 256 * 1024);
        FeedSnapshot {
            authorization: FeedAuthorization {
                key: FeedKey {
                    subscription_id: "s".into(),
                    configuration_revision: 0,
                    source_id: "source".into(),
                    data_revision: 1,
                    scope_digest: [0; 32],
                },
                scope: if restricted {
                    FeedScope::parse(r#"{"version":1,"calendar_ids":["c"],"visible_fields":[]}"#)
                        .unwrap()
                } else {
                    FeedScope::Legacy
                },
            },
            records: vec![FeedRecord {
                event_id: "event".into(),
                revision: 1,
                changed_at: 1767225600000,
                deleted: false,
                document_json: value.to_string(),
            }],
        }
    }
    #[test]
    fn recurring_fanout_is_rejected_before_original_or_projected_encoding() {
        for restricted in [false, true] {
            let result = render_with(snapshot(restricted), 256 * 1024, |_| {
                panic!("over-budget input reached the encoder")
            });
            assert!(matches!(result, Err(FeedRenderError::Budget)));
        }
    }
    #[test]
    fn arithmetic_overflow_denies_and_a_sufficient_budget_preserves_supported_rendering() {
        assert_eq!(component_budget(usize::MAX), None);
        assert_eq!(component_budget(usize::MAX / 4), None);
        let document = render(snapshot(false), 8 * 1024 * 1024).unwrap();
        assert!(document.body.len() > 256 * 1024);
    }
}
