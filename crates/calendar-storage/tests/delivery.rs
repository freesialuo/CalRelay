//! #24: real SQLite lease, retry and recovery acceptance tests.
use calendar_storage::Storage;
use tempfile::tempdir;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[tokio::test]
async fn ac3_attempt_history_is_persistent() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='delivery_attempts')").fetch_one(db.pool()).await?;
    assert!(
        exists,
        "delivery attempts must be durable, not an in-memory counter"
    );
    db.close().await;
    Ok(())
}

use calendar_application::{
    deliver_one, Clock, DeliveryEnvelope, DeliveryError, DeliveryOutcome as Outcome,
    DeliveryPolicy, DeliveryRepository, PushOutput,
};
use std::sync::atomic::{AtomicI64, Ordering};
struct Time(AtomicI64);
impl Time {
    fn at(now: i64) -> Self {
        Self(AtomicI64::new(now))
    }
    fn set(&self, now: i64) {
        self.0.store(now, Ordering::SeqCst);
    }
}
impl Clock for Time {
    fn utc_now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn monotonic_now_ms(&self) -> u64 {
        0
    }
}
fn policy() -> DeliveryPolicy {
    DeliveryPolicy {
        lease_ms: 1000,
        request_timeout_ms: 100,
        catch_up_ms: 10_000,
        base_backoff_ms: 100,
        max_backoff_ms: 1000,
        ..Default::default()
    }
}
async fn seed(db: &Storage) -> Result {
    sqlx::raw_sql(r#"
    INSERT INTO sources(id,kind,revision,last_verified_at) VALUES ('s','test',1,1000);
    INSERT INTO calendars(id,source_id,external_id) VALUES ('c','s','main');
    INSERT INTO events(id,source_id,calendar_id,external_uid,revision,document_json,content_hash)
      VALUES ('e','s','c','a',1,'{"schema_version":"1.0","external_uid":"a","title":"Test","status":"confirmed","start":{"kind":"utc","value":"2026-11-02T17:00:00Z"},"duration":"PT1H"}','h');
    INSERT INTO outputs(id,kind) VALUES ('o','test');
    INSERT INTO reminder_rules(id,policy_json) VALUES ('r','{"calendar_ids":["c"]}');
    INSERT INTO rule_outputs VALUES ('r','o');
    INSERT INTO logical_deliveries(id,event_id,original_key,rule_id,output_id) VALUES ('l','e','single','r','o');
    INSERT INTO delivery_jobs(id,logical_delivery_id,generation,event_revision,rule_revision,output_revision,due_at,state,next_attempt_at,planning_context)
      VALUES ('j','l',1,1,1,1,1000,'pending',1000,'[null,"2025b"]');
    INSERT INTO planning_watermarks(scope_id,source_id,rule_id,output_id,source_revision,rule_revision,output_revision,planned_until,tzdata_version)
      VALUES ('w','s','r','o',1,1,1,20000,'2025b');
    "#).execute(db.pool()).await?;
    Ok(())
}
async fn state(db: &Storage) -> Result<String> {
    Ok(
        sqlx::query_scalar("SELECT state FROM delivery_jobs WHERE id='j'")
            .fetch_one(db.pool())
            .await?,
    )
}
async fn history(db: &Storage) -> Result<Vec<(i64, Option<String>, Option<String>)>> {
    Ok(sqlx::query_as(
        "SELECT attempt_no,outcome,disposition FROM delivery_attempts ORDER BY attempt_no",
    )
    .fetch_all(db.pool())
    .await?)
}

#[tokio::test]
async fn ac1_concurrent_claim_has_one_owner_and_rejects_wrong_token() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    let time = Time::at(1000);
    let p = policy();
    let (a, b) = tokio::join!(db.claim("a", &p, &time), db.claim("b", &p, &time));
    let (a, b) = (a?, b?);
    assert_ne!(a.is_some(), b.is_some());
    let lease = a.or(b).unwrap();
    let mut wrong = lease.clone();
    wrong.token = "wrong".into();
    assert!(matches!(
        db.prepare(&wrong, &p, &time).await,
        Err(DeliveryError::StaleLease)
    ));
    let mut wrong_owner = lease.clone();
    wrong_owner.owner = "other".into();
    assert!(matches!(
        db.prepare(&wrong_owner, &p, &time).await,
        Err(DeliveryError::StaleLease)
    ));
    let envelope = db.prepare(&lease, &p, &time).await?.unwrap();
    assert_eq!(envelope.original_key, "single");
    assert!(matches!(
        db.prepare(&lease, &p, &time).await,
        Err(DeliveryError::StaleLease)
    ));
    db.finish(&lease, Outcome::Accepted, &p, &time).await?;
    assert_eq!(state(&db).await?, "accepted");
    assert_eq!(
        history(&db).await?,
        vec![(1, Some("accepted".into()), Some("accepted".into()))]
    );
    assert!(db.claim("c", &p, &time).await?.is_none());
    assert!(matches!(
        db.finish(&lease, Outcome::Accepted, &p, &time).await,
        Err(DeliveryError::StaleLease)
    ));
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac1_expiry_rejects_old_token_and_recovers_unknown_outcome() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    let time = Time::at(1000);
    let p = policy();
    let old = db.claim("a", &p, &time).await?.unwrap();
    db.prepare(&old, &p, &time).await?;
    time.set(2000);
    assert!(matches!(
        db.finish(&old, Outcome::Accepted, &p, &time).await,
        Err(DeliveryError::StaleLease)
    ));
    assert!(db.claim("b", &p, &time).await?.is_none());
    assert_eq!(
        history(&db).await?,
        vec![(1, Some("unknown".into()), Some("retry_wait".into()))]
    );
    time.set(2100);
    let new = db.claim("b", &p, &time).await?.unwrap();
    assert_ne!(old.token, new.token);
    assert!(matches!(
        db.prepare(&old, &p, &time).await,
        Err(DeliveryError::StaleLease)
    ));
    db.prepare(&new, &p, &time).await?;
    db.finish(&new, Outcome::Accepted, &p, &time).await?;
    assert_eq!(state(&db).await?, "accepted");
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac2_retry_after_deadline_and_attempt_limit() -> Result {
    for (outcome, max_attempts, expected) in [
        (
            Outcome::Retryable {
                retry_after_ms: Some(500),
            },
            8,
            "retry_wait",
        ),
        (
            Outcome::Unknown {
                retry_after_ms: Some(10_000),
            },
            8,
            "expired",
        ),
        (
            Outcome::Retryable {
                retry_after_ms: None,
            },
            1,
            "failed_permanent",
        ),
        (Outcome::PermanentFailure, 8, "failed_permanent"),
    ] {
        let dir = tempdir()?;
        let db = Storage::open(dir.path().join("db")).await?;
        seed(&db).await?;
        let time = Time::at(1000);
        let mut p = policy();
        p.max_attempts = max_attempts;
        let lease = db.claim("a", &p, &time).await?.unwrap();
        db.prepare(&lease, &p, &time).await?;
        db.finish(&lease, outcome, &p, &time).await?;
        assert_eq!(state(&db).await?, expected);
        if expected == "retry_wait" {
            time.set(1499);
            assert!(db.claim("a", &p, &time).await?.is_none());
            time.set(1500);
            assert!(db.claim("a", &p, &time).await?.is_some());
        }
        db.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn ac2_final_check_blocks_cancellation_revision_and_stale_planning() -> Result {
    for mutation in [
        "UPDATE events SET deleted_at=1001",
        "UPDATE events SET revision=2",
        "UPDATE sources SET enabled=0",
        "UPDATE reminder_rules SET generation=2",
        "UPDATE outputs SET generation=2",
        "UPDATE sources SET revision=2",
        "UPDATE sources SET last_verified_at=NULL",
        "UPDATE delivery_jobs SET state='cancelled'",
    ] {
        let dir = tempdir()?;
        let db = Storage::open(dir.path().join("db")).await?;
        seed(&db).await?;
        let time = Time::at(1000);
        let p = policy();
        let lease = db.claim("a", &p, &time).await?.unwrap();
        sqlx::query(mutation).execute(db.pool()).await?;
        assert!(db.prepare(&lease, &p, &time).await?.is_none(), "{mutation}");
        assert_eq!(history(&db).await?[0].1.as_deref(), Some("not_sent"));
        db.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn ac3_cancelled_inflight_acceptance_is_audited_without_reviving_job() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    let time = Time::at(1000);
    let p = policy();
    let lease = db.claim("a", &p, &time).await?.unwrap();
    db.prepare(&lease, &p, &time).await?;
    sqlx::query("UPDATE delivery_jobs SET state='cancelled'")
        .execute(db.pool())
        .await?;
    db.finish(&lease, Outcome::Accepted, &p, &time).await?;
    assert_eq!(state(&db).await?, "cancelled");
    assert_eq!(
        history(&db).await?,
        vec![(1, Some("accepted".into()), Some("cancelled".into()))]
    );
    let accepted: Option<i64> = sqlx::query_scalar("SELECT accepted_at FROM logical_deliveries")
        .fetch_one(db.pool())
        .await?;
    assert_eq!(accepted, Some(1000));
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac3_accepted_attempt_job_and_dedup_roll_back_together() -> Result {
    let dir = tempdir()?;
    let path = dir.path().join("db");
    let db = Storage::open(&path).await?;
    seed(&db).await?;
    let time = Time::at(1000);
    let p = policy();
    let lease = db.claim("a", &p, &time).await?.unwrap();
    db.prepare(&lease, &p, &time).await?;
    sqlx::raw_sql("CREATE TRIGGER fail_accept BEFORE UPDATE OF accepted_at ON logical_deliveries BEGIN SELECT RAISE(ABORT,'injected'); END;").execute(db.pool()).await?;
    assert_eq!(
        db.finish(&lease, Outcome::Accepted, &p, &time).await,
        Err(DeliveryError::Database)
    );
    db.close().await;
    let db = Storage::open(&path).await?;
    assert_eq!(state(&db).await?, "leased");
    assert_eq!(history(&db).await?, vec![(1, None, None)]);
    let accepted: Option<i64> = sqlx::query_scalar("SELECT accepted_at FROM logical_deliveries")
        .fetch_one(db.pool())
        .await?;
    assert_eq!(accepted, None);
    sqlx::query("DROP TRIGGER fail_accept")
        .execute(db.pool())
        .await?;
    db.finish(&lease, Outcome::Accepted, &p, &time).await?;
    db.close().await;
    Ok(())
}

struct FakeOutput<'a>(&'a Storage);
impl PushOutput for FakeOutput<'_> {
    async fn deliver(&self, envelope: DeliveryEnvelope) -> Outcome {
        assert_eq!(envelope.event.external_uid, "a");
        // A write during I/O proves the worker has released its transaction.
        sqlx::query("UPDATE sources SET last_verified_at=1000")
            .execute(self.0.pool())
            .await
            .unwrap();
        Outcome::Accepted
    }
}
struct HangingOutput;
impl PushOutput for HangingOutput {
    async fn deliver(&self, _: DeliveryEnvelope) -> Outcome {
        std::future::pending().await
    }
}
#[tokio::test]
async fn ac4_worker_uses_mock_output_outside_transaction_and_times_out_unknown() -> Result {
    for hang in [false, true] {
        let dir = tempdir()?;
        let db = Storage::open(dir.path().join("db")).await?;
        seed(&db).await?;
        let time = Time::at(1000);
        let mut p = policy();
        p.request_timeout_ms = 5;
        if hang {
            assert!(deliver_one(&db, &HangingOutput, "a", &p, &time).await?)
        } else {
            assert!(deliver_one(&db, &FakeOutput(&db), "a", &p, &time).await?)
        }
        assert_eq!(
            state(&db).await?,
            if hang { "retry_wait" } else { "accepted" }
        );
        assert_eq!(
            history(&db).await?[0].1.as_deref(),
            Some(if hang { "unknown" } else { "accepted" })
        );
        db.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn ac4_clock_jumps_never_send_future_or_expired_jobs() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    let time = Time::at(0);
    let p = policy();
    assert!(db.claim("a", &p, &time).await?.is_none());
    time.set(11_000);
    assert!(db.claim("a", &p, &time).await?.is_none());
    assert_eq!(state(&db).await?, "expired");
    time.set(1000);
    assert!(db.claim("a", &p, &time).await?.is_none());
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac4_busy_returns_error_without_partial_lease() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    // Set each worker connection to a short timeout; hold a competing real SQLite writer.
    let mut connections = Vec::new();
    for _ in 0..4 {
        let mut c = db.pool().acquire().await?;
        sqlx::query("PRAGMA busy_timeout=1")
            .execute(&mut *c)
            .await?;
        connections.push(c);
    }
    drop(connections);
    let tx = db.pool().begin_with("BEGIN IMMEDIATE").await?;
    assert_eq!(
        db.claim("a", &policy(), &Time::at(1000)).await,
        Err(DeliveryError::Busy)
    );
    tx.rollback().await?;
    assert_eq!(state(&db).await?, "pending");
    assert!(history(&db).await?.is_empty());
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac4_process_helper() -> Result {
    let Some(path) = std::env::var_os("CALRELAY_TEST_CHILD_DB") else {
        return Ok(());
    };
    let db = Storage::open(path).await?;
    let p = policy();
    let time = Time::at(1000);
    let lease = db.claim("child", &p, &time).await?.unwrap();
    db.prepare(&lease, &p, &time).await?;
    std::fs::write(
        std::env::var_os("CALRELAY_TEST_CHILD_READY").unwrap(),
        "ready",
    )?;
    loop {
        std::thread::park();
    }
}
struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[tokio::test]
async fn ac4_killed_process_recovers_persisted_send_intent() -> Result {
    let dir = tempdir()?;
    let path = dir.path().join("db");
    let ready = dir.path().join("ready");
    let db = Storage::open(&path).await?;
    seed(&db).await?;
    db.close().await;
    let mut child = Child(
        std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "ac4_process_helper", "--nocapture"])
            .env("CALRELAY_TEST_CHILD_DB", &path)
            .env("CALRELAY_TEST_CHILD_READY", &ready)
            .stdout(std::process::Stdio::null())
            .spawn()?,
    );
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !ready.exists() {
            if let Some(exit) = child.0.try_wait().unwrap() {
                panic!("child exited early: {exit}")
            };
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await?;
    child.0.kill()?;
    child.0.wait()?;
    let db = Storage::open(&path).await?;
    assert_eq!(state(&db).await?, "leased");
    let time = Time::at(2000);
    assert!(db.claim("new", &policy(), &time).await?.is_none());
    assert_eq!(
        history(&db).await?,
        vec![(1, Some("unknown".into()), Some("retry_wait".into()))]
    );
    time.set(2100);
    assert!(deliver_one(&db, &FakeOutput(&db), "new", &policy(), &time).await?);
    assert_eq!(state(&db).await?, "accepted");
    db.close().await;
    Ok(())
}

#[test]
fn backoff_is_bounded_and_overflow_safe() {
    let p = policy();
    for attempt in [1, 2, 8, i64::MAX] {
        for entropy in [0, 1, u64::MAX] {
            let delay = p.retry_delay_ms(attempt, entropy);
            assert!((50..=1000).contains(&delay));
        }
    }
    let mut invalid = p;
    invalid.lease_ms = invalid.request_timeout_ms;
    assert_eq!(invalid.validate(), Err(DeliveryError::InvalidPolicy));
}

#[tokio::test]
async fn paused_jobs_do_not_starve_ready_jobs_beyond_scan_limit() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    for index in 0..65 {
        let id = format!("a{index:03}");
        sqlx::query("INSERT INTO logical_deliveries(id,event_id,original_key,rule_id,output_id) VALUES (?,'e',?,'r','o')")
            .bind(&id).bind(&id).execute(db.pool()).await?;
        sqlx::query("INSERT INTO delivery_jobs(id,logical_delivery_id,generation,event_revision,rule_revision,output_revision,due_at,state,next_attempt_at) VALUES (?,?,1,1,1,1,1000,'pending',1000)")
            .bind(&id).bind(&id).execute(db.pool()).await?;
    }
    let lease = db.claim("a", &policy(), &Time::at(1000)).await?;
    assert_eq!(
        lease.map(|l| l.job_id),
        Some("j".into()),
        "legacy jobs awaiting replan must not starve a ready job"
    );
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn accepted_inflight_old_generation_deduplicates_new_generation() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    let time = Time::at(1000);
    let p = policy();
    let lease = db.claim("a", &p, &time).await?.unwrap();
    db.prepare(&lease, &p, &time).await?;
    sqlx::raw_sql("UPDATE delivery_jobs SET state='superseded'; INSERT INTO delivery_jobs(id,logical_delivery_id,generation,event_revision,rule_revision,output_revision,due_at,state,next_attempt_at,planning_context) SELECT 'j2',logical_delivery_id,2,event_revision,rule_revision,output_revision,due_at,'pending',next_attempt_at,planning_context FROM delivery_jobs WHERE id='j';").execute(db.pool()).await?;
    db.finish(&lease, Outcome::Accepted, &p, &time).await?;
    let states: Vec<String> = sqlx::query_scalar("SELECT state FROM delivery_jobs ORDER BY id")
        .fetch_all(db.pool())
        .await?;
    assert_eq!(states, vec!["superseded", "superseded"]);
    assert!(db.claim("b", &p, &time).await?.is_none());
    assert_eq!(history(&db).await?[0].1.as_deref(), Some("accepted"));
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn claimed_but_not_prepared_recovers_as_not_sent_and_attempts_stop() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    let time = Time::at(1000);
    let mut p = policy();
    p.max_attempts = 1;
    db.claim("a", &p, &time).await?.unwrap();
    time.set(2000);
    assert!(db.claim("b", &p, &time).await?.is_none());
    assert_eq!(
        history(&db).await?,
        vec![(1, Some("not_sent".into()), Some("failed_permanent".into()))]
    );
    assert_eq!(state(&db).await?, "failed_permanent");
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn ac2_exact_deadline_and_timezone_change_stop_prepared_send() -> Result {
    for mutation in [
        "UPDATE delivery_jobs SET deadline_at=1000",
        "UPDATE delivery_jobs SET planning_context='[null,\"old\"]'",
    ] {
        let dir = tempdir()?;
        let db = Storage::open(dir.path().join("db")).await?;
        seed(&db).await?;
        let time = Time::at(1000);
        let p = policy();
        let lease = db.claim("a", &p, &time).await?.unwrap();
        sqlx::query(mutation).execute(db.pool()).await?;
        assert!(db.prepare(&lease, &p, &time).await?.is_none());
        assert_eq!(history(&db).await?[0].1.as_deref(), Some("not_sent"));
        db.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn ac3_claim_attempt_and_lease_roll_back_together() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    sqlx::raw_sql("CREATE TRIGGER fail_claim BEFORE INSERT ON delivery_attempts BEGIN SELECT RAISE(ABORT,'injected'); END;").execute(db.pool()).await?;
    assert_eq!(
        db.claim("a", &policy(), &Time::at(1000)).await,
        Err(DeliveryError::Database)
    );
    assert_eq!(state(&db).await?, "pending");
    assert!(history(&db).await?.is_empty());
    let token: Option<String> = sqlx::query_scalar("SELECT lease_token FROM delivery_jobs")
        .fetch_one(db.pool())
        .await?;
    assert!(token.is_none());
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn sync_plan_deliver_and_replan_keep_real_logical_dedup() -> Result {
    use calendar_application::{
        EventRepository, PlanningLimits, PlanningRequest, ReminderPlanner, SyncBatch, SyncMode,
        SyncRecord,
    };
    const START: i64 = 1_793_638_800_000;
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    sqlx::raw_sql("DELETE FROM delivery_jobs; DELETE FROM logical_deliveries; DELETE FROM planning_watermarks; UPDATE reminder_rules SET policy_json='{\"calendar_ids\":[\"c\"],\"lead_ms\":0,\"all_day_time\":\"09:00\"}';").execute(db.pool()).await?;
    let document: String = sqlx::query_scalar("SELECT document_json FROM events")
        .fetch_one(db.pool())
        .await?;
    db.commit_sync(SyncBatch {
        source_id: "s".into(),
        base_revision: 1,
        mode: SyncMode::Delta,
        records: vec![SyncRecord::Upsert {
            calendar_id: "c".into(),
            event: Box::new(serde_json::from_str(&document)?),
        }],
        proposed_checkpoint: None,
        verified_at: START - 1000,
    })
    .await?;
    let request = PlanningRequest {
        source_id: "s".into(),
        base_revision: 2,
        rule_id: "r".into(),
        rule_generation: 1,
        output_id: "o".into(),
        output_generation: 1,
        horizon_ms: 86_400_000,
        max_lead_ms: 0,
        extension_interval_ms: 1000,
        tzdata_version: "2025b".into(),
        limits: PlanningLimits {
            max_occurrences: 10,
            max_iterations: 100,
            max_elapsed_ms: 1000,
        },
    };
    let time = Time::at(START - 1000);
    db.reconcile(request.clone(), &time).await?;
    time.set(START);
    assert!(deliver_one(&db, &FakeOutput(&db), "a", &policy(), &time).await?);
    let accepted: Option<i64> = sqlx::query_scalar("SELECT accepted_at FROM logical_deliveries")
        .fetch_one(db.pool())
        .await?;
    assert_eq!(accepted, Some(START));
    db.reconcile(request, &time).await?;
    assert!(!deliver_one(&db, &FakeOutput(&db), "b", &policy(), &time).await?);
    assert_eq!(history(&db).await?.len(), 1);
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn repeated_prepare_after_cancellation_must_not_erase_inflight_outcome() -> Result {
    let dir = tempdir()?;
    let db = Storage::open(dir.path().join("db")).await?;
    seed(&db).await?;
    let time = Time::at(1000);
    let p = policy();
    let lease = db.claim("a", &p, &time).await?.unwrap();
    db.prepare(&lease, &p, &time).await?.unwrap();
    sqlx::query("UPDATE delivery_jobs SET state='cancelled'")
        .execute(db.pool())
        .await?;
    assert!(matches!(
        db.prepare(&lease, &p, &time).await,
        Err(DeliveryError::StaleLease)
    ));
    db.finish(&lease, Outcome::Accepted, &p, &time).await?;
    assert_eq!(
        history(&db).await?,
        vec![(1, Some("accepted".into()), Some("cancelled".into()))]
    );
    db.close().await;
    Ok(())
}
