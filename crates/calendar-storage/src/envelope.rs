//! Standard-calendar-source/v1 snapshot and delta batches, single- or multi-page. Authorization, source state, receipt and commit
//! are one write transaction, so revocation, disabling or a concurrent edit cannot race a batch.
use crate::{
    security::digest,
    sync::{apply, prepare, SyncError},
    Storage,
};
use calendar_application::{
    Clock, DeletionPolicy, EventV1, IngestPolicy, SyncBatch, SyncMode, SyncRecord,
};
use sha2::{Digest, Sha256};

/// More deletions than this, or emptying a non-empty source, needs separate administrator approval.
pub const SNAPSHOT_DELETION_LIMIT: usize = 100;
/// Bounds of one collection across all its pages.
pub const MAX_COLLECTION_OPERATIONS: i64 = 10_000;
pub const MAX_COLLECTION_BYTES: i64 = 32 * 1024 * 1024;
/// An unfinished collection is discarded after this long.
pub const PAGE_TTL_MS: i64 = 3_600_000;

macro_rules! push_source {
    () => {
        "s.config_revision>0 AND s.enabled=1 AND s.retired_at IS NULL AND json_extract(s.config_json,'$.enabled')=1 AND json_extract(s.config_json,'$.mode')='push-only' AND json_extract(s.config_json,'$.adapter')='standard-calendar'"
    };
}
macro_rules! capability {
    ($name:literal) => {
        concat!(
            "coalesce(json_extract(s.config_json,'$.capabilities.",
            $name,
            "')=1,0)"
        )
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EnvelopeError {
    #[error("authorization denied")]
    Denied,
    #[error("invalid batch")]
    Invalid,
    #[error("source is not an enabled push-only source accepting this batch")]
    NotAccepting,
    #[error("base revision or idempotency conflict")]
    Conflict,
    #[error("snapshot deletions need administrator approval")]
    ReviewRequired,
    #[error("collection exceeds its operation or byte budget")]
    Budget,
    #[error("receipt storage at capacity")]
    Capacity,
    #[error("envelope persistence failed")]
    Database,
}
impl From<sqlx::Error> for EnvelopeError {
    fn from(_: sqlx::Error) -> Self {
        Self::Database
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchMode {
    /// A complete collection; absent events are deleted.
    Snapshot,
    /// Explicit changes against the persisted checkpoint of a complete baseline.
    Delta { base_checkpoint: Option<String> },
}
#[derive(Debug, Clone)]
pub enum BatchOperation {
    Upsert {
        calendar_id: String,
        event: Box<EventV1>,
    },
    /// Only in a delta; the event must exist at exactly this revision.
    Delete {
        external_uid: String,
        expected_event_revision: i64,
    },
}
/// One validated single-page envelope. `digest` is the server SHA-256 of its JCS form.
#[derive(Debug, Clone)]
pub struct EnvelopeBatch {
    pub source_id: String,
    pub key: String,
    pub digest: [u8; 32],
    pub batch_id: String,
    pub mode: BatchMode,
    pub base_config_revision: i64,
    pub base_source_revision: i64,
    pub proposed_checkpoint: Option<String>,
    /// 1 for a single-page batch; otherwise pages of `collection_tag` arrive in order.
    pub pages: u32,
    pub collection_tag: Option<String>,
    pub operations: Vec<BatchOperation>,
}
/// Staged pages keep their operations in the envelope's own JSON shape.
fn encode(operations: &[BatchOperation]) -> Result<String, serde_json::Error> {
    let values = operations
        .iter()
        .map(|op| match op {
            BatchOperation::Upsert { calendar_id, event } => Ok(serde_json::json!({
                "op":"upsert","calendar_id":calendar_id,"event":serde_json::to_value(event)?})),
            BatchOperation::Delete {
                external_uid,
                expected_event_revision,
            } => Ok(
                serde_json::json!({"op":"delete","external_uid":external_uid,
                "expected_event_revision":expected_event_revision}),
            ),
        })
        .collect::<Result<Vec<_>, serde_json::Error>>()?;
    serde_json::to_string(&values)
}
fn decode(text: &str) -> Option<Vec<BatchOperation>> {
    let values: Vec<serde_json::Value> = serde_json::from_str(text).ok()?;
    values
        .into_iter()
        .map(|mut value| match value["op"].as_str()? {
            "upsert" => Some(BatchOperation::Upsert {
                calendar_id: value["calendar_id"].as_str()?.to_owned(),
                event: Box::new(serde_json::from_value(value["event"].take()).ok()?),
            }),
            "delete" => Some(BatchOperation::Delete {
                external_uid: value["external_uid"].as_str()?.to_owned(),
                expected_event_revision: value["expected_event_revision"].as_i64()?,
            }),
            _ => None,
        })
        .collect()
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchOutcome {
    Committed(BatchReceipt),
    /// The page is held until the collection is complete; nothing is visible yet.
    Staged {
        received_pages: u32,
        pages: u32,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchReceipt {
    pub batch_id: String,
    pub source_revision: i64,
    pub changed_events: i64,
    pub replayed: bool,
}
/// What a producer needs to fill `base_*`; never event content or configuration detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushSourceState {
    pub config_revision: i64,
    pub source_revision: i64,
    pub checkpoint: Option<String>,
    pub has_snapshot: bool,
}

type SourceRow = (i64, i64, Option<String>, Option<i64>, bool);
#[derive(sqlx::FromRow)]
struct Authorized {
    id: String,
    can_upsert: bool,
    can_delete: bool,
    config_revision: i64,
    revision: i64,
    checkpoint: Option<String>,
    has_snapshot: bool,
    push: bool,
    snapshot: bool,
    delta: bool,
    delete: bool,
}

impl Storage {
    pub async fn push_source_state(
        &self,
        secret: &[u8],
        source: &str,
        clock: &impl Clock,
    ) -> Result<PushSourceState, EnvelopeError> {
        let token_hash = digest("api", secret).map_err(|_| EnvelopeError::Denied)?;
        let mut tx = self.pool.begin().await?;
        let now = clock.utc_now_ms();
        let row: Option<SourceRow> = sqlx::query_as(concat!(
            "SELECT s.config_revision,s.revision,s.committed_checkpoint,s.snapshot_revision,(",
            push_source!(),
            ") AND (",
            capability!("snapshot"),
            " OR ",
            capability!("delta"),
            ")",
            " FROM api_tokens t JOIN sources s ON s.id=t.source_id WHERE t.token_digest=? ",
            "AND t.source_id=? AND t.revoked_at IS NULL AND (t.expires_at IS NULL OR t.expires_at>?)"
        ))
        .bind(token_hash)
        .bind(source)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        let (config_revision, source_revision, checkpoint, snapshot, accepting) =
            row.ok_or(EnvelopeError::Denied)?;
        if !accepting {
            return Err(EnvelopeError::NotAccepting);
        }
        Ok(PushSourceState {
            config_revision,
            source_revision,
            checkpoint,
            has_snapshot: snapshot.is_some(),
        })
    }

    pub async fn ingest_batch(
        &self,
        secret: &[u8],
        batch: EnvelopeBatch,
        policy: IngestPolicy,
        clock: &impl Clock,
    ) -> Result<BatchOutcome, EnvelopeError> {
        let paged = batch.pages > 1;
        if !policy.valid()
            || batch.key.is_empty()
            || batch.key.len() > 128
            || !batch.key.bytes().all(|b| b.is_ascii_graphic())
            || batch.pages == 0
            || batch.pages > 32
            || (paged && batch.collection_tag.is_none())
        {
            return Err(EnvelopeError::Invalid);
        }
        let token_hash = digest("api", secret).map_err(|_| EnvelopeError::Denied)?;
        let key = Sha256::digest(batch.key.as_bytes()).to_vec();
        let snapshot = batch.mode == BatchMode::Snapshot;
        let deletes = batch
            .operations
            .iter()
            .any(|op| matches!(op, BatchOperation::Delete { .. }));
        let upserts = batch
            .operations
            .iter()
            .any(|op| matches!(op, BatchOperation::Upsert { .. }));
        if snapshot && deletes {
            return Err(EnvelopeError::Invalid);
        }
        // Pages of one collection must agree on everything but their operations.
        let header = serde_json::to_string(&(
            snapshot,
            batch.pages,
            batch.base_config_revision,
            batch.base_source_revision,
            match &batch.mode {
                BatchMode::Snapshot => None,
                BatchMode::Delta { base_checkpoint } => Some(base_checkpoint),
            },
            &batch.proposed_checkpoint,
        ))
        .map_err(|_| EnvelopeError::Invalid)?;
        let page_json = if paged {
            Some(encode(&batch.operations).map_err(|_| EnvelopeError::Invalid)?)
        } else {
            None
        };
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        // Sampled after the lock: an expired token must not survive lock contention.
        let now = clock.utc_now_ms();
        let expires_at = now
            .checked_add(policy.retention_ms)
            .ok_or(EnvelopeError::Invalid)?;
        let token: Option<Authorized> = sqlx::query_as(concat!(
            "SELECT t.id,t.can_upsert,t.can_delete,s.config_revision,s.revision,",
            "s.committed_checkpoint AS checkpoint,s.snapshot_revision IS NOT NULL AS has_snapshot,(",
            push_source!(),
            ") AS push,",
            capability!("snapshot"),
            " AS snapshot,",
            capability!("delta"),
            " AS delta,",
            capability!("delete"),
            " AS \"delete\" FROM api_tokens t JOIN sources s ON s.id=t.source_id WHERE t.token_digest=? ",
            "AND t.source_id=? AND t.revoked_at IS NULL AND (t.expires_at IS NULL OR t.expires_at>?)"
        ))
        .bind(token_hash)
        .bind(&batch.source_id)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;
        let source = token.ok_or(EnvelopeError::Denied)?;
        // A snapshot deletes absent events, so it needs both permissions; a delta needs the
        // permission of each kind of operation it contains. Checked for every page.
        if ((snapshot || upserts) && !source.can_upsert)
            || ((snapshot || deletes) && !source.can_delete)
        {
            return Err(EnvelopeError::Denied);
        }
        // Checked before any receipt: a disabled or retired source cannot replay one.
        let accepts = if snapshot {
            source.snapshot
        } else {
            source.delta && (!deletes || source.delete)
        };
        if !source.push || !accepts {
            return Err(EnvelopeError::NotAccepting);
        }
        let token_id = source.id;
        sqlx::query("DELETE FROM envelope_receipts WHERE expires_at<=?")
            .bind(now)
            .execute(&mut *tx)
            .await?;
        // A collection expires as a unit, so no gap can break arrival-order numbering.
        sqlx::query("DELETE FROM envelope_pages WHERE (source_id,collection_tag) IN (SELECT source_id,collection_tag FROM envelope_pages WHERE expires_at<=?)")
            .bind(now)
            .execute(&mut *tx)
            .await?;
        let previous: Option<(Vec<u8>, String, i64, i64)> = sqlx::query_as(
            "SELECT request_digest,batch_id,source_revision,changed_events FROM envelope_receipts WHERE token_id=? AND key_digest=?",
        )
        .bind(&token_id)
        .bind(&key)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((old, batch_id, source_revision, changed_events)) = previous {
            if old != batch.digest {
                return Err(EnvelopeError::Conflict);
            }
            tx.commit().await?;
            return Ok(BatchOutcome::Committed(BatchReceipt {
                batch_id,
                source_revision,
                changed_events,
                replayed: true,
            }));
        }
        let held: Option<(Vec<u8>, String, String, i64)> = sqlx::query_as(
            "SELECT request_digest,source_id,collection_tag,pages FROM envelope_pages WHERE token_id=? AND key_digest=?",
        )
        .bind(&token_id)
        .bind(&key)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((old, source_id, tag, pages)) = held {
            if old != batch.digest {
                return Err(EnvelopeError::Conflict);
            }
            let received: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM envelope_pages WHERE source_id=? AND collection_tag=?",
            )
            .bind(source_id)
            .bind(tag)
            .fetch_one(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(BatchOutcome::Staged {
                received_pages: received as u32,
                pages: pages as u32,
            });
        }
        let live: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM envelope_receipts)+(SELECT count(*) FROM ingest_requests WHERE expires_at>?)",
        )
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
        if live >= policy.max_records {
            return Err(EnvelopeError::Capacity);
        }
        if source.config_revision != batch.base_config_revision
            || source.revision != batch.base_source_revision
        {
            return Err(EnvelopeError::Conflict);
        }
        if let BatchMode::Delta { base_checkpoint } = &batch.mode {
            // A delta applies only on top of a complete baseline at the persisted checkpoint.
            if !source.has_snapshot || *base_checkpoint != source.checkpoint {
                return Err(EnvelopeError::Conflict);
            }
        }
        let mut operations = batch.operations;
        // (token, key digest, request digest) of earlier pages, which share the final receipt.
        let mut held_keys: Vec<(String, Vec<u8>, Vec<u8>)> = Vec::new();
        if let (Some(tag), Some(page_json)) =
            (batch.collection_tag.as_ref().filter(|_| paged), page_json)
        {
            type Page = (String, Vec<u8>, Vec<u8>, String, String, i64, i64);
            let earlier: Vec<Page> = sqlx::query_as("SELECT token_id,key_digest,request_digest,header,operations_json,operation_count,bytes FROM envelope_pages WHERE source_id=? AND collection_tag=? ORDER BY page_no")
                .bind(&batch.source_id).bind(tag).fetch_all(&mut *tx).await?;
            if earlier.is_empty() {
                // One open collection per source: a new one discards an unfinished one.
                sqlx::query("DELETE FROM envelope_pages WHERE source_id=?")
                    .bind(&batch.source_id)
                    .execute(&mut *tx)
                    .await?;
            } else if earlier[0].3 != header {
                return Err(EnvelopeError::Conflict);
            }
            let count = operations.len() as i64 + earlier.iter().map(|p| p.5).sum::<i64>();
            let bytes = page_json.len() as i64 + earlier.iter().map(|p| p.6).sum::<i64>();
            if count > MAX_COLLECTION_OPERATIONS || bytes > MAX_COLLECTION_BYTES {
                return Err(EnvelopeError::Budget);
            }
            let page_no = earlier.len() as u32 + 1;
            if page_no < batch.pages {
                let held_until = now.checked_add(PAGE_TTL_MS).ok_or(EnvelopeError::Invalid)?;
                sqlx::query("INSERT INTO envelope_pages(source_id,collection_tag,page_no,pages,token_id,key_digest,request_digest,batch_id,header,operations_json,operation_count,bytes,expires_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)")
                    .bind(&batch.source_id).bind(tag).bind(page_no).bind(batch.pages).bind(&token_id)
                    .bind(&key).bind(batch.digest.as_slice()).bind(&batch.batch_id).bind(&header)
                    .bind(&page_json).bind(operations.len() as i64).bind(page_json.len() as i64).bind(held_until)
                    .execute(&mut *tx).await?;
                tx.commit().await?;
                return Ok(BatchOutcome::Staged {
                    received_pages: page_no,
                    pages: batch.pages,
                });
            }
            // Every page key gets the collection receipt, so all of them must fit.
            if live + earlier.len() as i64 >= policy.max_records {
                return Err(EnvelopeError::Capacity);
            }
            // The final page: assemble the whole collection in arrival order. Each earlier page
            // is committed only while its own token is still valid and permitted for it.
            let mut all = Vec::with_capacity(count as usize);
            for page in earlier {
                let ops = decode(&page.4).ok_or(EnvelopeError::Database)?;
                let permitted: Option<(bool, bool)> = sqlx::query_as("SELECT can_upsert,can_delete FROM api_tokens WHERE id=? AND source_id=? AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at>?)")
                    .bind(&page.0).bind(&batch.source_id).bind(now)
                    .fetch_optional(&mut *tx).await?;
                let page_upserts = ops
                    .iter()
                    .any(|op| matches!(op, BatchOperation::Upsert { .. }));
                let page_deletes = ops
                    .iter()
                    .any(|op| matches!(op, BatchOperation::Delete { .. }));
                match permitted {
                    Some((can_upsert, can_delete))
                        if (can_upsert || !(snapshot || page_upserts))
                            && (can_delete || !(snapshot || page_deletes)) => {}
                    _ => return Err(EnvelopeError::Conflict),
                }
                all.extend(ops);
                held_keys.push((page.0, page.1, page.2));
            }
            all.extend(operations);
            operations = all;
            sqlx::query("DELETE FROM envelope_pages WHERE source_id=? AND collection_tag=?")
                .bind(&batch.source_id)
                .bind(tag)
                .execute(&mut *tx)
                .await?;
        }
        let mut expected = Vec::new();
        let mut records = Vec::with_capacity(operations.len());
        for operation in operations {
            records.push(match operation {
                BatchOperation::Upsert { calendar_id, event } => {
                    SyncRecord::Upsert { calendar_id, event }
                }
                BatchOperation::Delete {
                    external_uid,
                    expected_event_revision,
                } => {
                    if expected_event_revision < 1 {
                        return Err(EnvelopeError::Invalid);
                    }
                    expected.push((external_uid.clone(), expected_event_revision));
                    SyncRecord::Delete { external_uid }
                }
            });
        }
        let mut sync = SyncBatch {
            source_id: batch.source_id.clone(),
            base_revision: batch.base_source_revision,
            mode: if snapshot {
                SyncMode::FullSnapshot {
                    complete: true,
                    deletion_policy: DeletionPolicy::Limit(SNAPSHOT_DELETION_LIMIT),
                }
            } else {
                SyncMode::Delta
            },
            records,
            proposed_checkpoint: batch.proposed_checkpoint,
            verified_at: now,
        };
        let prepared = prepare(&mut sync).map_err(|_| EnvelopeError::Invalid)?;
        for (uid, revision) in &expected {
            let current: Option<i64> = sqlx::query_scalar(
                "SELECT revision FROM events WHERE source_id=? AND external_uid=?",
            )
            .bind(&batch.source_id)
            .bind(uid)
            .fetch_optional(&mut *tx)
            .await?;
            if current != Some(*revision) {
                return Err(EnvelopeError::Conflict);
            }
        }
        // A separate administrator approval of exactly these revisions lifts the deletion limit.
        let approved = snapshot
            && crate::deletion_approval::approved(
                &mut tx,
                &batch.source_id,
                source.config_revision,
                source.revision,
                now,
            )
            .await?;
        if approved {
            sync.mode = SyncMode::FullSnapshot {
                complete: true,
                deletion_policy: DeletionPolicy::Approved,
            };
        }
        if snapshot && !approved && prepared.is_empty() {
            let current: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM events WHERE source_id=? AND deleted_at IS NULL",
            )
            .bind(&batch.source_id)
            .fetch_one(&mut *tx)
            .await?;
            if current > 0 {
                return Err(EnvelopeError::ReviewRequired);
            }
        }
        let committed = apply(&mut tx, sync, prepared)
            .await
            .map_err(|error| match error {
                SyncError::ReviewRequired { .. } => EnvelopeError::ReviewRequired,
                SyncError::Conflict | SyncError::SourceUnavailable | SyncError::StaleSequence => {
                    EnvelopeError::Conflict
                }
                SyncError::Invalid(_) | SyncError::CalendarMismatch => EnvelopeError::Invalid,
                _ => EnvelopeError::Database,
            })?;
        let changed_events =
            i64::try_from(committed.changed_events).map_err(|_| EnvelopeError::Database)?;
        held_keys.push((token_id, key, batch.digest.to_vec()));
        for (token, key, request) in held_keys {
            sqlx::query("INSERT INTO envelope_receipts(token_id,key_digest,request_digest,batch_id,source_revision,changed_events,expires_at) VALUES(?,?,?,?,?,?,?)")
                .bind(token).bind(key).bind(request).bind(&batch.batch_id)
                .bind(committed.source_revision).bind(changed_events).bind(expires_at)
                .execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(BatchOutcome::Committed(BatchReceipt {
            batch_id: batch.batch_id,
            source_revision: committed.source_revision,
            changed_events,
            replayed: false,
        }))
    }
}
