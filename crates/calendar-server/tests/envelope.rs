//! standard-calendar-source/v1 snapshot batches over real HTTP routing and SQLite (#174).
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use calendar_application::{configuration::*, Clock, IngestPolicy};
use calendar_server::{Config, Server};
use calendar_storage::envelope::{EnvelopeError, SnapshotBatch};
use serde_json::{json, Value};
use std::time::Duration;
use tower::ServiceExt;

const BATCHES: &str = "/api/ingest/standard-calendar/v1/sources/push/batches";
const STATE: &str = "/api/ingest/standard-calendar/v1/sources/push/state";

struct Setup {
    _dir: tempfile::TempDir,
    server: Server,
    full: String,
    upsert_only: String,
    record: SourceConfigurationRecord,
}
async fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::initialize(Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        database: dir.path().join("db"),
        max_request_bytes: 1_048_576,
        max_response_bytes: 1_048_576,
        request_timeout: Duration::from_secs(30),
    })
    .await
    .unwrap();
    let db = &server.services.storage;
    let mut config: SourceConfigurationV1 = serde_json::from_str(include_str!(
        "../../../examples/source-configuration-v1.json"
    ))
    .unwrap();
    config.adapter = AdapterKind::StandardCalendar;
    config.mode = SourceMode::PushOnly;
    let created = db
        .put_source_configuration("push", ConfigurationCondition::Absent, config, "test", 1)
        .await
        .unwrap();
    let mut enabled = created.configuration;
    enabled.enabled = true;
    let record = db
        .put_source_configuration(
            "push",
            ConfigurationCondition::Revision(created.config_revision),
            enabled,
            "test",
            1,
        )
        .await
        .unwrap();
    let token = |id: &'static str, delete: bool| async move {
        let secret = db
            .issue_api_token(id, "push", true, delete, None)
            .await
            .unwrap();
        String::from_utf8(secret.expose().to_vec()).unwrap()
    };
    let full = token("full", true).await;
    let upsert_only = token("upsert-only", false).await;
    Setup {
        _dir: dir,
        server,
        full,
        upsert_only,
        record,
    }
}
fn event(uid: &str, title: &str) -> Value {
    json!({"schema_version":"1.0","external_uid":uid,"title":title,"status":"confirmed",
        "start":{"kind":"utc","value":"2026-11-02T17:00:00Z"}})
}
fn envelope(config: i64, source: i64, events: &[(&str, &str)]) -> Value {
    json!({"protocol_version":"standard-calendar-source/v1","batch_id":"batch-1","mode":"snapshot",
        "base_source_revision":source,"base_config_revision":config,"base_checkpoint":null,
        "proposed_checkpoint":"cp","complete":true,"pages":1,"collection_tag":null,
        "operations":events.iter().map(|(uid,title)| json!({"op":"upsert","calendar_id":"calendar-a","event":event(uid,title)})).collect::<Vec<_>>()})
}
async fn call(
    server: &Server,
    method: &str,
    uri: &str,
    token: Option<&str>,
    key: Option<&str>,
    body: String,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    let response = server
        .router()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    if status.is_success() {
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    let bytes = to_bytes(response.into_body(), 1_048_576).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
async fn post(s: &Setup, token: &str, key: &str, body: impl ToString) -> (StatusCode, Value) {
    call(
        &s.server,
        "POST",
        BATCHES,
        Some(token),
        Some(key),
        body.to_string(),
    )
    .await
}
async fn state(s: &Setup) -> Value {
    let (status, value) = call(&s.server, "GET", STATE, Some(&s.full), None, String::new()).await;
    assert_eq!(status, StatusCode::OK);
    value
}
async fn live_events(s: &Setup) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM events WHERE source_id='push' AND deleted_at IS NULL")
        .fetch_one(s.server.services.storage.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn snapshot_commits_replays_by_canonical_digest_and_fences_base_revisions() {
    let s = setup().await;
    let config = s.record.config_revision;
    let initial = state(&s).await;
    assert_eq!(
        initial,
        json!({"source_id":"push","config_revision":config,"source_revision":s.record.source_revision,
            "checkpoint":null,"has_snapshot":false})
    );
    let base = s.record.source_revision;
    let first = envelope(config, base, &[("a", "A"), ("b", "B")]);
    let (status, receipt) = post(&s, &s.full, "k1", &first).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        receipt,
        json!({"source_id":"push","batch_id":"batch-1","source_revision":base+1,"changed_events":2})
    );
    assert_eq!(live_events(&s).await, 2);
    let after = state(&s).await;
    assert_eq!(
        (
            &after["source_revision"],
            &after["checkpoint"],
            &after["has_snapshot"]
        ),
        (&json!(base + 1), &json!("cp"), &json!(true))
    );

    // The same key and a JCS-equal body (other key order and whitespace) replays the receipt.
    let mut reordered: Vec<(String, Value)> =
        first.as_object().unwrap().clone().into_iter().collect();
    reordered.reverse();
    let text = format!(
        "{{ {} }}",
        reordered
            .iter()
            .map(|(k, v)| format!("{:?} : {}", k, v))
            .collect::<Vec<_>>()
            .join(" ,\n ")
    );
    assert_eq!(
        post(&s, &s.full, "k1", text).await,
        (StatusCode::OK, receipt)
    );
    assert_eq!(state(&s).await["source_revision"], json!(base + 1));
    // The same key with a different envelope is a conflict.
    let other = envelope(config, base, &[("a", "changed")]);
    assert_eq!(
        post(&s, &s.full, "k1", &other).await.0,
        StatusCode::CONFLICT
    );
    // Stale base revisions are rejected before any change.
    assert_eq!(
        post(&s, &s.full, "k2", &other).await.0,
        StatusCode::CONFLICT
    );
    let stale_config = envelope(config + 1, base + 1, &[("a", "A")]);
    assert_eq!(
        post(&s, &s.full, "k3", &stale_config).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(live_events(&s).await, 2);
    // Absent events are deleted; an unchanged event is not counted.
    let (status, receipt) =
        post(&s, &s.full, "k4", envelope(config, base + 1, &[("a", "A")])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(receipt["changed_events"], 1);
    assert_eq!(live_events(&s).await, 1);
    // Emptying a non-empty source needs administrator approval.
    assert_eq!(
        post(&s, &s.full, "k5", envelope(config, base + 2, &[]))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(live_events(&s).await, 1);
    s.server.services.drain().await;
}

#[tokio::test]
async fn malformed_unsupported_and_unauthorized_batches_change_nothing() {
    let s = setup().await;
    let config = s.record.config_revision;
    let base = s.record.source_revision;
    let valid = envelope(config, base, &[("a", "A")]);
    let text = valid.to_string();
    let mut cases: Vec<(String, StatusCode)> = vec![
        // Duplicate keys and numbers outside the safe integer range are not I-JSON/JCS input.
        (
            text.replacen("\"pages\":1", "\"pages\":1,\"pages\":1", 1),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            text.replacen("\"pages\":1", "\"pages\":1.0", 1),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            text.replacen(
                &format!("\"base_source_revision\":{base}"),
                "\"base_source_revision\":9007199254740992",
                1,
            ),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        ("not json".into(), StatusCode::UNPROCESSABLE_ENTITY),
    ];
    for mutate in [
        |v: &mut Value| v["source_id"] = json!("other"),
        |v: &mut Value| v["protocol_version"] = json!("v2"),
        |v: &mut Value| v["operations"][0]["calendar_id"] = json!("not-this-source"),
        |v: &mut Value| v["operations"][0]["event"]["status"] = json!("unknown"),
        // Not yet supported by this leaf.
        |v: &mut Value| {
            v["mode"] = json!("delta");
            v["base_checkpoint"] = json!("old");
        },
        |v: &mut Value| {
            v["pages"] = json!(2);
            v["collection_tag"] = json!("collection");
        },
    ] {
        let mut body = valid.clone();
        mutate(&mut body);
        cases.push((body.to_string(), StatusCode::UNPROCESSABLE_ENTITY));
    }
    for (n, (body, expected)) in cases.into_iter().enumerate() {
        assert_eq!(
            post(&s, &s.full, &format!("m{n}"), body).await.0,
            expected,
            "case {n}"
        );
    }
    let server = &s.server;
    assert_eq!(
        call(server, "POST", BATCHES, Some(&s.full), None, text.clone())
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY,
        "Idempotency-Key is required"
    );
    for token in [None, Some("forged"), Some(s.upsert_only.as_str())] {
        assert_eq!(
            call(server, "POST", BATCHES, token, Some("auth"), text.clone())
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
    let other = "/api/ingest/standard-calendar/v1/sources/elsewhere/batches";
    assert_eq!(
        call(
            server,
            "POST",
            other,
            Some(&s.full),
            Some("auth"),
            text.clone()
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(server, "GET", STATE, None, None, String::new())
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(live_events(&s).await, 0);
    assert_eq!(state(&s).await["source_revision"], json!(base));

    // Revocation, disabling and retirement are checked before any receipt can be replayed.
    assert_eq!(post(&s, &s.full, "kept", &valid).await.0, StatusCode::OK);
    let db = &server.services.storage;
    let mut disabled = s.record.configuration.clone();
    disabled.enabled = false;
    db.put_source_configuration(
        "push",
        ConfigurationCondition::Revision(config),
        disabled,
        "test",
        2,
    )
    .await
    .unwrap();
    assert_eq!(
        post(&s, &s.full, "kept", &valid).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(server, "GET", STATE, Some(&s.full), None, String::new())
            .await
            .0,
        StatusCode::CONFLICT
    );
    sqlx::query("UPDATE api_tokens SET revoked_at=1 WHERE id='full'")
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        post(&s, &s.full, "kept", &valid).await.0,
        StatusCode::UNAUTHORIZED
    );
    server.services.drain().await;
}

#[tokio::test]
async fn more_than_one_hundred_deletions_need_approval() {
    let s = setup().await;
    let config = s.record.config_revision;
    let base = s.record.source_revision;
    let uids: Vec<String> = (0..102).map(|n| format!("e{n}")).collect();
    let many: Vec<(&str, &str)> = uids.iter().map(|u| (u.as_str(), "x")).collect();
    assert_eq!(
        post(&s, &s.full, "seed", envelope(config, base, &many))
            .await
            .0,
        StatusCode::OK
    );
    // Keeping one event would delete 101.
    let (status, _) = post(
        &s,
        &s.full,
        "drop101",
        envelope(config, base + 1, &many[..1]),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(live_events(&s).await, 102);
    let (status, receipt) = post(
        &s,
        &s.full,
        "drop100",
        envelope(config, base + 1, &many[..2]),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(receipt["changed_events"], 100);
    s.server.services.drain().await;
}

struct At(i64);
impl Clock for At {
    fn utc_now_ms(&self) -> i64 {
        self.0
    }
    fn monotonic_now_ms(&self) -> u64 {
        0
    }
}
#[tokio::test]
async fn receipts_are_bounded_and_expire_into_fresh_base_checks() {
    let s = setup().await;
    let db = &s.server.services.storage;
    let base = s.record.source_revision;
    let batch = |key: &str, digest: u8, source_revision: i64| SnapshotBatch {
        source_id: "push".into(),
        key: key.into(),
        digest: [digest; 32],
        batch_id: "b".into(),
        base_config_revision: s.record.config_revision,
        base_source_revision: source_revision,
        proposed_checkpoint: None,
        records: vec![],
    };
    let policy = IngestPolicy {
        retention_ms: 60_000,
        max_records: 1,
    };
    let secret = s.full.as_bytes();
    let first = db
        .ingest_snapshot_batch(secret, batch("a", 1, base), policy, &At(10))
        .await
        .unwrap();
    assert!(!first.replayed);
    // Capacity is full: a new key is refused without evicting the live receipt.
    assert_eq!(
        db.ingest_snapshot_batch(secret, batch("b", 2, base + 1), policy, &At(11))
            .await,
        Err(EnvelopeError::Capacity)
    );
    assert!(
        db.ingest_snapshot_batch(secret, batch("a", 1, base), policy, &At(60_009))
            .await
            .unwrap()
            .replayed
    );
    // After expiry the key executes again and the stale base revision is rejected.
    assert_eq!(
        db.ingest_snapshot_batch(secret, batch("a", 1, base), policy, &At(60_010))
            .await,
        Err(EnvelopeError::Conflict)
    );
    s.server.services.drain().await;
}
