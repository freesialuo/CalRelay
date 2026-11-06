//! #125/#140: current occurrence text survives sync, planning and the final delivery fence.
use calendar_application::{
    Clock, DeliveryOutcome, DeliveryPolicy, DeliveryRepository, EventRepository, EventV1,
    PlanningLimits, PlanningRequest, ReminderPlanner, SyncBatch, SyncMode, SyncRecord,
};
use calendar_storage::Storage;
use serde_json::{json, Value};
use tempfile::tempdir;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
const NOW: i64 = 1_793_577_600_000; // 2026-11-02T00:00:00Z
const DUE: i64 = NOW + 16 * 3_600_000 + 45 * 60_000;
const DAY: i64 = 86_400_000;
const ORIGINAL_KEY: &str = "utc:2026-11-02T17:00:00Z";
struct Time(i64);
impl Clock for Time {
    fn utc_now_ms(&self) -> i64 {
        self.0
    }
    fn monotonic_now_ms(&self) -> u64 {
        0
    }
}
fn event(patch: Value) -> EventV1 {
    serde_json::from_value(json!({
        "schema_version":"1.0", "external_uid":"series", "title":"Master title",
        "description":"Master body", "status":"confirmed",
        "start":{"kind":"utc","value":"2026-11-02T17:00:00Z"}, "duration":"PT1H",
        "recurrence":{"rrule":"FREQ=DAILY;COUNT=2","exceptions":[{
            "recurrence_id":{"kind":"utc","value":"2026-11-02T17:00:00Z"},
            "range":"single", "change":{"kind":"override","patch":patch}
        }]}
    }))
    .unwrap()
}
async fn seed(db: &Storage) -> Result {
    sqlx::raw_sql(
        "INSERT INTO sources(id,kind) VALUES ('s','ics');
         INSERT INTO calendars(id,source_id,external_id) VALUES ('c','s','main');
         INSERT INTO outputs(id,kind) VALUES ('o','bark');
         INSERT INTO reminder_rules(id,policy_json) VALUES ('r',
           '{\"calendar_ids\":[\"c\"],\"lead_ms\":900000,\"all_day_time\":\"09:00\"}');
         INSERT INTO rule_outputs(rule_id,output_id) VALUES ('r','o');",
    )
    .execute(db.pool())
    .await?;
    Ok(())
}
async fn sync(db: &Storage, base_revision: i64, event: EventV1) -> Result {
    db.commit_sync(SyncBatch {
        source_id: "s".into(),
        base_revision,
        mode: SyncMode::Delta,
        records: vec![SyncRecord::Upsert {
            calendar_id: "c".into(),
            event: Box::new(event),
        }],
        proposed_checkpoint: None,
        verified_at: NOW,
    })
    .await?;
    Ok(())
}
async fn plan(db: &Storage, base_revision: i64) -> Result {
    db.reconcile(
        PlanningRequest {
            source_id: "s".into(),
            base_revision,
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
        },
        &Time(NOW),
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn prepare_uses_occurrence_text_with_inheritance_and_explicit_clear() -> Result {
    for (patch, expected_title, expected_body) in [
        (
            json!({"title":"Changed title","description":"Changed body"}),
            "Changed title",
            Some("Changed body"),
        ),
        (
            json!({"title":"Changed title"}),
            "Changed title",
            Some("Master body"),
        ),
        (
            json!({"title":"Changed title","description":null}),
            "Changed title",
            None,
        ),
        (
            json!({"description":"Changed body"}),
            "Master title",
            Some("Changed body"),
        ),
    ] {
        let dir = tempdir()?;
        let db = Storage::open(dir.path().join("db")).await?;
        seed(&db).await?;
        sync(&db, 0, event(patch.clone())).await?;
        plan(&db, 1).await?;
        let policy = DeliveryPolicy {
            source_max_age_ms: 3 * DAY,
            ..Default::default()
        };
        for (due, key, title, body) in [
            (DUE, ORIGINAL_KEY, expected_title, expected_body),
            (
                DUE + DAY,
                "utc:2026-11-03T17:00:00Z",
                "Master title",
                Some("Master body"),
            ),
        ] {
            let lease = db.claim("content", &policy, &Time(due)).await?.unwrap();
            let envelope = db.prepare(&lease, &policy, &Time(due)).await?.unwrap();
            assert_eq!(envelope.original_key, key);
            assert_eq!(envelope.event.title, title, "patch {patch}");
            assert_eq!(envelope.event.description.as_deref(), body, "patch {patch}");
            db.finish(&lease, DeliveryOutcome::Accepted, &policy, &Time(due))
                .await?;
        }
        db.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn rescheduled_text_keeps_original_identity_and_accepted_deduplication() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    sync(
        &db,
        0,
        event(json!({
            "title":"Moved title", "description":null,
            "start":{"kind":"utc","value":"2026-11-02T18:00:00Z"}
        })),
    )
    .await?;
    plan(&db, 1).await?;
    let policy = DeliveryPolicy::default();
    let due = DUE + 3_600_000;
    assert!(db.claim("content", &policy, &Time(DUE)).await?.is_none());
    let lease = db.claim("content", &policy, &Time(due)).await?.unwrap();
    let envelope = db.prepare(&lease, &policy, &Time(due)).await?.unwrap();
    assert_eq!(envelope.original_key, ORIGINAL_KEY);
    assert_eq!(envelope.due_at, due);
    assert_eq!(
        (envelope.event.title.as_str(), envelope.event.description),
        ("Moved title", None)
    );
    db.finish(&lease, DeliveryOutcome::Accepted, &policy, &Time(due))
        .await?;
    sync(
        &db,
        1,
        event(json!({
            "title":"Moved again", "description":"New body",
            "start":{"kind":"utc","value":"2026-11-02T19:00:00Z"}
        })),
    )
    .await?;
    plan(&db, 2).await?;
    assert!(db
        .claim("content", &policy, &Time(due + 3_600_000))
        .await?
        .is_none());
    let accepted: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM logical_deliveries WHERE original_key=? AND accepted_at IS NOT NULL",
    )
    .bind(ORIGINAL_KEY)
    .fetch_one(db.pool())
    .await?;
    assert_eq!(accepted, 1);
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn occurrence_text_does_not_bypass_revision_or_cancellation_fences() -> Result {
    for cancelled in [false, true] {
        let dir = tempdir()?;
        let db = Storage::open(dir.path().join("db")).await?;
        seed(&db).await?;
        sync(&db, 0, event(json!({"title":"Old override"}))).await?;
        plan(&db, 1).await?;
        let policy = DeliveryPolicy::default();
        let lease = db.claim("content", &policy, &Time(DUE)).await?.unwrap();
        let mut updated = serde_json::to_value(event(
            json!({"title":"Current override","description":null}),
        ))?;
        if cancelled {
            updated["recurrence"]["exceptions"][0]["change"] = json!({"kind":"cancelled"});
        }
        sync(&db, 1, serde_json::from_value(updated)?).await?;
        assert!(db.prepare(&lease, &policy, &Time(DUE)).await?.is_none());
        plan(&db, 2).await?;
        let next = db.claim("content", &policy, &Time(DUE)).await?;
        if cancelled {
            assert!(next.is_none());
        } else {
            let envelope = db
                .prepare(&next.unwrap(), &policy, &Time(DUE))
                .await?
                .unwrap();
            assert_eq!(envelope.event.title, "Current override");
            assert_eq!(envelope.event.description, None);
        }
        db.close().await;
    }
    Ok(())
}
