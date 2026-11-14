//! standard-calendar-source/v1 batches for push-only sources: single-page snapshots (#174) and
//! deltas (#176). Multi-page batches are rejected as not yet supported.
//! Never log bearer bytes, envelopes, event bodies or idempotency keys.
use crate::{ingest::SystemClock, Services};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use calendar_application::{configuration::valid_id, EventV1};
use calendar_storage::envelope::{BatchMode, BatchOperation, EnvelopeBatch, EnvelopeError};
use serde::{de, Deserialize, Deserializer};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, LazyLock};

const BATCHES: &str = "/api/ingest/standard-calendar/v1/sources/{source_id}/batches";
const STATE: &str = "/api/ingest/standard-calendar/v1/sources/{source_id}/state";
/// The synchronization byte budget; the deployment request limit still applies first.
const MAX_BATCH_BYTES: usize = 32 * 1024 * 1024;
/// Largest integer every JCS (IEEE 754 double) implementation represents exactly.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

fn schema(text: &str) -> Value {
    serde_json::from_str(text).expect("embedded schema JSON")
}
// Trusted embedded schemas only; references resolve from this registry, never the network.
static REGISTRY: LazyLock<jsonschema::Registry<'static>> = LazyLock::new(|| {
    jsonschema::Registry::new()
        .add(
            "urn:calrelay:schema:event:1.0",
            schema(include_str!("../../../schemas/event-v1.schema.json")),
        )
        .expect("event schema resource")
        .prepare()
        .expect("schema registry")
});
static ENVELOPE_SCHEMA: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
    jsonschema::options()
        .should_validate_formats(true)
        .with_registry(&REGISTRY)
        .build(&schema(include_str!(
            "../../../schemas/standard-calendar-source-v1.schema.json"
        )))
        .expect("embedded envelope schema")
});

/// A JSON value parsed with duplicate object keys rejected (I-JSON, RFC 7493 §2.3).
struct Strict(Value);
impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StrictVisitor).map(Strict)
    }
}
struct StrictVisitor;
impl<'de> de::Visitor<'de> for StrictVisitor {
    type Value = Value;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("JSON")
    }
    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(v.into())
    }
    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(v.into())
    }
    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(v.into())
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Value, E> {
        Err(E::custom("only integers are accepted"))
    }
    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(v.into())
    }
    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(v.into())
    }
    fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(Strict(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }
    fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut object = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(de::Error::custom("duplicate key"));
            }
            let Strict(value) = map.next_value()?;
            object.insert(key, value);
        }
        Ok(Value::Object(object))
    }
}
/// Parse an envelope accepting only I-JSON with exactly representable (safe integer) numbers.
fn parse(body: &[u8]) -> Option<Value> {
    fn safe(value: &Value) -> bool {
        match value {
            Value::Number(n) => n
                .as_i64()
                .map(i64::unsigned_abs)
                .or(n.as_u64())
                .is_some_and(|n| n <= MAX_SAFE_INTEGER),
            Value::Array(items) => items.iter().all(safe),
            Value::Object(map) => map.values().all(safe),
            _ => true,
        }
    }
    let Strict(value) = serde_json::from_slice(body).ok()?;
    safe(&value).then_some(value)
}
/// RFC 8785 JCS. Strings and safe integers serialize as in ECMAScript `JSON.stringify`, which
/// serde_json matches; object keys are sorted by UTF-16 code units.
fn canonical(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
            out.push(b'{');
            for (index, (key, item)) in entries.into_iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                serde_json::to_writer(&mut *out, key).expect("string serialization");
                out.push(b':');
                canonical(item, out);
            }
            out.push(b'}');
        }
        Value::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                canonical(item, out);
            }
            out.push(b']');
        }
        scalar => serde_json::to_writer(&mut *out, scalar).expect("scalar serialization"),
    }
}
pub(crate) fn digest(value: &Value) -> [u8; 32] {
    let mut bytes = Vec::new();
    canonical(value, &mut bytes);
    Sha256::digest(bytes).into()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    batch_id: String,
    mode: String,
    base_source_revision: i64,
    base_config_revision: i64,
    proposed_checkpoint: Option<String>,
    pages: u32,
    collection_tag: Option<String>,
    operations: Vec<Operation>,
    base_checkpoint: Option<String>,
    // Fixed by the schema: protocol_version and complete=true.
    #[serde(rename = "protocol_version")]
    _protocol_version: String,
    #[serde(rename = "complete")]
    _complete: bool,
}
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum Operation {
    Upsert {
        calendar_id: String,
        event: Box<EventV1>,
    },
    Delete {
        external_uid: String,
        expected_event_revision: i64,
    },
}

pub(crate) fn router() -> Router<Arc<Services>> {
    Router::new()
        .route(BATCHES, post(batch))
        .route(STATE, get(state))
        .layer(DefaultBodyLimit::max(MAX_BATCH_BYTES))
}
fn bearer(headers: &HeaderMap) -> Option<&[u8]> {
    let mut values = headers.get_all("authorization").iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        return None;
    }
    value.strip_prefix("Bearer ").map(str::as_bytes)
}
fn denied() -> Response {
    (StatusCode::UNAUTHORIZED, [("www-authenticate", "Bearer")]).into_response()
}
fn failure(error: EnvelopeError) -> Response {
    match error {
        EnvelopeError::Denied => denied(),
        EnvelopeError::Invalid => StatusCode::UNPROCESSABLE_ENTITY.into_response(),
        EnvelopeError::NotAccepting | EnvelopeError::Conflict | EnvelopeError::ReviewRequired => {
            StatusCode::CONFLICT.into_response()
        }
        EnvelopeError::Capacity => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        EnvelopeError::Database => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
fn ok(value: Value) -> Response {
    ([("cache-control", "no-store")], Json(value)).into_response()
}
async fn batch(
    State(s): State<Arc<Services>>,
    Path(source): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(secret) = bearer(&headers) else {
        return denied();
    };
    let mut keys = headers.get_all("idempotency-key").iter();
    let (Some(key), None) = (keys.next().and_then(|k| k.to_str().ok()), keys.next()) else {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    };
    if !valid_id(&source) {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    }
    let Some(value) = parse(&body).filter(|v| ENVELOPE_SCHEMA.is_valid(v)) else {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    };
    let digest = digest(&value);
    let Ok(envelope) = serde_json::from_value::<Envelope>(value) else {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    };
    // Not yet supported: multi-page collections.
    if envelope.pages != 1 || envelope.collection_tag.is_some() {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    }
    let mode = match envelope.mode.as_str() {
        "snapshot" => BatchMode::Snapshot,
        "delta" => BatchMode::Delta {
            base_checkpoint: envelope.base_checkpoint,
        },
        _ => return StatusCode::UNPROCESSABLE_ENTITY.into_response(),
    };
    let operations = envelope
        .operations
        .into_iter()
        .map(|operation| match operation {
            Operation::Upsert { calendar_id, event } => {
                BatchOperation::Upsert { calendar_id, event }
            }
            Operation::Delete {
                external_uid,
                expected_event_revision,
            } => BatchOperation::Delete {
                external_uid,
                expected_event_revision,
            },
        })
        .collect();
    let request = EnvelopeBatch {
        source_id: source.clone(),
        key: key.into(),
        digest,
        batch_id: envelope.batch_id,
        mode,
        base_config_revision: envelope.base_config_revision,
        base_source_revision: envelope.base_source_revision,
        proposed_checkpoint: envelope.proposed_checkpoint,
        operations,
    };
    let result = s
        .run_task(async |db| {
            db.ingest_batch(secret, request, s.ingest_policy, &SystemClock)
                .await
        })
        .await;
    match result {
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Some(Err(error)) => failure(error),
        // Explicit allowlist: no event content is echoed.
        Some(Ok(receipt)) => ok(json!({"source_id":source,"batch_id":receipt.batch_id,
            "source_revision":receipt.source_revision,"changed_events":receipt.changed_events})),
    }
}
async fn state(
    State(s): State<Arc<Services>>,
    Path(source): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(secret) = bearer(&headers) else {
        return denied();
    };
    if !valid_id(&source) {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    }
    let result = s
        .run_task(async |db| db.push_source_state(secret, &source, &SystemClock).await)
        .await;
    match result {
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Some(Err(error)) => failure(error),
        Some(Ok(state)) => ok(
            json!({"source_id":source,"config_revision":state.config_revision,
            "source_revision":state.source_revision,"checkpoint":state.checkpoint,
            "has_snapshot":state.has_snapshot}),
        ),
    }
}

pub(crate) fn describe(document: &mut Value) {
    let string = |name: &str, location: &str| json!({"name":name,"in":location,"required":true,"schema":{"type":"string"}});
    let schemas = &mut document["components"]["schemas"];
    schemas["StandardCalendarBatchReceipt"] = json!({"type":"object","additionalProperties":false,
        "required":["source_id","batch_id","source_revision","changed_events"],
        "properties":{"source_id":{"type":"string"},"batch_id":{"type":"string"},
            "source_revision":{"type":"integer"},"changed_events":{"type":"integer","minimum":0}}});
    schemas["PushSourceState"] = json!({"type":"object","additionalProperties":false,
        "required":["source_id","config_revision","source_revision","checkpoint","has_snapshot"],
        "properties":{"source_id":{"type":"string"},"config_revision":{"type":"integer"},
            "source_revision":{"type":"integer"},"checkpoint":{"type":["string","null"]},
            "has_snapshot":{"type":"boolean"}}});
    document["paths"][BATCHES] = json!({"post":{
        "summary":"Commit one standard-calendar-source/v1 snapshot or delta of a push-only source",
        "description":"Body: schemas/standard-calendar-source-v1.schema.json. This version accepts pages=1 and collection_tag=null only. base_config_revision and base_source_revision must equal the current values (see the state endpoint). A snapshot needs the snapshot capability and a token with both upsert and delete permission; events absent from it are deleted, and more than 100 deletions or emptying a non-empty source is rejected unchanged pending administrator approval. A delta needs the delta capability (and delete for delete operations), the token permission of each operation kind, an existing snapshot baseline and base_checkpoint equal to the persisted checkpoint; each delete must name the event's current revision, and no absent event is deleted. A lower source_sequence than stored is a conflict. proposed_checkpoint is persisted only on success. Idempotency receipts are keyed per token and kept for the ingest retention; the same key with a different SHA-256 of the RFC 8785 canonical envelope is a conflict. Duplicate JSON keys and non-integer or unsafe-integer numbers are rejected.",
        "security":[{"IngestBearer":[]}],
        "parameters":[string("source_id","path"),string("Idempotency-Key","header")],
        "requestBody":{"required":true,"content":{"application/json":{"schema":{"type":"object","description":"standard-calendar-source/v1 envelope"}}}},
        "responses":{"200":{"description":"Snapshot committed, or the identical receipt replayed","content":{"application/json":{"schema":{"$ref":"#/components/schemas/StandardCalendarBatchReceipt"}}}},
            "401":{"description":"Token missing, wrong source, revoked, expired or without the permission the batch needs"},
            "409":{"description":"Source not accepting this mode, stale base revision or checkpoint, missing baseline, delete revision mismatch, lower source sequence, idempotency conflict, or deletions needing approval"},
            "413":{"description":"Request exceeds the byte budget"},
            "422":{"description":"Invalid envelope, event or calendar; multi-page batch not yet supported"},
            "500":{"description":"Persistence failed"},"503":{"description":"Stopping or receipt capacity exhausted"}}
    }});
    document["paths"][STATE] = json!({"get":{
        "summary":"Read the revisions a push-only producer needs for its next batch",
        "security":[{"IngestBearer":[]}],
        "parameters":[string("source_id","path")],
        "responses":{"200":{"description":"Current source state; no event content","content":{"application/json":{"schema":{"$ref":"#/components/schemas/PushSourceState"}}}},
            "401":{"description":"Token missing, wrong source, revoked or expired"},
            "409":{"description":"Source not an enabled push-only standard-calendar source declaring snapshot or delta"},
            "422":{"description":"Invalid source ID"},
            "500":{"description":"Storage failed"},"503":{"description":"Stopping"}}
    }});
}

#[cfg(test)]
mod tests {
    use super::*;
    fn jcs(text: &str) -> String {
        let mut out = Vec::new();
        canonical(&parse(text.as_bytes()).unwrap(), &mut out);
        String::from_utf8(out).unwrap()
    }
    #[test]
    fn canonical_form_matches_rfc_8785_examples() {
        // RFC 8785 §3.2.2–3.2.3 without the fractional numbers this protocol rejects.
        assert_eq!(
            jcs(
                r#"{"string":"\u20ac$\u000F\u000aA'\u0042\u0022\u005c\\\"\/","literals":[null,true,false]}"#
            ),
            r#"{"literals":[null,true,false],"string":"€$\u000f\nA'B\"\\\\\"/"}"#
        );
        // RFC 8785 §3.2.3 property sorting by UTF-16 code units.
        let sorted = jcs(
            r#"{"\u20ac":"Euro Sign","\r":"Carriage Return","\ufb33":"Hebrew Letter Dalet With Dagesh","1":"One","\ud83d\ude00":"Emoji: Grinning Face","\u0080":"Control","\u00f6":"Latin Small Letter O With Diaeresis"}"#,
        );
        let positions: Vec<_> = [
            "Carriage Return",
            "One",
            "Control",
            "Latin Small Letter O With Diaeresis",
            "Euro Sign",
            "Emoji: Grinning Face",
            "Hebrew Letter Dalet With Dagesh",
        ]
        .iter()
        .map(|name| sorted.find(name).unwrap())
        .collect();
        assert!(positions.windows(2).all(|w| w[0] < w[1]), "{sorted}");
        assert_eq!(
            jcs("[ 1 , -9007199254740991 ,{ } ]"),
            "[1,-9007199254740991,{}]"
        );
    }
    #[test]
    fn only_i_json_with_safe_integers_is_accepted() {
        for text in [
            r#"{"a":1,"a":1}"#,
            r#"{"a":{"b":1,"b":2}}"#,
            r#"[1.5]"#,
            r#"[1e2]"#,
            r#"[9007199254740992]"#,
            r#"[-9007199254740992]"#,
            r#"["\ud800"]"#,
        ] {
            assert!(parse(text.as_bytes()).is_none(), "{text}");
        }
        assert!(parse(br#"{"a":[9007199254740991,-9007199254740991,"x"]}"#).is_some());
    }
}
