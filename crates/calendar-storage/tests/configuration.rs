use calendar_application::{
    configuration::*, EventRepository, SourceRepository, SyncBatch, SyncMode,
};
use calendar_storage::{Storage, SyncError};
fn config() -> SourceConfigurationV1 {
    serde_json::from_str(include_str!(
        "../../../examples/source-configuration-v1.json"
    ))
    .unwrap()
}
async fn number(db: &Storage, query: &'static str) -> i64 {
    sqlx::query_scalar(query)
        .fetch_one(db.pool())
        .await
        .unwrap()
}
#[tokio::test]
async fn cas_rename_restart_retirement_and_atomic_failure() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let db = Storage::open(&path).await.unwrap();
    let first = db
        .put_source_configuration(
            "source-a",
            ConfigurationCondition::Absent,
            config(),
            "alice",
            100,
        )
        .await
        .unwrap();
    assert_eq!((first.config_revision, first.source_revision), (1, 1));
    let mut enabled = config();
    enabled.enabled = true;
    let second = db
        .put_source_configuration(
            "source-a",
            ConfigurationCondition::Revision(1),
            enabled.clone(),
            "alice",
            101,
        )
        .await
        .unwrap();
    assert_eq!(second.config_revision, 2);
    let state = db.source_state("source-a").await.unwrap();
    enabled.name = "Renamed".into();
    let (a, b) = tokio::join!(
        db.put_source_configuration(
            "source-a",
            ConfigurationCondition::Revision(2),
            enabled.clone(),
            "alice",
            102
        ),
        db.put_source_configuration(
            "source-a",
            ConfigurationCondition::Revision(2),
            enabled.clone(),
            "bob",
            102
        )
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert!(matches!(
        a.as_ref().err().or(b.as_ref().err()).unwrap(),
        ConfigurationError::Conflict
    ));
    assert_eq!(
        number(&db, "SELECT count(*) FROM configuration_audit").await,
        3
    );
    let stale = SyncBatch {
        source_id: "source-a".into(),
        base_revision: state.revision,
        mode: SyncMode::Delta,
        records: vec![],
        proposed_checkpoint: Some("bad".into()),
        verified_at: 103,
    };
    assert!(matches!(
        db.commit_sync(stale).await,
        Err(SyncError::Conflict)
    ));
    assert!(db
        .source_state("source-a")
        .await
        .unwrap()
        .checkpoint
        .is_none());
    sqlx::query("CREATE TRIGGER fail_audit BEFORE INSERT ON configuration_audit BEGIN SELECT RAISE(ABORT,'injected'); END").execute(db.pool()).await.unwrap();
    enabled.name = "Must roll back".into();
    assert!(db
        .put_source_configuration(
            "source-a",
            ConfigurationCondition::Revision(3),
            enabled.clone(),
            "alice",
            104
        )
        .await
        .is_err());
    assert_eq!(
        db.source_configuration("source-a")
            .await
            .unwrap()
            .config_revision,
        3
    );
    assert_eq!(
        db.source_configuration("source-a")
            .await
            .unwrap()
            .configuration
            .name,
        "Renamed"
    );
    sqlx::query("DROP TRIGGER fail_audit")
        .execute(db.pool())
        .await
        .unwrap();
    let token = db
        .issue_api_token("t", "source-a", true, true, None)
        .await
        .unwrap();
    // A configuration identity conflict rolls back source row and calendar writes.
    assert!(matches!(
        db.put_source_configuration(
            "source-b",
            ConfigurationCondition::Absent,
            config(),
            "alice",
            105
        )
        .await,
        Err(ConfigurationError::Invalid)
    ));
    assert_eq!(number(&db, "SELECT count(*) FROM sources").await, 1);
    db.close().await;
    let db = Storage::open(&path).await.unwrap();
    let persisted = db.source_configuration("source-a").await.unwrap();
    assert_eq!(persisted.source_id, "source-a");
    assert_eq!(persisted.configuration.name, "Renamed");
    db.retire_source_configuration("source-a", 3, "alice", 106)
        .await
        .unwrap();
    let retired = db.source_configuration("source-a").await.unwrap();
    assert_eq!(retired.retired_at, Some(106));
    assert!(!retired.configuration.enabled);
    assert_eq!(
        number(&db, "SELECT count(*) FROM api_tokens WHERE revoked_at=106").await,
        1
    );
    assert!(matches!(
        db.put_source_configuration(
            "source-a",
            ConfigurationCondition::Revision(4),
            enabled,
            "alice",
            107
        )
        .await,
        Err(ConfigurationError::Conflict)
    ));
    assert_eq!(
        number(&db, "SELECT count(*) FROM configuration_audit").await,
        4
    );
    let audit: String =
        sqlx::query_scalar("SELECT group_concat(actor_digest) FROM configuration_audit")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(!audit.contains("alice"));
    assert!(!audit.contains(std::str::from_utf8(token.expose()).unwrap()));
    db.close().await;
}
#[tokio::test]
async fn legacy_and_calendar_identities_are_not_reinterpreted() {
    let dir = tempfile::tempdir().unwrap();
    let db = Storage::open(dir.path().join("db")).await.unwrap();
    sqlx::query(
        "INSERT INTO sources(id,kind,config_json) VALUES('legacy','ics','{\"custom\":true}')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    assert!(matches!(
        db.source_configuration("legacy").await,
        Err(ConfigurationError::NotFound)
    ));
    for condition in [
        ConfigurationCondition::Absent,
        ConfigurationCondition::Revision(0),
    ] {
        assert!(matches!(
            db.put_source_configuration("legacy", condition, config(), "alice", 100)
                .await,
            Err(ConfigurationError::Conflict)
        ));
    }
    db.put_source_configuration(
        "source-a",
        ConfigurationCondition::Absent,
        config(),
        "alice",
        100,
    )
    .await
    .unwrap();
    let mut remap = config();
    remap.calendars[0].external_id = "other".into();
    assert!(matches!(
        db.put_source_configuration(
            "source-a",
            ConfigurationCondition::Revision(1),
            remap,
            "alice",
            101
        )
        .await,
        Err(ConfigurationError::Conflict)
    ));
    db.close().await;
}
#[test]
fn source_capabilities_and_untrusted_fields_have_strict_semantics() {
    let original = config();
    original.validate().unwrap();
    for invalid in ["source_id", "config_revision", "raw_token", "unexpected"] {
        let mut value = serde_json::to_value(&original).unwrap();
        value[invalid] = serde_json::json!("forged");
        assert!(serde_json::from_value::<SourceConfigurationV1>(value).is_err());
    }
    let mut pull = original.clone();
    pull.mode = SourceMode::Pull;
    pull.endpoint = Some("https://calendar.example.test/path".into());
    pull.refresh_interval_ms = Some(60_000);
    pull.freshness_sla_ms = Some(86_400_000);
    pull.capabilities.conditional_read = true;
    pull.validate().unwrap();
    for url in [
        "http://calendar.example.test",
        "https://user:secret@calendar.example.test",
        "https://calendar.example.test/?token=secret",
        "https://calendar.example.test/#secret",
    ] {
        pull.endpoint = Some(url.into());
        assert!(pull.validate().is_err(), "{url}");
    }
    let mut bad = original.clone();
    bad.freshness_sla_ms = Some(60000);
    assert!(bad.validate().is_err());
    let mut bad = original.clone();
    bad.calendars[0].timezone = Some("Unknown/Zone".into());
    assert!(bad.validate().is_err());
    let mut bad = original.clone();
    bad.capabilities.delete = true;
    assert!(bad.validate().is_err());
    let mut push = original;
    push.mode = SourceMode::PushOnly;
    push.adapter = AdapterKind::StandardCalendar;
    push.capabilities.delta = true;
    push.capabilities.delete = true;
    push.validate().unwrap(); // Intermittent push producers require no implicit SLA.
}
