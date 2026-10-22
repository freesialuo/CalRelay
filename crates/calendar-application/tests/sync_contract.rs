use calendar_application::{
    BatchError, DeletionPolicy, SyncBatch, SyncMode, SyncRecord, MAX_BATCH_RECORDS,
};

fn batch() -> SyncBatch {
    SyncBatch {
        source_id: "s".into(),
        base_revision: 0,
        mode: SyncMode::Delta,
        records: vec![],
        proposed_checkpoint: None,
        verified_at: 0,
    }
}

#[test]
fn incomplete_and_ambiguous_batches_are_rejected_before_repository_io() {
    let mut incomplete = batch();
    incomplete.mode = SyncMode::FullSnapshot {
        complete: false,
        deletion_policy: DeletionPolicy::Approved,
    };
    assert!(matches!(incomplete.validate(), Err(BatchError::Shape(_))));
    let mut duplicate = batch();
    duplicate.records = vec![
        SyncRecord::Delete {
            external_uid: "same".into()
        };
        2
    ];
    assert!(duplicate.validate().is_err());
    let mut empty = batch();
    empty.records = vec![SyncRecord::Delete {
        external_uid: String::new(),
    }];
    assert!(empty.validate().is_err());
    let mut negative = batch();
    negative.base_revision = -1;
    assert!(negative.validate().is_err());
    let mut no_source = batch();
    no_source.source_id.clear();
    assert!(no_source.validate().is_err());
}

#[test]
fn batch_size_is_bounded_and_complete_empty_snapshots_are_explicitly_valid() {
    let mut large = batch();
    large.records = (0..=MAX_BATCH_RECORDS)
        .map(|n| SyncRecord::Delete {
            external_uid: n.to_string(),
        })
        .collect();
    assert!(large.validate().is_err());
    large.records.pop();
    assert!(large.validate().is_ok());
    let empty = SyncBatch {
        mode: SyncMode::FullSnapshot {
            complete: true,
            deletion_policy: DeletionPolicy::Limit(0),
        },
        ..batch()
    };
    assert!(empty.validate().is_ok());
}
