//! standard-calendar-source/v1 snapshot batches over real HTTP routing and SQLite (#174).
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use calendar_application::{configuration::*, Clock, IngestPolicy};
use calendar_server::{Config, Server};
use calendar_storage::envelope::{BatchMode, EnvelopeBatch, EnvelopeError};
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
    setup_with(|_| {}).await
}
async fn setup_with(adjust: impl FnOnce(&mut SourceConfigurationV1)) -> Setup {
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
    adjust(&mut config);
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
        // Two pages need a shared collection tag.
        |v: &mut Value| v["pages"] = json!(2),
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
    let batch = |key: &str, digest: u8, source_revision: i64| EnvelopeBatch {
        source_id: "push".into(),
        key: key.into(),
        digest: [digest; 32],
        batch_id: "b".into(),
        mode: BatchMode::Snapshot,
        base_config_revision: s.record.config_revision,
        base_source_revision: source_revision,
        proposed_checkpoint: None,
        pages: 1,
        collection_tag: None,
        operations: vec![],
    };
    let policy = IngestPolicy {
        retention_ms: 60_000,
        max_records: 1,
    };
    let secret = s.full.as_bytes();
    let first = db
        .ingest_batch(secret, batch("a", 1, base), policy, &At(10))
        .await
        .unwrap();
    assert!(
        matches!(first, calendar_storage::envelope::BatchOutcome::Committed(ref r) if !r.replayed)
    );
    // Capacity is full: a new key is refused without evicting the live receipt.
    assert_eq!(
        db.ingest_batch(secret, batch("b", 2, base + 1), policy, &At(11))
            .await,
        Err(EnvelopeError::Capacity)
    );
    assert!(matches!(
        db.ingest_batch(secret, batch("a", 1, base), policy, &At(60_009)).await,
        Ok(calendar_storage::envelope::BatchOutcome::Committed(r)) if r.replayed
    ));
    // The budget is shared: a single-event write (#30) cannot add a receipt past it either.
    let event: calendar_application::EventV1 = serde_json::from_value(event("one", "One")).unwrap();
    let single = calendar_application::IngestRequest {
        source_id: "push".into(),
        uid: "one".into(),
        key: "single".into(),
        condition: calendar_application::IngestCondition::Absent,
        operation: calendar_application::IngestOperation::Upsert {
            calendar_id: "calendar-a".into(),
            event: Box::new(event),
        },
    };
    assert!(matches!(
        db.ingest(secret, single, policy, &At(12), "request").await,
        Err(calendar_storage::ingest::IngestError::Capacity)
    ));
    // After expiry the key executes again and the stale base revision is rejected.
    assert_eq!(
        db.ingest_batch(secret, batch("a", 1, base), policy, &At(60_010))
            .await,
        Err(EnvelopeError::Conflict)
    );
    s.server.services.drain().await;
}

fn delta(config: i64, source: i64, base: Option<&str>, proposed: &str, ops: Vec<Value>) -> Value {
    json!({"protocol_version":"standard-calendar-source/v1","batch_id":"delta-1","mode":"delta",
        "base_source_revision":source,"base_config_revision":config,"base_checkpoint":base,
        "proposed_checkpoint":proposed,"complete":true,"pages":1,"collection_tag":null,"operations":ops})
}
fn upsert(uid: &str, title: &str) -> Value {
    json!({"op":"upsert","calendar_id":"calendar-a","event":event(uid,title)})
}
fn remove(uid: &str, revision: i64) -> Value {
    json!({"op":"delete","external_uid":uid,"expected_event_revision":revision})
}
async fn live(s: &Setup, uid: &str) -> bool {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM events WHERE source_id='push' AND external_uid=? AND deleted_at IS NULL)")
        .bind(uid)
        .fetch_one(s.server.services.storage.pool())
        .await
        .unwrap()
}
fn incremental(c: &mut SourceConfigurationV1) {
    c.capabilities.delta = true;
    c.capabilities.delete = true;
}

#[tokio::test]
async fn delta_needs_a_baseline_matching_checkpoint_and_explicit_revisioned_deletes() {
    let s = setup_with(incremental).await;
    let config = s.record.config_revision;
    let base = s.record.source_revision;
    // No complete snapshot baseline yet: the producer must send a snapshot first.
    let early = delta(config, base, None, "cp1", vec![upsert("a", "A")]);
    assert_eq!(
        post(&s, &s.full, "early", &early).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        post(
            &s,
            &s.full,
            "snap",
            envelope(config, base, &[("a", "A"), ("b", "B")])
        )
        .await
        .0,
        StatusCode::OK
    );
    let rev = base + 1;
    // Explicit delete of b at its revision plus a new event; absent events are kept.
    let body = delta(
        config,
        rev,
        Some("cp"),
        "cp2",
        vec![upsert("c", "C"), remove("b", 1)],
    );
    let (status, receipt) = post(&s, &s.full, "d1", &body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        receipt,
        json!({"source_id":"push","batch_id":"delta-1","source_revision":rev+1,"changed_events":2})
    );
    assert!(live(&s, "a").await && live(&s, "c").await && !live(&s, "b").await);
    let after = state(&s).await;
    assert_eq!(
        (&after["checkpoint"], &after["source_revision"]),
        (&json!("cp2"), &json!(rev + 1))
    );
    let rev = rev + 1;
    // Mismatched checkpoint, stale revisions, wrong or missing delete targets change nothing.
    for (key, body) in [
        (
            "stale-cp",
            delta(config, rev, Some("cp"), "x", vec![upsert("d", "D")]),
        ),
        (
            "null-cp",
            delta(config, rev, None, "x", vec![upsert("d", "D")]),
        ),
        (
            "stale-rev",
            delta(config, rev - 1, Some("cp2"), "x", vec![upsert("d", "D")]),
        ),
        (
            "stale-config",
            delta(config + 1, rev, Some("cp2"), "x", vec![upsert("d", "D")]),
        ),
        (
            "wrong-revision",
            delta(config, rev, Some("cp2"), "x", vec![remove("a", 7)]),
        ),
        (
            "missing",
            delta(config, rev, Some("cp2"), "x", vec![remove("zz", 1)]),
        ),
    ] {
        assert_eq!(
            post(&s, &s.full, key, &body).await.0,
            StatusCode::CONFLICT,
            "{key}"
        );
    }
    assert!(!live(&s, "d").await && live(&s, "a").await);
    assert_eq!(state(&s).await["checkpoint"], json!("cp2"));
    // Deleting an already deleted event at its current revision is no change.
    let (status, receipt) = post(
        &s,
        &s.full,
        "again",
        delta(config, rev, Some("cp2"), "cp3", vec![remove("b", 2)]),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(receipt["changed_events"], 0);
    let rev = rev + 1;
    // A lower upstream sequence than the stored one is a conflict.
    let mut newer = upsert("a", "A2");
    newer["event"]["source_sequence"] = json!(5);
    assert_eq!(
        post(
            &s,
            &s.full,
            "seq5",
            delta(config, rev, Some("cp3"), "cp4", vec![newer])
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut older = upsert("a", "A1");
    older["event"]["source_sequence"] = json!(4);
    assert_eq!(
        post(
            &s,
            &s.full,
            "seq4",
            delta(config, rev + 1, Some("cp4"), "cp5", vec![older])
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    s.server.services.drain().await;
}

#[tokio::test]
async fn delta_operations_need_their_own_permission_and_capability() {
    let s = setup_with(incremental).await;
    let config = s.record.config_revision;
    let base = s.record.source_revision;
    assert_eq!(
        post(&s, &s.full, "snap", envelope(config, base, &[("a", "A")]))
            .await
            .0,
        StatusCode::OK
    );
    // An upsert-only token may send upserts but not deletes.
    let deleting = delta(config, base + 1, Some("cp"), "cp2", vec![remove("a", 1)]);
    assert_eq!(
        post(&s, &s.upsert_only, "del", &deleting).await.0,
        StatusCode::UNAUTHORIZED
    );
    let adding = delta(config, base + 1, Some("cp"), "cp2", vec![upsert("b", "B")]);
    assert_eq!(
        post(&s, &s.upsert_only, "add", &adding).await.0,
        StatusCode::OK
    );
    assert!(live(&s, "a").await);
    s.server.services.drain().await;

    // Without the delete capability a delete is refused; without delta no delta is accepted.
    for (adjust, body_has_delete) in [
        (
            (|c: &mut SourceConfigurationV1| c.capabilities.delta = true)
                as fn(&mut SourceConfigurationV1),
            true,
        ),
        (
            (|_: &mut SourceConfigurationV1| {}) as fn(&mut SourceConfigurationV1),
            false,
        ),
    ] {
        let s = setup_with(adjust).await;
        let config = s.record.config_revision;
        let base = s.record.source_revision;
        assert_eq!(
            post(&s, &s.full, "snap", envelope(config, base, &[("a", "A")]))
                .await
                .0,
            StatusCode::OK
        );
        let op = if body_has_delete {
            remove("a", 1)
        } else {
            upsert("b", "B")
        };
        let body = delta(config, base + 1, Some("cp"), "cp2", vec![op]);
        assert_eq!(
            post(&s, &s.full, "delta", &body).await.0,
            StatusCode::CONFLICT
        );
        assert!(live(&s, "a").await);
        s.server.services.drain().await;
    }
}

#[tokio::test]
async fn a_delta_only_source_reads_its_state_but_cannot_start_without_a_baseline() {
    let s = setup_with(|c| {
        c.capabilities.snapshot = false;
        c.capabilities.delta = true;
    })
    .await;
    assert_eq!(state(&s).await["has_snapshot"], json!(false));
    let config = s.record.config_revision;
    let base = s.record.source_revision;
    let body = delta(config, base, None, "cp", vec![upsert("a", "A")]);
    assert_eq!(post(&s, &s.full, "d", &body).await.0, StatusCode::CONFLICT);
    assert_eq!(
        post(&s, &s.full, "s", envelope(config, base, &[("a", "A")]))
            .await
            .0,
        StatusCode::CONFLICT
    );
    s.server.services.drain().await;
}

fn paged(mut body: Value, tag: &str, pages: u32, batch: &str) -> Value {
    body["pages"] = json!(pages);
    body["collection_tag"] = json!(tag);
    body["batch_id"] = json!(batch);
    body
}

#[tokio::test]
async fn a_multi_page_snapshot_commits_only_when_every_page_has_arrived() {
    let s = setup().await;
    let config = s.record.config_revision;
    let base = s.record.source_revision;
    let first = paged(
        envelope(config, base, &[("a", "A"), ("b", "B")]),
        "t1",
        2,
        "p1",
    );
    let staged = json!({"source_id":"push","batch_id":"p1","collection_tag":"t1","received_pages":1,"pages":2});
    assert_eq!(
        post(&s, &s.full, "k1", &first).await,
        (StatusCode::ACCEPTED, staged.clone())
    );
    // Nothing is visible before the collection is complete.
    assert_eq!(live_events(&s).await, 0);
    assert_eq!(state(&s).await["source_revision"], json!(base));
    // A page replays its staged answer; the same key with another page is a conflict.
    assert_eq!(
        post(&s, &s.full, "k1", &first).await,
        (StatusCode::ACCEPTED, staged)
    );
    let other = paged(envelope(config, base, &[("z", "Z")]), "t1", 2, "p1");
    assert_eq!(
        post(&s, &s.full, "k1", &other).await.0,
        StatusCode::CONFLICT
    );
    // A page whose header differs from the first page is refused.
    let mut mismatched = paged(envelope(config, base, &[("c", "C")]), "t1", 2, "p2");
    mismatched["proposed_checkpoint"] = json!("other");
    assert_eq!(
        post(&s, &s.full, "k2", &mismatched).await.0,
        StatusCode::CONFLICT
    );
    let last = paged(envelope(config, base, &[("c", "C")]), "t1", 2, "p2");
    let (status, receipt) = post(&s, &s.full, "k2", &last).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        receipt,
        json!({"source_id":"push","batch_id":"p2","source_revision":base+1,"changed_events":3})
    );
    assert_eq!(live_events(&s).await, 3);
    assert_eq!(state(&s).await["checkpoint"], json!("cp"));
    // After the commit every page key replays the collection receipt.
    assert_eq!(
        post(&s, &s.full, "k1", &first).await,
        (StatusCode::OK, receipt)
    );
    s.server.services.drain().await;
}

#[tokio::test]
async fn a_new_collection_replaces_an_unfinished_one_and_bad_collections_commit_nothing() {
    let s = setup().await;
    let config = s.record.config_revision;
    let base = s.record.source_revision;
    let old = paged(envelope(config, base, &[("old", "O")]), "old", 2, "o1");
    assert_eq!(post(&s, &s.full, "o1", &old).await.0, StatusCode::ACCEPTED);
    for (key, uid) in [("n1", "a"), ("n2", "b")] {
        let page = paged(envelope(config, base, &[(uid, "N")]), "new", 2, key);
        post(&s, &s.full, key, &page).await;
        // Starting "new" discarded the unfinished "old" collection.
        let held: i64 =
            sqlx::query_scalar("SELECT count(*) FROM envelope_pages WHERE collection_tag='old'")
                .fetch_one(s.server.services.storage.pool())
                .await
                .unwrap();
        assert_eq!(held, 0);
    }
    assert_eq!(live_events(&s).await, 2);
    let rev = base + 1;
    // A duplicate UID across pages rejects the whole collection.
    let one = paged(envelope(config, rev, &[("a", "A")]), "dup", 2, "d1");
    assert_eq!(post(&s, &s.full, "d1", &one).await.0, StatusCode::ACCEPTED);
    let two = paged(envelope(config, rev, &[("a", "A2")]), "dup", 2, "d2");
    assert_eq!(
        post(&s, &s.full, "d2", &two).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(state(&s).await["source_revision"], json!(rev));
    // At most 10000 operations across all pages.
    let uids: Vec<String> = (0..10_200).map(|n| format!("u{n}")).collect();
    let mut status = StatusCode::OK;
    for (n, chunk) in uids.chunks(3_400).enumerate() {
        let events: Vec<(&str, &str)> = chunk.iter().map(|u| (u.as_str(), "x")).collect();
        let page = paged(envelope(config, rev, &events), "big", 3, &format!("b{n}"));
        status = post(&s, &s.full, &format!("b{n}"), &page).await.0;
    }
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(state(&s).await["source_revision"], json!(rev));
    s.server.services.drain().await;
}

#[tokio::test]
async fn a_multi_page_delta_applies_every_page_atomically() {
    let s = setup_with(incremental).await;
    let config = s.record.config_revision;
    let base = s.record.source_revision;
    assert_eq!(
        post(&s, &s.full, "snap", envelope(config, base, &[("a", "A")]))
            .await
            .0,
        StatusCode::OK
    );
    let rev = base + 1;
    let one = paged(
        delta(config, rev, Some("cp"), "cp2", vec![upsert("b", "B")]),
        "d",
        2,
        "d1",
    );
    assert_eq!(post(&s, &s.full, "d1", &one).await.0, StatusCode::ACCEPTED);
    assert!(!live(&s, "b").await);
    let two = paged(
        delta(config, rev, Some("cp"), "cp2", vec![remove("a", 1)]),
        "d",
        2,
        "d2",
    );
    let (status, receipt) = post(&s, &s.full, "d2", &two).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(receipt["changed_events"], 2);
    assert!(live(&s, "b").await && !live(&s, "a").await);
    assert_eq!(state(&s).await["checkpoint"], json!("cp2"));
    s.server.services.drain().await;
}

#[tokio::test]
async fn unfinished_pages_expire_after_an_hour() {
    use calendar_storage::envelope::{BatchOperation, BatchOutcome};
    let s = setup().await;
    let db = &s.server.services.storage;
    let page = |key: &str, uid: &str| EnvelopeBatch {
        source_id: "push".into(),
        key: key.into(),
        digest: [key.len() as u8; 32],
        batch_id: key.into(),
        mode: BatchMode::Snapshot,
        base_config_revision: s.record.config_revision,
        base_source_revision: s.record.source_revision,
        proposed_checkpoint: None,
        pages: 2,
        collection_tag: Some("t".into()),
        operations: vec![BatchOperation::Upsert {
            calendar_id: "calendar-a".into(),
            event: Box::new(serde_json::from_value(event(uid, "x")).unwrap()),
        }],
    };
    let policy = IngestPolicy::default();
    let secret = s.full.as_bytes();
    let first = db
        .ingest_batch(secret, page("a", "a"), policy, &At(1_000))
        .await
        .unwrap();
    assert_eq!(
        first,
        BatchOutcome::Staged {
            received_pages: 1,
            pages: 2
        }
    );
    // The first page expired, so the next one starts the collection again.
    let second = db
        .ingest_batch(secret, page("bb", "b"), policy, &At(3_601_000))
        .await
        .unwrap();
    assert_eq!(
        second,
        BatchOutcome::Staged {
            received_pages: 1,
            pages: 2
        }
    );
    s.server.services.drain().await;
}
