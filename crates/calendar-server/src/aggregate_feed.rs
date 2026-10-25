//! Independent capability route; uses the shared restricted encoder/budget, never admin auth.
use crate::{
    feed::{now, render_body, FeedRenderError},
    Services,
};
use axum::{
    extract::{Path, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use calendar_application::{aggregate::*, SecretBytes};
use calendar_storage::feed::FeedRecord;
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;
#[derive(Clone)]
struct StateData {
    services: Arc<Services>,
    max_bytes: usize,
    cache: Arc<Mutex<HashMap<String, Arc<Document>>>>,
}
struct Document {
    body: String,
    etag: String,
}
pub(crate) fn router(services: Arc<Services>, max_bytes: usize) -> Router {
    Router::new()
        .route("/feeds/aggregates/{public_id}/{secret}", get(feed))
        .with_state(StateData {
            services,
            max_bytes,
            cache: Arc::default(),
        })
        .layer(middleware::from_fn(privacy))
}
async fn privacy(request: Request, next: Next) -> Response {
    let mut r = next.run(request).await;
    let cache = if r.status().is_success() || r.status() == StatusCode::NOT_MODIFIED {
        "private, no-cache"
    } else {
        "private, no-store"
    };
    r.headers_mut()
        .insert("cache-control", cache.parse().unwrap());
    r
}
fn denied(e: AggregateError) -> StatusCode {
    match e {
        AggregateError::Denied | AggregateError::NotFound | AggregateError::Invalid => {
            StatusCode::NOT_FOUND
        }
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}
fn key(key: &AggregateKey) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(key).expect("finite aggregate fence"))
    )
}
async fn feed(
    State(s): State<StateData>,
    Path((id, secret)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if !s.services.ready() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let Some(secret) = secret.strip_suffix(".ics") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let secret = SecretBytes::new(secret.as_bytes().to_vec());
    let db = &s.services.storage;
    let mut authorization = match db.authorize_aggregate(&id, secret.expose(), now()).await {
        Ok(v) => v,
        Err(e) => return denied(e).into_response(),
    };
    let cached = s.cache.lock().await.get(&key(&authorization.key)).cloned();
    let document = if let Some(v) = cached {
        v
    } else {
        let snapshot = match db.aggregate_snapshot(&id, secret.expose(), now()).await {
            Ok(v) => v,
            Err(e) => return denied(e).into_response(),
        };
        authorization = snapshot.authorization;
        let records: Vec<_> = snapshot
            .events
            .into_iter()
            .map(|v| FeedRecord {
                event_id: v.event_id,
                revision: v.revision,
                changed_at: v.changed_at,
                deleted: v.deleted,
                document_json: v.document_json,
            })
            .collect();
        let body = match render_body(
            &records,
            &authorization.configuration.visible_fields,
            s.max_bytes,
        ) {
            Ok(v) => v,
            Err(FeedRenderError::Budget) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            Err(FeedRenderError::Invalid) => {
                return StatusCode::UNPROCESSABLE_ENTITY.into_response()
            }
        };
        let cache_key = key(&authorization.key);
        let mut hash = Sha256::new();
        hash.update(cache_key.as_bytes());
        hash.update(body.as_bytes());
        let document = Arc::new(Document {
            body,
            etag: format!("\"{:x}\"", hash.finalize()),
        });
        let mut cache = s.cache.lock().await;
        if cache.len() >= 16 {
            cache.clear();
        }
        cache.insert(cache_key, document.clone());
        document
    };
    finish(&s, &id, &secret, &authorization.key, document, headers).await
}
async fn finish(
    s: &StateData,
    id: &str,
    secret: &SecretBytes,
    fence: &AggregateKey,
    document: Arc<Document>,
    headers: HeaderMap,
) -> Response {
    match s
        .services
        .storage
        .authorize_aggregate(id, secret.expose(), now())
        .await
    {
        Ok(a) if &a.key == fence => {}
        Ok(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(e) => return denied(e).into_response(),
    }
    let matched = headers
        .get_all("if-none-match")
        .iter()
        .filter_map(|h| h.to_str().ok())
        .any(|h| {
            h.split(',').any(|v| {
                let v = v.trim();
                v == "*" || v.strip_prefix("W/").unwrap_or(v) == document.etag
            })
        });
    let mut response = if matched {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        document.body.clone().into_response()
    };
    response
        .headers_mut()
        .insert("etag", document.etag.parse().unwrap());
    response.headers_mut().insert(
        "content-type",
        "text/calendar; charset=utf-8".parse().unwrap(),
    );
    response
        .headers_mut()
        .insert("cache-control", "private, no-cache".parse().unwrap());
    response
}
pub(crate) fn describe(document: &mut serde_json::Value) {
    let mut single = serde_json::json!({"paths":{}});
    crate::feed::describe(&mut single);
    let mut path = single["paths"]["/feeds/{public_id}/{secret}.ics"].take();
    for method in ["get", "head"] {
        path[method]["summary"] = serde_json::json!("Read a managed aggregate calendar");
        path[method]["description"]=serde_json::json!("Read one consistent managed aggregate. All members must remain authorized and fresh under reject policy; no partial source omission. Independent aggregate capability, never management credentials.");
    }
    document["paths"]["/feeds/aggregates/{public_id}/{secret}.ics"] = path;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, Server};
    use calendar_application::configuration::{
        ConfigurationCondition as Condition, SourceConfigurationV1,
    };
    use std::time::Duration;
    #[tokio::test]
    async fn prepared_aggregate_fence_rejects_concurrent_policy_member_data_and_capability_changes()
    {
        for change in [
            "policy",
            "configuration",
            "data",
            "selection",
            "expiry",
            "rotate",
            "retire",
            "source-retire",
            "disable",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let server = Server::initialize(Config {
                bind: "127.0.0.1:0".parse().unwrap(),
                database: dir.path().join("db"),
                max_request_bytes: 65536,
                max_response_bytes: 65536,
                request_timeout: Duration::from_secs(5),
            })
            .await
            .unwrap();
            let db = &server.services.storage;
            let mut source: SourceConfigurationV1 = serde_json::from_str(include_str!(
                "../../../examples/source-configuration-v1.json"
            ))
            .unwrap();
            db.put_source_configuration("source", Condition::Absent, source.clone(), "test", 1)
                .await
                .unwrap();
            source.enabled = true;
            db.put_source_configuration(
                "source",
                Condition::Revision(1),
                source.clone(),
                "test",
                2,
            )
            .await
            .unwrap();
            let mut config:AggregateConfiguration=serde_json::from_value(serde_json::json!({"version":1,"name":"Combined","enabled":false,"sources":[{"source_id":"source","calendar_ids":["calendar-a"]}],"visible_fields":["summary"],"stale_policy":"reject","expires_at":null})).unwrap();
            let (r, secret) = db
                .put_aggregate("sub", Condition::Absent, config.clone(), "test", 3)
                .await
                .unwrap();
            let secret = secret.unwrap();
            config.enabled = true;
            db.put_aggregate("sub", Condition::Revision(1), config.clone(), "test", 4)
                .await
                .unwrap();
            let fence = db
                .authorize_aggregate(&r.public_id, secret.expose(), now())
                .await
                .unwrap()
                .key;
            let state = StateData {
                services: server.services.clone(),
                max_bytes: 65536,
                cache: Arc::default(),
            };
            let document = Arc::new(Document {
                body: "old private payload".into(),
                etag: "\"old\"".into(),
            });
            let mut headers = HeaderMap::new();
            headers.insert("if-none-match", "\"old\"".parse().unwrap());
            let services = server.services.clone();
            tokio::spawn(async move {let db=&services.storage;match change {
    "policy"=>{config.visible_fields.clear();db.put_aggregate("sub",Condition::Revision(2),config,"test",5).await.unwrap();},
    "configuration"=>{source.name="changed".into();db.put_source_configuration("source",Condition::Revision(2),source,"test",5).await.unwrap();},
    "data"=>{sqlx::query("UPDATE sources SET revision=revision+1").execute(db.pool()).await.unwrap();},
    "selection"=>{sqlx::query("UPDATE calendars SET selected=0").execute(db.pool()).await.unwrap();},
    "expiry"=>{sqlx::query("UPDATE aggregate_subscriptions SET configuration_json=json_set(configuration_json,'$.expires_at',0)").execute(db.pool()).await.unwrap();},
    "rotate"=>{db.rotate_aggregate("sub",2,"test",5).await.unwrap();},
    "retire"=>{db.retire_aggregate("sub",2,"test",5).await.unwrap();},
    "source-retire"=>{db.retire_source_configuration("source",2,"test",5).await.unwrap();},
    "disable"=>{source.enabled=false;db.put_source_configuration("source",Condition::Revision(2),source,"test",5).await.unwrap();},_=>unreachable!()
   }}).await.unwrap();
            let r = finish(&state, &r.public_id, &secret, &fence, document, headers).await;
            assert_eq!(
                r.status(),
                if ["policy", "configuration", "data"].contains(&change) {
                    StatusCode::SERVICE_UNAVAILABLE
                } else {
                    StatusCode::NOT_FOUND
                },
                "{change}"
            );
            assert!(!r.headers().contains_key("etag"));
            assert!(axum::body::to_bytes(r.into_body(), 1000)
                .await
                .unwrap()
                .is_empty());
            server.services.drain().await;
        }
    }
}
