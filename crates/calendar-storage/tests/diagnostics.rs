use calendar_application::configuration::{
    ConfigurationCondition, ConfigurationError, SourceConfigurationV1,
};
use calendar_storage::Storage;

#[tokio::test]
async fn diagnostics_match_source_state_before_and_after_retirement() {
    for enabled in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let db = Storage::open(dir.path().join("db")).await.unwrap();
        let mut config: SourceConfigurationV1 = serde_json::from_str(include_str!(
            "../../../examples/source-configuration-v1.json"
        ))
        .unwrap();
        db.put_source_configuration(
            "retired",
            ConfigurationCondition::Absent,
            config.clone(),
            "test",
            1,
        )
        .await
        .unwrap();
        config.enabled = enabled;
        db.put_source_configuration(
            "retired",
            ConfigurationCondition::Revision(1),
            config,
            "test",
            2,
        )
        .await
        .unwrap();
        let before = db.source_diagnostic("retired").await.unwrap();
        assert_eq!(before.configuration.enabled, enabled);
        assert_eq!(before.retired_at, None);

        db.retire_source_configuration("retired", 2, "test", 3)
            .await
            .unwrap();
        let saved = db.source_configuration("retired").await.unwrap();
        let diagnostic = db.source_diagnostic("retired").await.unwrap();
        assert!(!saved.configuration.enabled);
        assert_eq!(diagnostic.configuration, saved.configuration);
        assert_eq!(diagnostic.retired_at, Some(3));
        assert_eq!(diagnostic.retired_at, saved.retired_at);
        db.close().await;
    }
}

#[tokio::test]
async fn read_projection_is_source_scoped_bounded_and_uses_latest_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let db = Storage::open(dir.path().join("db")).await.unwrap();
    let config: SourceConfigurationV1 = serde_json::from_str(include_str!(
        "../../../examples/source-configuration-v1.json"
    ))
    .unwrap();
    db.put_source_configuration("diag", ConfigurationCondition::Absent, config, "test", 1)
        .await
        .unwrap();
    let empty = db.source_diagnostic("diag").await.unwrap();
    assert!(empty.deliveries.is_empty());
    assert!(!empty.more_deliveries);
    assert_eq!(empty.last_verified_at, None);
    assert!(matches!(
        db.source_diagnostic("missing").await,
        Err(ConfigurationError::NotFound)
    ));
    for query in [
        "INSERT INTO events(id,source_id,calendar_id,external_uid,revision,document_json,content_hash) VALUES ('event','diag','calendar-a','uid',1,'{}','hash')",
        "INSERT INTO outputs(id,kind,credential_id) VALUES ('out','bark',NULL)",
        "INSERT INTO reminder_rules(id) VALUES ('rule')",
        "INSERT INTO rule_outputs(rule_id,output_id) VALUES ('rule','out')"
    ] { sqlx::query(query).execute(db.pool()).await.unwrap(); }
    for n in 0..21 {
        let id = format!("job{n}");
        sqlx::query("INSERT INTO logical_deliveries(id,event_id,original_key,rule_id,output_id) VALUES (?,'event',?,'rule','out')")
            .bind(&id).bind(n.to_string()).execute(db.pool()).await.unwrap();
        sqlx::query("INSERT INTO delivery_jobs(id,logical_delivery_id,generation,event_revision,rule_revision,output_revision,due_at,state,next_attempt_at) VALUES (?,?,1,1,1,1,10,'retry_wait',11)")
            .bind(&id).bind(&id).execute(db.pool()).await.unwrap();
    }
    for (n, outcome) in [(1, "retryable"), (2, "unknown")] {
        sqlx::query("INSERT INTO delivery_attempts(id,job_id,attempt_no,lease_owner,lease_token,request_id,started_at,sent_at,finished_at,outcome,disposition) VALUES (?,'job20',?,'SECRET_OWNER',?,?,1,2,3,?,'retry_wait')")
            .bind(format!("attempt{n}")).bind(n).bind(format!("SECRET_LEASE{n}")).bind(format!("SECRET_REQUEST{n}")).bind(outcome).execute(db.pool()).await.unwrap();
    }
    let row = db.source_diagnostic("diag").await.unwrap();
    assert!(row.more_deliveries);
    assert_eq!(row.deliveries.len(), 20);
    assert_eq!(row.deliveries[0].job_id, "job20");
    assert_eq!(row.deliveries[0].last_outcome.as_deref(), Some("unknown"));
    assert_eq!(row.deliveries[0].finished_at, Some(3));
    assert_eq!(row.deliveries[19].job_id, "job1");
    assert!(row.deliveries[1].last_outcome.is_none());
    let mut other: SourceConfigurationV1 = serde_json::from_str(include_str!(
        "../../../examples/source-configuration-v1.json"
    ))
    .unwrap();
    other.calendars[0].id = "calendar-other".into();
    db.put_source_configuration("other", ConfigurationCondition::Absent, other, "test", 2)
        .await
        .unwrap();
    assert!(db
        .source_diagnostic("other")
        .await
        .unwrap()
        .deliveries
        .is_empty());
    db.close().await;
}

#[tokio::test]
async fn sync_status_is_one_bounded_row_scoped_to_the_current_configuration_revision() {
    use calendar_storage::{SourceSyncOutcome, SourceSyncStatus};
    let dir = tempfile::tempdir().unwrap();
    let db = Storage::open(dir.path().join("db")).await.unwrap();
    let config: SourceConfigurationV1 = serde_json::from_str(include_str!(
        "../../../examples/source-configuration-v1.json"
    ))
    .unwrap();
    let record = db
        .put_source_configuration("sync", ConfigurationCondition::Absent, config, "test", 1)
        .await
        .unwrap();
    assert_eq!(db.source_sync_status("sync").await.unwrap(), None);
    assert_eq!(
        db.source_diagnostic("sync").await.unwrap().sync_status,
        None
    );

    let failed = SourceSyncStatus {
        config_revision: record.config_revision,
        attempted_at: 10,
        outcome: SourceSyncOutcome::RetryAfter,
        consecutive_failures: 2,
        next_allowed_at: 3_600_010,
    };
    db.record_source_sync_status("sync", failed).await.unwrap();
    let recovered = SourceSyncStatus {
        attempted_at: 20,
        outcome: SourceSyncOutcome::NotModified,
        consecutive_failures: 0,
        next_allowed_at: 1020,
        ..failed
    };
    db.record_source_sync_status("sync", recovered)
        .await
        .unwrap();
    assert_eq!(
        db.source_sync_status("sync").await.unwrap(),
        Some(recovered)
    );
    assert_eq!(
        db.source_diagnostic("sync").await.unwrap().sync_status,
        Some(recovered)
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM source_sync_status")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 1, "an upsert, never a history table");

    // Only the enumeration and times are storable; free text is rejected by the schema.
    assert!(
        sqlx::query("UPDATE source_sync_status SET outcome='HTTP 500: secret body'")
            .execute(db.pool())
            .await
            .is_err()
    );
    for (outcome, name) in SourceSyncOutcome::ALL {
        db.record_source_sync_status(
            "sync",
            SourceSyncStatus {
                outcome,
                ..recovered
            },
        )
        .await
        .unwrap();
        let stored: String = sqlx::query_scalar("SELECT outcome FROM source_sync_status")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(stored, name);
        assert_eq!(
            db.source_sync_status("sync")
                .await
                .unwrap()
                .unwrap()
                .outcome,
            outcome
        );
    }

    // A new configuration revision is not constrained or described by the old attempt.
    let mut changed = record.configuration.clone();
    changed.name = "renamed".into();
    db.put_source_configuration(
        "sync",
        ConfigurationCondition::Revision(record.config_revision),
        changed,
        "test",
        2,
    )
    .await
    .unwrap();
    assert_eq!(
        db.source_diagnostic("sync").await.unwrap().sync_status,
        None
    );
    db.close().await;
}
