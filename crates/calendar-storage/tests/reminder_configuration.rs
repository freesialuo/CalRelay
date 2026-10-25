use calendar_application::{
    configuration::*, deliver_one, Clock, CredentialProvider, DeletionPolicy, DeliveryEnvelope,
    DeliveryOutcome, DeliveryPolicy, DeliveryRepository, EventRepository, PlanningLimits,
    PlanningRequest, PushOutput, ReminderPlanner, SecretBytes, SyncBatch, SyncMode, SyncRecord,
};
use calendar_storage::{
    security::{KeyRing, SecurityStore},
    Storage,
};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicI64, AtomicUsize, Ordering},
};
const START: i64 = 1_793_638_800_000;
struct Time(AtomicI64);
impl Clock for Time {
    fn utc_now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn monotonic_now_ms(&self) -> u64 {
        0
    }
}
fn output() -> BarkOutputConfigurationV1 {
    serde_json::from_str(include_str!(
        "../../../examples/bark-output-configuration-v1.json"
    ))
    .unwrap()
}
fn rule() -> RuleConfigurationV1 {
    let mut r: RuleConfigurationV1 =
        serde_json::from_str(include_str!("../../../examples/rule-configuration-v1.json")).unwrap();
    r.policy.lead_ms = 0;
    r.output_ids.push("bark-b".into());
    r
}
struct Fixture {
    _dir: tempfile::TempDir,
    db: Storage,
    keys: KeyRing,
    time: Time,
}
async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("mock-key");
    std::fs::write(&key, [7; 32]).unwrap();
    let keys = KeyRing::from_files(1, &BTreeMap::from([(1, key)])).unwrap();
    let db = Storage::open(dir.path().join("db")).await.unwrap();
    let provider = SecurityStore {
        storage: &db,
        keys: &keys,
    };
    for id in ["bark-credential", "replacement"] {
        provider
            .create_credential(
                id,
                "bark",
                &SecretBytes::new(b"synthetic-mock-secret".to_vec()),
                START - 1000,
            )
            .await
            .unwrap();
    }
    provider
        .create_credential(
            "wrong-kind",
            "bearer",
            &SecretBytes::new(b"synthetic-mock-secret".to_vec()),
            START - 1000,
        )
        .await
        .unwrap();
    let mut source: SourceConfigurationV1 = serde_json::from_str(include_str!(
        "../../../examples/source-configuration-v1.json"
    ))
    .unwrap();
    db.put_source_configuration(
        "source-a",
        ConfigurationCondition::Absent,
        source.clone(),
        "test",
        START - 1000,
    )
    .await
    .unwrap();
    source.enabled = true;
    db.put_source_configuration(
        "source-a",
        ConfigurationCondition::Revision(1),
        source,
        "test",
        START - 1000,
    )
    .await
    .unwrap();
    for id in ["bark-a", "bark-b"] {
        let mut o = output();
        db.put_bark_output_configuration(
            id,
            ConfigurationCondition::Absent,
            o.clone(),
            "test",
            START - 1000,
        )
        .await
        .unwrap();
        o.enabled = true;
        db.put_bark_output_configuration(
            id,
            ConfigurationCondition::Revision(1),
            o,
            "test",
            START - 1000,
        )
        .await
        .unwrap();
    }
    let mut r = rule();
    db.put_rule_configuration(
        "rule-a",
        ConfigurationCondition::Absent,
        r.clone(),
        "test",
        START - 1000,
    )
    .await
    .unwrap();
    r.enabled = true;
    db.put_rule_configuration(
        "rule-a",
        ConfigurationCondition::Revision(1),
        r,
        "test",
        START - 1000,
    )
    .await
    .unwrap();
    let event=serde_json::from_value(serde_json::json!({"schema_version":"1.0","external_uid":"uid","title":"Reminder","status":"confirmed","start":{"kind":"utc","value":"2026-11-02T17:00:00Z"}})).unwrap();
    db.commit_sync(SyncBatch {
        source_id: "source-a".into(),
        base_revision: 2,
        mode: SyncMode::FullSnapshot {
            complete: true,
            deletion_policy: DeletionPolicy::Limit(100),
        },
        records: vec![SyncRecord::Upsert {
            calendar_id: "calendar-a".into(),
            event: Box::new(event),
        }],
        proposed_checkpoint: None,
        verified_at: START - 1000,
    })
    .await
    .unwrap();
    Fixture {
        _dir: dir,
        db,
        keys,
        time: Time(AtomicI64::new(START - 1000)),
    }
}
fn request(f: &PlanningConfiguration) -> PlanningRequest {
    PlanningRequest {
        source_id: f.source_id.clone(),
        base_revision: f.source_revision,
        rule_id: f.rule_id.clone(),
        rule_generation: f.rule_generation,
        output_id: f.output_id.clone(),
        output_generation: f.output_generation,
        horizon_ms: 86_400_000,
        max_lead_ms: 604_800_000,
        extension_interval_ms: 300_000,
        tzdata_version: "2025b".into(),
        limits: PlanningLimits {
            max_occurrences: 100,
            max_iterations: 1000,
            max_elapsed_ms: 1000,
        },
    }
}
async fn plan(f: &Fixture) {
    for cfg in f.db.planning_configurations(None).await.unwrap().items {
        let mut r = request(&cfg);
        r.horizon_ms = 30 * 86_400_000;
        f.db.reconcile(r, &f.time).await.unwrap();
    }
    f.time.0.store(START, Ordering::SeqCst);
}
struct MockDispatch<'a> {
    fixture: &'a Fixture,
    calls: AtomicUsize,
}
impl PushOutput for MockDispatch<'_> {
    async fn deliver(&self, envelope: DeliveryEnvelope) -> DeliveryOutcome {
        let cfg = self
            .fixture
            .db
            .output_for_delivery(&envelope.job_id, &envelope.output_id)
            .await
            .unwrap()
            .unwrap();
        let secret = SecurityStore {
            storage: &self.fixture.db,
            keys: &self.fixture.keys,
        }
        .credential(&cfg.configuration.credential_id, "bark")
        .await
        .unwrap();
        assert_eq!(secret.expose(), b"synthetic-mock-secret");
        self.calls.fetch_add(1, Ordering::SeqCst);
        DeliveryOutcome::Accepted
    }
}
#[tokio::test]
async fn persisted_management_configuration_drives_real_planner_and_dispatch_consumer() {
    let f = fixture().await;
    let configs = f.db.planning_configurations(None).await.unwrap();
    assert_eq!(configs.items.len(), 2);
    for cfg in &configs.items {
        assert_eq!(cfg.source_config_revision, 2);
        assert_eq!(cfg.source_revision, 3);
        assert_eq!(cfg.source_mode, SourceMode::ManualStatic);
        assert_eq!(cfg.freshness_sla_ms, None);
        assert_eq!(cfg.rule_generation, 2);
        assert_eq!(cfg.output_generation, 2);
    }
    plan(&f).await;
    let dispatch = MockDispatch {
        fixture: &f,
        calls: AtomicUsize::new(0),
    };
    let policy = DeliveryPolicy::default();
    for _ in 0..2 {
        assert!(deliver_one(&f.db, &dispatch, "worker", &policy, &f.time)
            .await
            .unwrap());
    }
    assert!(!deliver_one(&f.db, &dispatch, "worker", &policy, &f.time)
        .await
        .unwrap());
    assert_eq!(dispatch.calls.load(Ordering::SeqCst), 2);
    let mut r =
        f.db.rule_configuration("rule-a")
            .await
            .unwrap()
            .configuration;
    r.name = "Rename".into();
    f.db.put_rule_configuration(
        "rule-a",
        ConfigurationCondition::Revision(2),
        r,
        "admin",
        START,
    )
    .await
    .unwrap();
    plan(&f).await;
    assert!(!deliver_one(&f.db, &dispatch, "worker", &policy, &f.time)
        .await
        .unwrap());
    let accepted: i64 =
        sqlx::query_scalar("SELECT count(*) FROM logical_deliveries WHERE accepted_at IS NOT NULL")
            .fetch_one(f.db.pool())
            .await
            .unwrap();
    assert_eq!(accepted, 2);
    f.db.close().await;
    let reopened = Storage::open(f._dir.path().join("db")).await.unwrap();
    assert_eq!(
        reopened
            .rule_configuration("rule-a")
            .await
            .unwrap()
            .configuration
            .name,
        "Rename"
    );
    assert_eq!(
        reopened
            .bark_output_configuration("bark-a")
            .await
            .unwrap()
            .generation,
        2
    );
    reopened.close().await;
}
#[tokio::test]
async fn configuration_changes_and_secret_rotation_reject_old_dispatch_but_keep_real_acceptance() {
    for action in [
        "rule_edit",
        "rule_disable",
        "output_reference",
        "output_disable",
        "credential_replace",
        "credential_reencrypt",
        "unbind",
        "rule_retire",
        "output_retire",
    ] {
        let f = fixture().await;
        plan(&f).await;
        let p = DeliveryPolicy::default();
        let lease = f.db.claim("worker", &p, &f.time).await.unwrap().unwrap();
        for output in ["bark-a", "bark-b"] {
            assert!(f
                .db
                .output_for_delivery(&lease.job_id, output)
                .await
                .unwrap()
                .is_none());
        }
        let envelope = f.db.prepare(&lease, &p, &f.time).await.unwrap().unwrap();
        let id = &envelope.output_id;
        assert!(f
            .db
            .output_for_delivery(&lease.job_id, id)
            .await
            .unwrap()
            .is_some());
        match action {
            "rule_edit" | "rule_disable" | "unbind" => {
                let mut r =
                    f.db.rule_configuration("rule-a")
                        .await
                        .unwrap()
                        .configuration;
                if action == "rule_edit" {
                    r.policy.lead_ms = 1000;
                }
                if action == "rule_disable" {
                    r.enabled = false;
                }
                if action == "unbind" {
                    r.output_ids.retain(|s| s != id);
                }
                f.db.put_rule_configuration(
                    "rule-a",
                    ConfigurationCondition::Revision(2),
                    r,
                    "admin",
                    START,
                )
                .await
                .unwrap();
            }
            "output_reference" | "output_disable" => {
                let mut o =
                    f.db.bark_output_configuration(id)
                        .await
                        .unwrap()
                        .configuration;
                if action == "output_reference" {
                    o.credential_id = "replacement".into();
                } else {
                    o.enabled = false;
                }
                f.db.put_bark_output_configuration(
                    id,
                    ConfigurationCondition::Revision(2),
                    o,
                    "admin",
                    START,
                )
                .await
                .unwrap();
            }
            "credential_replace" => SecurityStore {
                storage: &f.db,
                keys: &f.keys,
            }
            .replace_credential(
                "bark-credential",
                0,
                &SecretBytes::new(b"synthetic-new-secret".to_vec()),
                START,
            )
            .await
            .unwrap(),
            "credential_reencrypt" => SecurityStore {
                storage: &f.db,
                keys: &f.keys,
            }
            .rotate_credential("bark-credential", 0, START)
            .await
            .unwrap(),
            "rule_retire" => {
                f.db.retire_rule_configuration("rule-a", 2, "admin", START)
                    .await
                    .unwrap()
            }
            "output_retire" => {
                f.db.retire_bark_output_configuration(id, 2, "admin", START)
                    .await
                    .unwrap()
            }
            _ => unreachable!(),
        }
        assert!(
            f.db.output_for_delivery(&lease.job_id, id)
                .await
                .unwrap()
                .is_none(),
            "{action}"
        );
        f.db.finish(&lease, DeliveryOutcome::Accepted, &p, &f.time)
            .await
            .unwrap();
        let (outcome, disposition): (String, String) =
            sqlx::query_as("SELECT outcome,disposition FROM delivery_attempts WHERE job_id=?")
                .bind(&lease.job_id)
                .fetch_one(f.db.pool())
                .await
                .unwrap();
        assert_eq!(outcome, "accepted", "{action}");
        assert!(
            matches!(disposition.as_str(), "superseded" | "cancelled"),
            "{action}: {disposition}"
        );
        let accepted:Option<i64>=sqlx::query_scalar("SELECT l.accepted_at FROM logical_deliveries l JOIN delivery_jobs j ON j.logical_delivery_id=l.id WHERE j.id=?").bind(&lease.job_id).fetch_one(f.db.pool()).await.unwrap();
        assert_eq!(accepted, Some(START));
        f.db.close().await;
    }
}
#[tokio::test]
async fn cas_and_audit_failures_are_atomic_without_plaintext_or_legacy_adoption() {
    let f = fixture().await;
    plan(&f).await;
    let o = f.db.bark_output_configuration("bark-a").await.unwrap();
    let (a, b) = tokio::join!(
        f.db.put_bark_output_configuration(
            "bark-a",
            ConfigurationCondition::Revision(2),
            o.configuration.clone(),
            "alice",
            START
        ),
        f.db.put_bark_output_configuration(
            "bark-a",
            ConfigurationCondition::Revision(2),
            o.configuration.clone(),
            "bob",
            START
        )
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let mut wrong = o.configuration.clone();
    wrong.credential_id = "wrong-kind".into();
    assert!(matches!(
        f.db.put_bark_output_configuration(
            "new",
            ConfigurationCondition::Absent,
            {
                wrong.enabled = false;
                wrong
            },
            "admin",
            START
        )
        .await,
        Err(ConfigurationError::Invalid)
    ));
    sqlx::query("CREATE TRIGGER fail_reminder_audit BEFORE INSERT ON reminder_configuration_audit BEGIN SELECT RAISE(ABORT,'injected'); END").execute(f.db.pool()).await.unwrap();
    let before = f.db.bark_output_configuration("bark-a").await.unwrap();
    assert!(f
        .db
        .put_bark_output_configuration(
            "bark-a",
            ConfigurationCondition::Revision(3),
            before.configuration.clone(),
            "admin",
            START
        )
        .await
        .is_err());
    assert_eq!(
        f.db.bark_output_configuration("bark-a").await.unwrap(),
        before
    );
    assert!(SecurityStore {
        storage: &f.db,
        keys: &f.keys
    }
    .replace_credential(
        "bark-credential",
        0,
        &SecretBytes::new(b"must-roll-back".to_vec()),
        START
    )
    .await
    .is_err());
    assert_eq!(
        f.db.bark_output_configuration("bark-a")
            .await
            .unwrap()
            .generation,
        3
    );
    let secret = SecurityStore {
        storage: &f.db,
        keys: &f.keys,
    }
    .credential("bark-credential", "bark")
    .await
    .unwrap();
    assert_eq!(secret.expose(), b"synthetic-mock-secret");
    let document = serde_json::to_string(&before).unwrap();
    assert!(!document.contains("synthetic-mock-secret"));
    assert!(!document.contains("ciphertext"));
    let audit: String = sqlx::query_scalar(
        "SELECT group_concat(COALESCE(actor_digest,'')) FROM reminder_configuration_audit",
    )
    .fetch_one(f.db.pool())
    .await
    .unwrap();
    assert!(!audit.contains("alice"));
    assert!(!audit.contains("synthetic"));
    sqlx::query("INSERT INTO outputs(id,kind) VALUES('legacy','custom')")
        .execute(f.db.pool())
        .await
        .unwrap();
    assert!(matches!(
        f.db.bark_output_configuration("legacy").await,
        Err(ConfigurationError::NotFound)
    ));
    let mut disabled = output();
    disabled.enabled = false;
    assert!(matches!(
        f.db.put_bark_output_configuration(
            "legacy",
            ConfigurationCondition::Revision(1),
            disabled,
            "admin",
            START
        )
        .await,
        Err(ConfigurationError::Conflict)
    ));
    f.db.close().await;
}

#[tokio::test]
async fn bounded_discovery_is_current_without_filtering_intermittent_sources() {
    let f = fixture().await;
    for n in 0..50 {
        let mut r = rule();
        r.enabled = false;
        let id = format!("rule-{n:03}");
        f.db.put_rule_configuration(
            &id,
            ConfigurationCondition::Absent,
            r.clone(),
            "test",
            START,
        )
        .await
        .unwrap();
        r.enabled = true;
        f.db.put_rule_configuration(&id, ConfigurationCondition::Revision(1), r, "test", START)
            .await
            .unwrap();
    }
    sqlx::query("UPDATE sources SET last_verified_at=NULL")
        .execute(f.db.pool())
        .await
        .unwrap();
    let page = f.db.planning_configurations(None).await.unwrap();
    assert_eq!(page.items.len(), 100);
    assert!(page.next_after.is_some());
    let last =
        f.db.planning_configurations(page.next_after.as_deref())
            .await
            .unwrap();
    assert_eq!(last.items.len(), 2);
    assert!(last.next_after.is_none());
    for cursor in ["source-a", "::", "source-a:bad id:bark-a"] {
        assert!(f.db.planning_configurations(Some(cursor)).await.is_err());
    }
    for n in 0..100 {
        f.db.put_bark_output_configuration(
            &format!("bark-{n:03}"),
            ConfigurationCondition::Absent,
            output(),
            "test",
            START,
        )
        .await
        .unwrap();
    }
    f.db.retire_bark_output_configuration("bark-000", 1, "test", START)
        .await
        .unwrap();
    let page = f.db.list_reminder_configurations(None, true).await.unwrap();
    assert_eq!(page.items.len(), 100);
    assert_eq!(page.items[0].retired_at, Some(START));
    assert_eq!(page.next_after.as_deref(), Some("bark-099"));
    let last =
        f.db.list_reminder_configurations(page.next_after.as_deref(), true)
            .await
            .unwrap();
    assert_eq!(last.items.len(), 2);
    assert!(last.next_after.is_none());
    assert!(f
        .db
        .list_reminder_configurations(Some("bad id"), false)
        .await
        .is_err());
    let mut invalid = rule();
    invalid.policy.calendar_ids = vec!["not-owned".into()];
    assert!(matches!(
        f.db.put_rule_configuration(
            "invalid",
            ConfigurationCondition::Absent,
            invalid,
            "test",
            START
        )
        .await,
        Err(ConfigurationError::Invalid)
    ));
    f.db.retire_rule_configuration("rule-a", 2, "admin", START)
        .await
        .unwrap();
    let mut config = rule();
    config.enabled = true;
    assert!(matches!(
        f.db.put_rule_configuration(
            "rule-a",
            ConfigurationCondition::Revision(3),
            config,
            "test",
            START
        )
        .await,
        Err(ConfigurationError::Conflict)
    ));
    f.db.close().await;
}
#[test]
fn strict_business_shapes_reuse_rule_policy_and_reject_secret_urls_and_unknown_fields() {
    rule().validate().unwrap();
    output().validate().unwrap();
    for origin in [
        "http://bark.example.test",
        "https://bark.example.test/key-in-path",
        "https://user:secret@bark.example.test",
        "https://bark.example.test/?key=secret",
        "https://bark.example.test/#secret",
    ] {
        let mut o = output();
        o.origin = origin.into();
        assert!(o.validate().is_err());
    }
    for time in ["24:00", "12:60", "not-a-time"] {
        let mut r = rule();
        r.policy.all_day_time = time.into();
        assert!(r.validate().is_err());
    }
    let mut r = rule();
    r.policy.calendar_ids.push(r.policy.calendar_ids[0].clone());
    assert!(r.validate().is_err());
    for field in ["device_key", "token", "script"] {
        let mut v = serde_json::to_value(output()).unwrap();
        v[field] = serde_json::json!("synthetic-secret");
        assert!(serde_json::from_value::<BarkOutputConfigurationV1>(v).is_err());
    }
}

#[tokio::test]
async fn source_freshness_is_capability_aware_at_claim_and_prepare() {
    for (mode, sla, age, allowed) in [
        (SourceMode::ManualStatic, None, None, true),
        (SourceMode::PushOnly, None, None, true),
        (SourceMode::PushOnly, Some(60_000), Some(60_001), false),
        (SourceMode::PushOnly, Some(60_000), Some(60_000), true),
        (SourceMode::Pull, Some(60_000), None, false),
        (SourceMode::Pull, Some(60_000), Some(60_000), true),
        (SourceMode::Pull, Some(60_000), Some(-1), false),
    ] {
        let f = fixture().await;
        let mut cfg =
            f.db.source_configuration("source-a")
                .await
                .unwrap()
                .configuration;
        cfg.mode = mode.clone();
        cfg.freshness_sla_ms = sla;
        if mode == SourceMode::PushOnly {
            cfg.adapter = AdapterKind::StandardCalendar;
        }
        if mode == SourceMode::Pull {
            cfg.endpoint = Some("https://source.example.test/feed".into());
            cfg.refresh_interval_ms = Some(60_000);
        }
        f.db.put_source_configuration(
            "source-a",
            ConfigurationCondition::Revision(2),
            cfg,
            "test",
            START - 1000,
        )
        .await
        .unwrap();
        plan(&f).await;
        sqlx::query("UPDATE sources SET last_verified_at=?")
            .bind(age.map(|n| START - n))
            .execute(f.db.pool())
            .await
            .unwrap();
        let policy = DeliveryPolicy::default();
        let lease = f.db.claim("worker", &policy, &f.time).await.unwrap();
        assert_eq!(lease.is_some(), allowed, "{mode:?}/{sla:?}/{age:?}");
        if let Some(lease) = lease {
            if sla.is_some() {
                sqlx::query("UPDATE sources SET last_verified_at=NULL")
                    .execute(f.db.pool())
                    .await
                    .unwrap();
                assert!(f
                    .db
                    .prepare(&lease, &policy, &f.time)
                    .await
                    .unwrap()
                    .is_none());
            } else {
                assert!(f
                    .db
                    .prepare(&lease, &policy, &f.time)
                    .await
                    .unwrap()
                    .is_some());
            }
        }
        f.db.close().await;
    }
}
