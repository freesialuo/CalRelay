//! Issue #126: a revision must not drop an unsent reminder that is still inside the
//! delivery catch-up window, and must not extend or revive reminders beyond it.
//! Contract: docs/delivery-recovery.md (deadline = due_at + catch_up_ms, equality expires).

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use calendar_application::{
    Clock, DeliveryOutcome, DeliveryPolicy, DeliveryRepository, EventRepository, EventV1,
    PlanningLimits, PlanningRequest, ReminderPlanner, SyncBatch, SyncMode, SyncRecord,
};
use calendar_storage::Storage;
use serde_json::{json, Value};
use tempfile::tempdir;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
const DAY_START: i64 = 1_793_577_600_000; // 2026-11-02T00:00:00Z
const DUE: i64 = 1_793_638_800_000; // 2026-11-02T17:00:00Z, lead 0
const MINUTE: i64 = 60_000;
const CATCH_UP: i64 = 15 * MINUTE;

struct Time(AtomicI64, AtomicU64);
impl Time {
    fn at(ms: i64) -> Self {
        Self(AtomicI64::new(ms), AtomicU64::new(0))
    }
    fn set(&self, ms: i64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}
impl Clock for Time {
    fn utc_now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn monotonic_now_ms(&self) -> u64 {
        self.1.load(Ordering::SeqCst)
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
        horizon_ms: 30 * 86_400_000,
        max_lead_ms: 86_400_000,
        extension_interval_ms: 300_000,
        catch_up_ms: CATCH_UP,
        tzdata_version: "2025b".into(),
        limits: PlanningLimits {
            max_occurrences: 100,
            max_iterations: 1_000,
            max_elapsed_ms: 1_000,
        },
    }
}

fn policy() -> DeliveryPolicy {
    DeliveryPolicy {
        catch_up_ms: CATCH_UP,
        ..DeliveryPolicy::default()
    }
}

fn event(title: &str, extra: Value) -> EventV1 {
    let mut doc = json!({
        "schema_version": "1.0", "external_uid": "a", "title": title,
        "status": "confirmed",
        "start": {"kind": "utc", "value": "2026-11-02T17:00:00Z"}
    });
    for (k, v) in extra.as_object().expect("object") {
        doc[k] = v.clone();
    }
    serde_json::from_value(doc).expect("valid EventV1")
}

async fn open() -> Result<(tempfile::TempDir, Storage)> {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    sqlx::raw_sql(
        "INSERT INTO sources(id,kind) VALUES ('s','ics');
         INSERT INTO calendars(id,source_id,external_id,timezone_policy)
           VALUES ('c','s','main','{\"tzid\":null}');
         INSERT INTO reminder_rules(id,policy_json,generation,enabled)
           VALUES ('r','{\"calendar_ids\":[\"c\"],\"lead_ms\":0,\"all_day_time\":\"09:00\"}',1,1);
         INSERT INTO outputs(id,kind,config_json,generation,enabled) VALUES ('o','bark','{}',1,1);
         INSERT INTO rule_outputs(rule_id,output_id) VALUES ('r','o');",
    )
    .execute(db.pool())
    .await?;
    Ok((dir, db))
}

async fn sync(db: &Storage, base: i64, record: SyncRecord) -> Result {
    db.commit_sync(SyncBatch {
        source_id: "s".into(),
        base_revision: base,
        mode: SyncMode::Delta,
        records: vec![record],
        proposed_checkpoint: None,
        verified_at: DAY_START,
    })
    .await?;
    Ok(())
}

fn upsert(event: EventV1) -> SyncRecord {
    SyncRecord::Upsert {
        calendar_id: "c".into(),
        event: Box::new(event),
    }
}

/// Plans `first` before it is due, then revises it to `second` at `revised_at`.
async fn plan_then_revise(
    db: &Storage,
    time: &Time,
    first: EventV1,
    second: SyncRecord,
    revised_at: i64,
) -> Result {
    time.set(DUE - MINUTE);
    sync(db, 0, upsert(first)).await?;
    db.reconcile(request(1), time).await?;
    time.set(revised_at);
    sync(db, 1, second).await?;
    db.reconcile(request(2), time).await?;
    Ok(())
}

/// Claims and prepares the next due job, returning the delivered title.
async fn deliver_title(db: &Storage, time: &Time) -> Result<Option<String>> {
    let p = policy();
    let Some(lease) = db.claim("w", &p, time).await? else {
        return Ok(None);
    };
    let envelope = db.prepare(&lease, &p, time).await?;
    Ok(envelope.map(|e| e.event.title))
}

#[tokio::test]
async fn ac1_ac3_title_revision_keeps_ended_reminders_deliverable() -> Result {
    for (extra, revised_at) in [
        (json!({}), DUE + MINUTE),                       // zero duration
        (json!({"duration": "PT1M"}), DUE + 2 * MINUTE), // already ended
        (json!({}), DUE + CATCH_UP - 1),                 // last deliverable instant
    ] {
        let (_dir, db) = open().await?;
        let time = Time::at(0);
        let revised = event("Changed", extra.clone());
        plan_then_revise(
            &db,
            &time,
            event("Master", extra),
            upsert(revised),
            revised_at,
        )
        .await?;
        assert_eq!(deliver_title(&db, &time).await?, Some("Changed".into()));
        db.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn ac3_retry_wait_revision_keeps_deadline_and_accepted_is_not_resent() -> Result {
    let (_dir, db) = open().await?;
    let time = Time::at(DUE - MINUTE);
    let p = policy();
    sync(&db, 0, upsert(event("Master", json!({})))).await?;
    db.reconcile(request(1), &time).await?;

    time.set(DUE);
    let lease = db.claim("w", &p, &time).await?.expect("due job");
    db.prepare(&lease, &p, &time).await?.expect("sendable");
    let retry = DeliveryOutcome::Retryable {
        retry_after_ms: None,
    };
    db.finish(&lease, retry, &p, &time).await?;

    time.set(DUE + MINUTE);
    sync(&db, 1, upsert(event("Changed", json!({})))).await?;
    db.reconcile(request(2), &time).await?;
    let deadlines: Vec<(i64, String, Option<i64>)> = sqlx::query_as(
        "SELECT generation,state,deadline_at FROM delivery_jobs ORDER BY generation",
    )
    .fetch_all(db.pool())
    .await?;
    assert_eq!(
        deadlines,
        vec![
            (1, "superseded".into(), Some(DUE + CATCH_UP)),
            (2, "pending".into(), Some(DUE + CATCH_UP)),
        ]
    );

    time.set(DUE + 2 * MINUTE);
    let lease = db.claim("w", &p, &time).await?.expect("revised job");
    let envelope = db.prepare(&lease, &p, &time).await?.expect("sendable");
    assert_eq!(envelope.event.title, "Changed");
    db.finish(&lease, DeliveryOutcome::Accepted, &p, &time)
        .await?;

    sync(&db, 2, upsert(event("Again", json!({})))).await?;
    db.reconcile(request(3), &time).await?;
    assert_eq!(deliver_title(&db, &time).await?, None);
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac2_cancel_delete_scope_and_deadline_still_block_delivery() -> Result {
    let cancelled = upsert(event("Master", json!({"status": "cancelled"})));
    let deleted = SyncRecord::Delete {
        external_uid: "a".into(),
    };
    let changed = |extra| upsert(event("Changed", extra));
    for (second, revised_at, deselect) in [
        (cancelled, DUE + MINUTE, false),
        (deleted, DUE + MINUTE, false),
        (changed(json!({})), DUE + MINUTE, true),
        (changed(json!({})), DUE + CATCH_UP, false), // deadline equality expires
        (
            changed(json!({"duration": "PT1H"})),
            DUE + CATCH_UP + MINUTE,
            false,
        ), // still ongoing
    ] {
        let (_dir, db) = open().await?;
        let time = Time::at(0);
        if deselect {
            time.set(DUE - MINUTE);
            sync(&db, 0, upsert(event("Master", json!({})))).await?;
            db.reconcile(request(1), &time).await?;
            time.set(revised_at);
            sync(&db, 1, second).await?;
            sqlx::query("UPDATE calendars SET selected=0")
                .execute(db.pool())
                .await?;
            db.reconcile(request(2), &time).await?;
        } else {
            plan_then_revise(&db, &time, event("Master", json!({})), second, revised_at).await?;
        }
        let live: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM delivery_jobs WHERE state IN ('pending','retry_wait','leased')",
        )
        .fetch_one(db.pool())
        .await?;
        assert_eq!(live, 0, "no sendable job may survive or be created");
        assert_eq!(deliver_title(&db, &time).await?, None);
        db.close().await;
    }
    Ok(())
}
