//! Issue #23 acceptance tests for persistent reminder reconciliation.
//!
//! Application API: `ReminderPlanner::reconcile(request, &clock)` on Storage.
//! Clock supplies UTC Unix milliseconds and an independent monotonic millisecond counter.
//! Request identifies a persisted rule/output and expected source/rule/output revisions.
//! Limits apply to the entire reconciliation, not separately to each event.
//! References: docs/event-model.md, docs/architecture.md and Context7-retrieved
//! https://docs.rs/sqlx/0.8.6/sqlx/struct.Transaction.html (rollback boundaries).

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use calendar_application::{
    Clock, EventRepository, EventV1, PlanningError, PlanningLimits, PlanningRequest,
    ReminderPlanner, SyncBatch, SyncMode, SyncRecord,
};
use calendar_storage::Storage;
use serde_json::{json, Value};
use tempfile::tempdir;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
const NOW: i64 = 1_793_577_600_000; // 2026-11-02T00:00:00Z
const START: i64 = 1_793_638_800_000; // 2026-11-02T17:00:00Z
const DUE: i64 = 1_793_637_900_000; // 2026-11-02T16:45:00Z
const DAY: i64 = 86_400_000;

struct FakeClock {
    utc: AtomicI64,
    monotonic: AtomicU64,
    step: u64,
}

impl FakeClock {
    fn fixed() -> Self {
        Self {
            utc: AtomicI64::new(NOW),
            monotonic: AtomicU64::new(0),
            step: 0,
        }
    }
}

impl Clock for FakeClock {
    fn utc_now_ms(&self) -> i64 {
        self.utc.load(Ordering::SeqCst)
    }

    fn monotonic_now_ms(&self) -> u64 {
        self.monotonic.fetch_add(self.step, Ordering::SeqCst)
    }
}

fn request(revision: i64) -> PlanningRequest {
    PlanningRequest {
        source_id: "s".into(),
        base_revision: revision,
        rule_id: "r".into(),
        rule_generation: 1,
        output_id: "o".into(),
        output_generation: 1,
        horizon_ms: 30 * DAY,
        max_lead_ms: 7 * DAY,
        extension_interval_ms: 300_000,
        tzdata_version: "2025b".into(),
        limits: PlanningLimits {
            max_occurrences: 100,
            max_iterations: 1_000,
            max_elapsed_ms: 1_000,
        },
    }
}

fn event(extra: Value) -> EventV1 {
    let mut document = json!({
        "schema_version": "1.0", "external_uid": "a", "title": "Review",
        "status": "confirmed", "duration": "PT1H",
        "start": {"kind": "utc", "value": "2026-11-02T17:00:00Z"}
    });
    for (key, value) in extra.as_object().expect("fixture object") {
        document[key] = value.clone();
    }
    serde_json::from_value(document).expect("valid EventV1 fixture")
}

async fn seed(db: &Storage) -> Result {
    // Configuration schema follows the target tables in docs/event-model.md.
    // Reconciliation reads persisted policy; it must not trust a caller-supplied event list.
    sqlx::raw_sql(
        "INSERT INTO sources(id,kind) VALUES ('s','ics'),('t','ics');
         INSERT INTO calendars(id,source_id,external_id,timezone_policy)
           VALUES ('c','s','main','{\"tzid\":\"America/Vancouver\"}'),
                  ('d','t','main','{\"tzid\":\"America/Vancouver\"}');
         INSERT INTO reminder_rules(id,policy_json,generation,enabled)
           VALUES ('r','{\"calendar_ids\":[\"c\",\"d\"],\"lead_ms\":900000,\"all_day_time\":\"09:00\"}',1,1);
         INSERT INTO outputs(id,kind,config_json,generation,enabled)
           VALUES ('o','bark','{}',1,1);
         INSERT INTO rule_outputs(rule_id,output_id) VALUES ('r','o');",
    )
    .execute(db.pool())
    .await?;
    Ok(())
}

async fn commit(db: &Storage, base: i64, events: Vec<EventV1>) -> Result {
    db.commit_sync(SyncBatch {
        source_id: "s".into(),
        base_revision: base,
        mode: SyncMode::Delta,
        records: events
            .into_iter()
            .map(|event| SyncRecord::Upsert {
                calendar_id: "c".into(),
                event: Box::new(event),
            })
            .collect(),
        proposed_checkpoint: Some(format!("revision-{}", base + 1)),
        verified_at: NOW,
    })
    .await?;
    Ok(())
}

// Include identity and revisions: a count-only assertion could miss identity replacement.
type Job = (String, String, String, String, i64, i64, i64, String);
async fn jobs(db: &Storage) -> Result<Vec<Job>> {
    Ok(sqlx::query_as(
        "SELECT e.source_id,l.original_key,l.rule_id,l.output_id,
                j.generation,j.event_revision,j.due_at,j.state
         FROM delivery_jobs j JOIN logical_deliveries l ON l.id=j.logical_delivery_id
         JOIN events e ON e.id=l.event_id
         WHERE j.state IN ('pending','retry_wait','leased')
         ORDER BY e.source_id,e.external_uid,l.original_key,l.rule_id,l.output_id,j.generation",
    )
    .fetch_all(db.pool())
    .await?)
}

async fn count(db: &Storage, table: &str) -> Result<i64> {
    let query = match table {
        "occurrences" => "SELECT count(*) FROM occurrences",
        "delivery_jobs" => "SELECT count(*) FROM delivery_jobs",
        "logical_deliveries" => "SELECT count(*) FROM logical_deliveries",
        "planning_watermarks" => "SELECT count(*) FROM planning_watermarks",
        "source_changes" => "SELECT count(*) FROM source_changes",
        _ => panic!("unsupported test table"),
    };
    Ok(sqlx::query_scalar(query).fetch_one(db.pool()).await?)
}

type Watermark = (i64, i64, i64, String);
async fn watermark(db: &Storage) -> Result<Watermark> {
    Ok(sqlx::query_as(
        "SELECT source_revision,rule_revision,planned_until,tzdata_version
         FROM planning_watermarks",
    )
    .fetch_one(db.pool())
    .await?)
}

#[tokio::test]
async fn ac1_utc_single_event_has_exact_identity_span_and_due_time() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    commit(&db, 0, vec![event(json!({}))]).await?;
    db.reconcile(request(1), &FakeClock::fixed()).await?;
    assert_eq!(
        jobs(&db).await?,
        vec![(
            "s".into(),
            "single".into(),
            "r".into(),
            "o".into(),
            1,
            1,
            DUE,
            "pending".into()
        )]
    );
    let span: (String, i64, i64) =
        sqlx::query_as("SELECT original_key,start_utc,end_utc FROM occurrences")
            .fetch_one(db.pool())
            .await?;
    assert_eq!(span, ("single".into(), START, START + 3_600_000));
    assert_eq!(
        watermark(&db).await?,
        (1, 1, NOW + 30 * DAY, "2025b".into())
    );
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac1_all_day_and_floating_use_calendar_timezone() -> Result {
    for (extra, expected_due) in [
        (
            json!({"start":{"kind":"date","date":"2026-11-02"},"duration":"P1D"}),
            START,
        ),
        (
            json!({"start":{"kind":"floating","local":"2026-11-02T09:00:00"}}),
            DUE,
        ),
    ] {
        let dir = tempdir()?;
        let db = Storage::open(dir.path().join("db")).await?;
        seed(&db).await?;
        commit(&db, 0, vec![event(extra)]).await?;
        db.reconcile(request(1), &FakeClock::fixed()).await?;
        let actual = jobs(&db).await?;
        assert_eq!(actual.len(), 1);
        assert_eq!(
            (actual[0].1.as_str(), actual[0].6),
            ("single", expected_due)
        );
        db.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn ac1_recurrence_exclusion_reschedule_and_cancellation_keep_original_keys() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    commit(&db, 0, vec![event(json!({
        "recurrence":{"rrule":"FREQ=DAILY;COUNT=4",
            "exdates":[{"kind":"utc","value":"2026-11-03T17:00:00Z"}],
        "exceptions":[
            {"recurrence_id":{"kind":"utc","value":"2026-11-04T17:00:00Z"},"range":"single","change":{"kind":"override","patch":{"start":{"kind":"utc","value":"2026-11-04T19:00:00Z"}}}},
            {"recurrence_id":{"kind":"utc","value":"2026-11-05T17:00:00Z"},"range":"single","change":{"kind":"cancelled"}}
        ]}
    }))]).await?;
    db.reconcile(request(1), &FakeClock::fixed()).await?;
    let actual: Vec<_> = jobs(&db).await?.into_iter().map(|j| (j.1, j.6)).collect();
    assert_eq!(
        actual,
        vec![
            ("utc:2026-11-02T17:00:00Z".into(), DUE),
            ("utc:2026-11-04T17:00:00Z".into(), 1_793_817_900_000),
        ]
    );
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac1_unsupported_recurrence_rejects_the_batch_without_partial_planning() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    commit(
        &db,
        0,
        vec![
            event(json!({})),
            event(json!({"external_uid":"bad","recurrence":{"rrule":"FREQ=HOURLY;COUNT=2"}})),
        ],
    )
    .await?;
    assert!(matches!(
        db.reconcile(request(1), &FakeClock::fixed()).await,
        Err(PlanningError::UnsupportedCapability(_))
    ));
    for table in [
        "occurrences",
        "logical_deliveries",
        "delivery_jobs",
        "planning_watermarks",
    ] {
        assert_eq!(count(&db, table).await?, 0, "partial write in {table}");
    }
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac2_window_must_cover_maximum_lead_plus_extension_interval() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    commit(&db, 0, vec![event(json!({}))]).await?;
    let mut short = request(1);
    short.horizon_ms = 7 * DAY + 300_000 - 1;
    assert!(matches!(
        db.reconcile(short, &FakeClock::fixed()).await,
        Err(PlanningError::InvalidWindow)
    ));
    assert_eq!(count(&db, "planning_watermarks").await?, 0);
    let mut exact = request(1);
    exact.horizon_ms = 7 * DAY + 300_000;
    db.reconcile(exact, &FakeClock::fixed()).await?;
    assert_eq!(watermark(&db).await?.2, NOW + 7 * DAY + 300_000);
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac2_window_is_half_open_and_does_not_lose_far_future_reminders() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    sqlx::query("UPDATE reminder_rules SET policy_json=json_set(policy_json,'$.lead_ms',0)")
        .execute(db.pool())
        .await?;
    commit(&db, 0, vec![
        event(json!({"external_uid":"left","start":{"kind":"utc","value":"2026-11-02T00:00:00Z"}})),
        event(json!({"external_uid":"inside","start":{"kind":"utc","value":"2026-12-01T23:59:59Z"}})),
        event(json!({"external_uid":"right","start":{"kind":"utc","value":"2026-12-02T00:00:00Z"}})),
    ]).await?;
    db.reconcile(request(1), &FakeClock::fixed()).await?;
    let due: Vec<_> = jobs(&db).await?.into_iter().map(|j| j.6).collect();
    assert_eq!(due, vec![NOW + 30 * DAY - 1_000, NOW]); // sorted by external UID
    db.close().await;

    let db = Storage::open(dir.path().join("far")).await?;
    seed(&db).await?;
    sqlx::query("UPDATE reminder_rules SET policy_json=json_set(policy_json,'$.lead_ms',?)")
        .bind(7 * DAY)
        .execute(db.pool())
        .await?;
    commit(
        &db,
        0,
        vec![event(
            json!({"start":{"kind":"utc","value":"2026-12-01T23:59:00Z"}}),
        )],
    )
    .await?;
    db.reconcile(request(1), &FakeClock::fixed()).await?;
    assert_eq!(jobs(&db).await?[0].6, 1_795_564_740_000);
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac2_occurrence_limit_is_global_and_exact_boundary_succeeds() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    commit(
        &db,
        0,
        vec![event(json!({})), event(json!({"external_uid":"b"}))],
    )
    .await?;
    let mut limited = request(1);
    limited.limits.max_occurrences = 1;
    assert!(matches!(
        db.reconcile(limited, &FakeClock::fixed()).await,
        Err(PlanningError::OccurrenceLimitExceeded)
    ));
    assert_eq!(count(&db, "delivery_jobs").await?, 0);
    assert_eq!(count(&db, "planning_watermarks").await?, 0);
    let mut exact = request(1);
    exact.limits.max_occurrences = 2;
    db.reconcile(exact, &FakeClock::fixed()).await?;
    assert_eq!(jobs(&db).await?.len(), 2);
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac2_elapsed_budget_uses_monotonic_clock_without_sleeping() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    commit(&db, 0, vec![event(json!({}))]).await?;
    let clock = FakeClock {
        step: 1_000,
        ..FakeClock::fixed()
    };
    assert!(matches!(
        db.reconcile(request(1), &clock).await,
        Err(PlanningError::TimeBudgetExceeded)
    ));
    assert_eq!(count(&db, "delivery_jobs").await?, 0);
    assert_eq!(count(&db, "planning_watermarks").await?, 0);
    db.reconcile(request(1), &FakeClock::fixed()).await?;
    assert_eq!(jobs(&db).await?.len(), 1);
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac2_timezone_change_replans_floating_without_changing_identity() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    commit(
        &db,
        0,
        vec![event(
            json!({"start":{"kind":"floating","local":"2026-11-02T09:00:00"}}),
        )],
    )
    .await?;
    db.reconcile(request(1), &FakeClock::fixed()).await?;
    assert_eq!(jobs(&db).await?[0].6, DUE);
    sqlx::query(
        "UPDATE calendars SET timezone_policy='{\"tzid\":\"America/New_York\"}' WHERE id='c'",
    )
    .execute(db.pool())
    .await?;
    db.reconcile(request(1), &FakeClock::fixed()).await?;
    let actual = jobs(&db).await?;
    assert_eq!(actual.len(), 1);
    assert_eq!(
        (actual[0].1.as_str(), actual[0].4, actual[0].6),
        ("single", 2, DUE - 3 * 3_600_000)
    );
    assert_eq!(count(&db, "logical_deliveries").await?, 1);
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac3_rescheduling_supersedes_old_generation_and_preserves_logical_identity() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    commit(&db, 0, vec![event(json!({}))]).await?;
    db.reconcile(request(1), &FakeClock::fixed()).await?;
    let logical_id: String = sqlx::query_scalar("SELECT id FROM logical_deliveries")
        .fetch_one(db.pool())
        .await?;
    commit(
        &db,
        1,
        vec![event(
            json!({"start":{"kind":"utc","value":"2026-11-02T18:00:00Z"}}),
        )],
    )
    .await?;
    db.reconcile(request(2), &FakeClock::fixed()).await?;
    assert_eq!(
        jobs(&db).await?,
        vec![(
            "s".into(),
            "single".into(),
            "r".into(),
            "o".into(),
            2,
            2,
            DUE + 3_600_000,
            "pending".into()
        )]
    );
    let unchanged_id: String = sqlx::query_scalar("SELECT id FROM logical_deliveries")
        .fetch_one(db.pool())
        .await?;
    assert_eq!(unchanged_id, logical_id);
    let old: String = sqlx::query_scalar("SELECT state FROM delivery_jobs WHERE generation=1")
        .fetch_one(db.pool())
        .await?;
    assert_eq!(old, "superseded");
    assert_eq!(watermark(&db).await?.0, 2);
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac3_cancellation_invalidates_pending_jobs() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    commit(&db, 0, vec![event(json!({}))]).await?;
    db.reconcile(request(1), &FakeClock::fixed()).await?;
    commit(&db, 1, vec![event(json!({"status":"cancelled"}))]).await?;
    db.reconcile(request(2), &FakeClock::fixed()).await?;
    assert!(jobs(&db).await?.is_empty());
    let state: String = sqlx::query_scalar("SELECT state FROM delivery_jobs")
        .fetch_one(db.pool())
        .await?;
    assert_eq!(state, "cancelled");
    assert_eq!(watermark(&db).await?.0, 2);
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac3_stale_source_revision_cannot_publish_or_advance_watermark() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    commit(&db, 0, vec![event(json!({}))]).await?;
    commit(&db, 1, vec![event(json!({"title":"updated"}))]).await?;
    assert!(matches!(
        db.reconcile(request(1), &FakeClock::fixed()).await,
        Err(PlanningError::Conflict)
    ));
    assert_eq!(count(&db, "delivery_jobs").await?, 0);
    assert_eq!(count(&db, "planning_watermarks").await?, 0);
    db.reconcile(request(2), &FakeClock::fixed()).await?;
    assert_eq!(watermark(&db).await?.0, 2);
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac3_write_failures_roll_back_occurrences_jobs_and_watermark_together() -> Result {
    // Reject each write boundary through SQLite itself, rather than mocking transaction success.
    for table in [
        "occurrences",
        "logical_deliveries",
        "delivery_jobs",
        "planning_watermarks",
    ] {
        let dir = tempdir()?;
        let path = dir.path().join("db");
        let db = Storage::open(&path).await?;
        seed(&db).await?;
        commit(&db, 0, vec![event(json!({}))]).await?;
        // Audited: table is from the fixed list above; no external input.
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE TRIGGER fail_plan BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT,'injected planning failure'); END;"))).execute(db.pool()).await?;
        assert!(matches!(
            db.reconcile(request(1), &FakeClock::fixed()).await,
            Err(PlanningError::Database(_))
        ));
        db.close().await;
        let db = Storage::open(&path).await?;
        for affected in [
            "occurrences",
            "logical_deliveries",
            "delivery_jobs",
            "planning_watermarks",
        ] {
            assert_eq!(
                count(&db, affected).await?,
                0,
                "failure at {table}: partial {affected}"
            );
        }
        let source: (i64, String) =
            sqlx::query_as("SELECT revision,committed_checkpoint FROM sources WHERE id='s'")
                .fetch_one(db.pool())
                .await?;
        assert_eq!(source, (1, "revision-1".into()));
        assert_eq!(count(&db, "source_changes").await?, 1);
        sqlx::raw_sql("DROP TRIGGER fail_plan")
            .execute(db.pool())
            .await?;
        db.reconcile(request(1), &FakeClock::fixed()).await?;
        assert_eq!(jobs(&db).await?.len(), 1);
        db.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn ac4_reopen_before_planning_and_replay_after_commit_are_idempotent() -> Result {
    let dir = tempdir()?;
    let path = dir.path().join("db");
    let db = Storage::open(&path).await?;
    seed(&db).await?;
    commit(&db, 0, vec![event(json!({}))]).await?;
    db.close().await;
    let db = Storage::open(&path).await?;
    db.reconcile(request(1), &FakeClock::fixed()).await?;
    let expected = jobs(&db).await?;
    assert_eq!(expected.len(), 1);
    db.close().await;
    let db = Storage::open(&path).await?;
    db.reconcile(request(1), &FakeClock::fixed()).await?;
    commit(&db, 1, vec![event(json!({}))]).await?; // new source CAS, same event content
    db.reconcile(request(2), &FakeClock::fixed()).await?;
    assert_eq!(jobs(&db).await?, expected);
    assert_eq!(count(&db, "logical_deliveries").await?, 1);
    assert_eq!(count(&db, "delivery_jobs").await?, 1);
    assert_eq!(watermark(&db).await?.0, 2);
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac4_accepted_identity_is_not_reissued_after_rescheduling_and_reopen() -> Result {
    let dir = tempdir()?;
    let path = dir.path().join("db");
    let db = Storage::open(&path).await?;
    seed(&db).await?;
    commit(
        &db,
        0,
        vec![event(json!({})), event(json!({"external_uid":"b"}))],
    )
    .await?;
    db.reconcile(request(1), &FakeClock::fixed()).await?;
    // Accepted state is a fixture for #23; delivery/attempt processing belongs to #24.
    let accepted_id: String = sqlx::query_scalar("SELECT l.id FROM logical_deliveries l JOIN events e ON e.id=l.event_id WHERE e.external_uid='a'").fetch_one(db.pool()).await?;
    let mut tx = db.pool().begin().await?;
    sqlx::query("UPDATE logical_deliveries SET accepted_at=? WHERE id=?")
        .bind(NOW)
        .bind(&accepted_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE delivery_jobs SET state='accepted' WHERE logical_delivery_id=?")
        .bind(&accepted_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    commit(
        &db,
        1,
        vec![event(
            json!({"start":{"kind":"utc","value":"2026-11-02T18:00:00Z"}}),
        )],
    )
    .await?;
    db.close().await;
    let db = Storage::open(&path).await?;
    db.reconcile(request(2), &FakeClock::fixed()).await?;
    db.reconcile(request(2), &FakeClock::fixed()).await?;
    let accepted: (String, i64) =
        sqlx::query_as("SELECT id,accepted_at FROM logical_deliveries WHERE id=?")
            .bind(&accepted_id)
            .fetch_one(db.pool())
            .await?;
    assert_eq!(accepted, (accepted_id.clone(), NOW));
    let accepted_jobs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM delivery_jobs WHERE logical_delivery_id=?")
            .bind(&accepted_id)
            .fetch_one(db.pool())
            .await?;
    assert_eq!(
        accepted_jobs, 1,
        "accepted identity received a new generation"
    );
    let pending_uids: Vec<String> = sqlx::query_scalar("SELECT e.external_uid FROM delivery_jobs j JOIN logical_deliveries l ON l.id=j.logical_delivery_id JOIN events e ON e.id=l.event_id WHERE j.state IN ('pending','retry_wait','leased')").fetch_all(db.pool()).await?;
    assert_eq!(pending_uids, vec!["b"]);
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac4_concurrent_replay_leaves_one_consistent_plan() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    commit(&db, 0, vec![event(json!({}))]).await?;
    let clock = FakeClock::fixed();
    let (a, b) = tokio::join!(
        db.reconcile(request(1), &clock),
        db.reconcile(request(1), &clock)
    );
    assert!(a.is_ok() || b.is_ok());
    for result in [a, b] {
        if let Err(error) = result {
            assert!(
                matches!(error, PlanningError::Conflict),
                "unexpected competing result: {error}"
            );
        }
    }
    assert_eq!(count(&db, "logical_deliveries").await?, 1);
    assert_eq!(count(&db, "delivery_jobs").await?, 1);
    assert_eq!(jobs(&db).await?[0].6, DUE);
    assert_eq!(
        watermark(&db).await?,
        (1, 1, NOW + 30 * DAY, "2025b".into())
    );
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac2_iteration_limit_counts_excluded_and_duplicate_rdates() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    commit(&db, 0, vec![event(json!({"recurrence":{
        "rrule":"FREQ=DAILY;COUNT=1",
        "rdates":[{"kind":"utc","value":"2026-11-02T17:00:00Z"},{"kind":"utc","value":"2026-11-02T17:00:00Z"}],
        "exdates":[{"kind":"utc","value":"2026-11-02T17:00:00Z"}]
    }}))]).await?;
    let mut limited = request(1);
    limited.limits.max_iterations = 2;
    assert!(matches!(
        db.reconcile(limited, &FakeClock::fixed()).await,
        Err(PlanningError::IterationLimitExceeded)
    ));
    assert_eq!(count(&db, "planning_watermarks").await?, 0);
    let mut exact = request(1);
    exact.limits.max_iterations = 3;
    db.reconcile(exact, &FakeClock::fixed()).await?;
    assert!(jobs(&db).await?.is_empty());
    assert_eq!(watermark(&db).await?.0, 1);
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac1_missing_calendar_timezone_and_source_vtimezone_do_not_fall_back() -> Result {
    for (extra, policy) in [
        (
            json!({"start":{"kind":"floating","local":"2026-11-02T09:00:00"}}),
            "{}",
        ),
        (
            json!({"start":{"kind":"zoned","local":"2026-11-02T09:00:00","tzid":"America/Vancouver"},
            "timezones":[{"tzid":"America/Vancouver","vtimezone":"BEGIN:VTIMEZONE\r\nTZID:America/Vancouver\r\nEND:VTIMEZONE"}]}),
            "{\"tzid\":\"America/Vancouver\"}",
        ),
    ] {
        let dir = tempdir()?;
        let db = Storage::open(dir.path().join("db")).await?;
        seed(&db).await?;
        sqlx::query("UPDATE calendars SET timezone_policy=? WHERE id='c'")
            .bind(policy)
            .execute(db.pool())
            .await?;
        commit(&db, 0, vec![event(extra)]).await?;
        assert!(matches!(
            db.reconcile(request(1), &FakeClock::fixed()).await,
            Err(PlanningError::UnsupportedCapability(_))
        ));
        assert_eq!(count(&db, "delivery_jobs").await?, 0);
        assert_eq!(count(&db, "planning_watermarks").await?, 0);
        db.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn ac3_rule_and_output_generations_are_checked_before_publishing() -> Result {
    for table in ["reminder_rules", "outputs"] {
        let dir = tempdir()?;
        let db = Storage::open(dir.path().join("db")).await?;
        seed(&db).await?;
        commit(&db, 0, vec![event(json!({}))]).await?;
        sqlx::query(match table {
            "reminder_rules" => "UPDATE reminder_rules SET generation=2",
            "outputs" => "UPDATE outputs SET generation=2",
            _ => unreachable!(),
        })
        .execute(db.pool())
        .await?;
        assert!(matches!(
            db.reconcile(request(1), &FakeClock::fixed()).await,
            Err(PlanningError::Conflict)
        ));
        assert_eq!(count(&db, "delivery_jobs").await?, 0);
        assert_eq!(count(&db, "planning_watermarks").await?, 0);
        let mut current = request(1);
        if table == "reminder_rules" {
            current.rule_generation = 2;
        } else {
            current.output_generation = 2;
        }
        db.reconcile(current, &FakeClock::fixed()).await?;
        assert_eq!(jobs(&db).await?.len(), 1);
        db.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn ac4_terminal_jobs_are_not_reissued_without_a_planning_change() -> Result {
    for terminal in ["failed_permanent", "expired"] {
        let dir = tempdir()?;
        let path = dir.path().join("db");
        let db = Storage::open(&path).await?;
        seed(&db).await?;
        commit(&db, 0, vec![event(json!({}))]).await?;
        db.reconcile(request(1), &FakeClock::fixed()).await?;
        sqlx::query("UPDATE delivery_jobs SET state=?")
            .bind(terminal)
            .execute(db.pool())
            .await?;
        db.close().await;
        let db = Storage::open(&path).await?;
        db.reconcile(request(1), &FakeClock::fixed()).await?;
        commit(&db, 1, vec![event(json!({}))]).await?;
        db.reconcile(request(2), &FakeClock::fixed()).await?;
        assert!(jobs(&db).await?.is_empty(), "requeued {terminal}");
        let history: Vec<(i64, String)> =
            sqlx::query_as("SELECT generation,state FROM delivery_jobs ORDER BY generation")
                .fetch_all(db.pool())
                .await?;
        assert_eq!(history, vec![(1, terminal.into())]);
        // A real event change may create a new generation; a replay must not reset retries.
        commit(
            &db,
            2,
            vec![event(
                json!({"start":{"kind":"utc","value":"2026-11-02T18:00:00Z"}}),
            )],
        )
        .await?;
        db.reconcile(request(3), &FakeClock::fixed()).await?;
        let active = jobs(&db).await?;
        assert_eq!(active.len(), 1);
        assert_eq!(
            (active[0].4, active[0].5, active[0].6),
            (2, 2, DUE + 3_600_000)
        );
        db.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn ac3_scope_components_with_colons_keep_separate_legacy_watermarks() -> Result {
    let dir = tempdir()?;
    let path = dir.path().join("db");
    let db = Storage::open(&path).await?;
    seed(&db).await?;
    commit(&db, 0, vec![event(json!({}))]).await?;
    sqlx::raw_sql(
        "INSERT INTO sources(id,kind) VALUES ('s:r','ics');
         INSERT INTO reminder_rules(id,policy_json,generation,enabled)
           SELECT 'r:o',policy_json,1,1 FROM reminder_rules WHERE id='r';
         INSERT INTO reminder_rules(id,policy_json,generation,enabled)
           SELECT 'o',policy_json,1,1 FROM reminder_rules WHERE id='r';
         INSERT INTO rule_outputs(rule_id,output_id) VALUES ('r:o','o'),('o','o');",
    )
    .execute(db.pool())
    .await?;
    let mut first = request(1);
    first.rule_id = "r:o".into();
    db.reconcile(first.clone(), &FakeClock::fixed()).await?;
    // Existing databases can retain IDs created by the previous colon-joined format.
    sqlx::query("UPDATE planning_watermarks SET scope_id='s:r:o:o'")
        .execute(db.pool())
        .await?;
    db.close().await;
    let db = Storage::open(&path).await?;
    let mut second = request(0);
    second.source_id = "s:r".into();
    second.rule_id = "o".into();
    db.reconcile(second.clone(), &FakeClock::fixed()).await?;
    db.reconcile(first, &FakeClock::fixed()).await?;
    db.reconcile(second, &FakeClock::fixed()).await?;
    let scopes: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT source_id,rule_id,output_id,source_revision FROM planning_watermarks ORDER BY source_id"
    ).fetch_all(db.pool()).await?;
    assert_eq!(
        scopes,
        vec![
            ("s".into(), "r:o".into(), "o".into(), 1),
            ("s:r".into(), "o".into(), "o".into(), 0),
        ]
    );
    let legacy_id: String =
        sqlx::query_scalar("SELECT scope_id FROM planning_watermarks WHERE source_id='s'")
            .fetch_one(db.pool())
            .await?;
    assert_eq!(legacy_id, "s:r:o:o");
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac2_timezone_changes_cancel_jobs_that_leave_either_window_edge() -> Result {
    for (start, target_zone, now) in [
        ("2026-11-02T09:00:00", "UTC", NOW + 12 * 3_600_000),
        ("2026-12-01T09:00:00", "Pacific/Honolulu", NOW),
    ] {
        let dir = tempdir()?;
        let db = Storage::open(dir.path().join("db")).await?;
        seed(&db).await?;
        // Second case uses a shorter window so the timezone change moves past its right edge.
        let mut req = request(1);
        if now == NOW {
            req.horizon_ms = 29 * DAY + 18 * 3_600_000;
        }
        commit(
            &db,
            0,
            vec![event(json!({"start":{"kind":"floating","local":start}}))],
        )
        .await?;
        let clock = FakeClock::fixed();
        clock.utc.store(now, Ordering::SeqCst);
        db.reconcile(req.clone(), &clock).await?;
        assert_eq!(jobs(&db).await?.len(), 1);
        sqlx::query("UPDATE calendars SET timezone_policy=json_object('tzid',?) WHERE id='c'")
            .bind(target_zone)
            .execute(db.pool())
            .await?;
        db.reconcile(req, &clock).await?;
        assert!(
            jobs(&db).await?.is_empty(),
            "stale reminder after timezone change to {target_zone}"
        );
        let state: String = sqlx::query_scalar("SELECT state FROM delivery_jobs")
            .fetch_one(db.pool())
            .await?;
        assert_eq!(state, "cancelled");
        db.close().await;
    }
    Ok(())
}

struct ExhaustingClock {
    reads: AtomicU64,
    exhaust_at: u64,
}
impl Clock for ExhaustingClock {
    fn utc_now_ms(&self) -> i64 {
        NOW
    }
    fn monotonic_now_ms(&self) -> u64 {
        if self.reads.fetch_add(1, Ordering::SeqCst) >= self.exhaust_at {
            1_000
        } else {
            0
        }
    }
}

#[tokio::test]
async fn ac2_reconciliation_budget_rolls_back_loading_persistence_and_watermark() -> Result {
    // Move the deterministic deadline across successive checkpoints, including the final
    // persistence checkpoint. The clock records whether its deadline was actually observed.
    let mut completed = false;
    for exhaust_at in 1..80 {
        let dir = tempdir()?;
        let db = Storage::open(dir.path().join("db")).await?;
        seed(&db).await?;
        commit(
            &db,
            0,
            vec![event(json!({})), event(json!({"external_uid":"b"}))],
        )
        .await?;
        let clock = ExhaustingClock {
            reads: AtomicU64::new(0),
            exhaust_at,
        };
        let result = db.reconcile(request(1), &clock).await;
        let reads = clock.reads.load(Ordering::SeqCst);
        if reads > exhaust_at && exhaust_at > 0 {
            assert!(matches!(result,Err(PlanningError::TimeBudgetExceeded)),"deadline observed at checkpoint {exhaust_at} but reconciliation returned {result:?}");
            for table in [
                "occurrences",
                "logical_deliveries",
                "delivery_jobs",
                "planning_watermarks",
            ] {
                assert_eq!(
                    count(&db, table).await?,
                    0,
                    "partial {table} at checkpoint {exhaust_at}"
                );
            }
        } else if result.is_ok() {
            completed = true;
            assert_eq!(jobs(&db).await?.len(), 2);
            assert_eq!(watermark(&db).await?.0, 1);
            db.close().await;
            break;
        }
        db.close().await;
    }
    assert!(completed, "generous budget never completed");
    Ok(())
}

struct DatabaseClock(std::sync::Arc<AtomicU64>);
impl Clock for DatabaseClock {
    fn utc_now_ms(&self) -> i64 {
        NOW
    }
    fn monotonic_now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[tokio::test]
async fn ac2_budget_expiring_during_sql_writes_rolls_back_even_the_watermark() -> Result {
    for table in [
        "occurrences",
        "logical_deliveries",
        "delivery_jobs",
        "planning_watermarks",
    ] {
        let dir = tempdir()?;
        let path = dir.path().join("db");
        let db = Storage::open(&path).await?;
        seed(&db).await?;
        commit(&db, 0, vec![event(json!({}))]).await?;
        let elapsed = std::sync::Arc::new(AtomicU64::new(0));
        // Register on every connection. A real SQLite trigger advances the injected clock
        // during persistence, after pure calculation has completed. No wall-clock sleeps.
        let mut connections = Vec::new();
        for _ in 0..4 {
            let mut connection = db.pool().acquire().await?;
            let elapsed = elapsed.clone();
            connection
                .lock_handle()
                .await?
                .create_collation("exhaust_budget", move |a, b| {
                    elapsed.store(1_000, Ordering::SeqCst);
                    a.cmp(b)
                })?;
            connections.push(connection);
        }
        drop(connections);
        // Audited: table is from the fixed list above; no external input.
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE TRIGGER exhaust_budget AFTER INSERT ON {table} BEGIN SELECT 'a' COLLATE exhaust_budget = 'b'; END;")))
            .execute(db.pool()).await?;
        let result = db
            .reconcile(request(1), &DatabaseClock(elapsed.clone()))
            .await;
        assert_eq!(
            elapsed.load(Ordering::SeqCst),
            1_000,
            "clock trigger did not execute at {table}"
        );
        assert!(
            matches!(result, Err(PlanningError::TimeBudgetExceeded)),
            "expired during {table}: {result:?}"
        );
        db.close().await;
        let db = Storage::open(&path).await?;
        for affected in [
            "occurrences",
            "logical_deliveries",
            "delivery_jobs",
            "planning_watermarks",
        ] {
            assert_eq!(
                count(&db, affected).await?,
                0,
                "partial {affected} after expiry at {table}"
            );
        }
        assert_eq!(count(&db, "source_changes").await?, 1);
        db.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn ac2_job_timezone_context_is_independent_of_other_outputs_and_window_movement() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    sqlx::raw_sql("INSERT INTO outputs(id,kind) VALUES ('p','bark'); INSERT INTO rule_outputs(rule_id,output_id) VALUES ('r','p');")
        .execute(db.pool()).await?;
    commit(
        &db,
        0,
        vec![event(
            json!({"start":{"kind":"floating","local":"2026-11-02T09:00:00"}}),
        )],
    )
    .await?;
    let mut other = request(1);
    other.output_id = "p".into();
    let clock = FakeClock::fixed();
    db.reconcile(request(1), &clock).await?;
    db.reconcile(other.clone(), &clock).await?;
    // Ordinary time passing must preserve valid pending reminders for scheduler catch-up.
    clock.utc.store(START + 3_600_000, Ordering::SeqCst);
    db.reconcile(other.clone(), &clock).await?;
    assert_eq!(jobs(&db).await?.len(), 2);
    // Another output refreshes the shared occurrence in the new timezone first.
    clock.utc.store(NOW, Ordering::SeqCst);
    sqlx::query("UPDATE calendars SET timezone_policy='{\"tzid\":\"UTC\"}' WHERE id='c'")
        .execute(db.pool())
        .await?;
    db.reconcile(request(1), &clock).await?;
    clock.utc.store(NOW + 12 * 3_600_000, Ordering::SeqCst);
    db.reconcile(other, &clock).await?;
    let state:String=sqlx::query_scalar("SELECT j.state FROM delivery_jobs j JOIN logical_deliveries l ON l.id=j.logical_delivery_id WHERE l.output_id='p'")
        .fetch_one(db.pool()).await?;
    assert_eq!(state, "cancelled");
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn configuration_rename_replans_unaccepted_jobs_and_calendar_selection_is_enforced() -> Result
{
    use calendar_application::configuration::{ConfigurationCondition, SourceConfigurationV1};
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    commit(&db, 0, vec![event(json!({}))]).await?;
    db.reconcile(request(1), &FakeClock::fixed()).await?;
    let original = jobs(&db).await?;
    assert_eq!(original.len(), 1);
    let mut config: SourceConfigurationV1 = serde_json::from_str(include_str!(
        "../../../examples/source-configuration-v1.json"
    ))?;
    config.enabled = true;
    config.calendars[0].id = "c".into();
    config.calendars[0].timezone = Some("America/Vancouver".into());
    sqlx::query("UPDATE sources SET config_revision=1,config_json=? WHERE id='s'")
        .bind(serde_json::to_string(&config)?)
        .execute(db.pool())
        .await?;
    config.name = "Renamed only".into();
    db.put_source_configuration(
        "s",
        ConfigurationCondition::Revision(1),
        config.clone(),
        "alice",
        NOW,
    )
    .await?;
    assert!(jobs(&db).await?.is_empty());
    assert!(matches!(
        db.reconcile(request(1), &FakeClock::fixed()).await,
        Err(PlanningError::Conflict)
    ));
    db.reconcile(request(2), &FakeClock::fixed()).await?;
    let replanned = jobs(&db).await?;
    assert_eq!(replanned.len(), 1);
    assert_eq!(replanned[0].4, original[0].4 + 1);
    assert_eq!(replanned[0].6, original[0].6);
    config.calendars[0].selected = false;
    db.put_source_configuration(
        "s",
        ConfigurationCondition::Revision(2),
        config,
        "alice",
        NOW + 1,
    )
    .await?;
    db.reconcile(request(3), &FakeClock::fixed()).await?;
    assert!(jobs(&db).await?.is_empty());
    assert_eq!(count(&db, "logical_deliveries").await?, 1); // Identity/history retained.
    db.close().await;
    Ok(())
}
