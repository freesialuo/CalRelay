//! Bounded, read-only management projections. Never select payloads, credentials or lease tokens.
use crate::{
    source_runtime::{SourceSyncStatus, StoredSyncStatus},
    Storage,
};
use calendar_application::configuration::{ConfigurationError as Error, SourceConfigurationV1};

#[derive(sqlx::FromRow)]
pub struct DeliveryDiagnostic {
    pub job_id: String,
    pub rule_id: String,
    pub output_id: String,
    pub state: String,
    pub due_at: i64,
    pub next_attempt_at: i64,
    pub accepted_at: Option<i64>,
    pub last_outcome: Option<String>,
    pub started_at: Option<i64>,
    pub sent_at: Option<i64>,
    pub finished_at: Option<i64>,
}
pub struct SourceDiagnostic {
    pub configuration: SourceConfigurationV1,
    pub retired_at: Option<i64>,
    pub last_verified_at: Option<i64>,
    pub snapshot_revision: Option<i64>,
    /// Last attempt under the current configuration revision; `None` when there was none.
    pub sync_status: Option<SourceSyncStatus>,
    pub deliveries: Vec<DeliveryDiagnostic>,
    pub more_deliveries: bool,
}
type StoredSourceState = (String, Option<i64>, Option<i64>, Option<i64>);

impl Storage {
    pub async fn source_diagnostic(&self, id: &str) -> Result<SourceDiagnostic, Error> {
        let mut tx = self.pool().begin().await.map_err(|_| Error::Storage)?;
        let row: Option<StoredSourceState> = sqlx::query_as(
            "SELECT config_json,retired_at,last_verified_at,snapshot_revision FROM sources WHERE id=? AND config_revision>0"
        ).bind(id).fetch_optional(&mut *tx).await.map_err(|_| Error::Storage)?;
        let (config, retired_at, last_verified_at, snapshot_revision) =
            row.ok_or(Error::NotFound)?;
        let mut configuration: SourceConfigurationV1 =
            serde_json::from_str(&config).map_err(|_| Error::Storage)?;
        configuration.validate().map_err(|_| Error::Storage)?;
        if retired_at.is_some() {
            configuration.enabled = false;
        }
        let status: Option<StoredSyncStatus> = sqlx::query_as(concat!(
            "SELECT t.config_revision,t.attempted_at,t.outcome,t.consecutive_failures,",
            "t.next_allowed_at FROM source_sync_status t JOIN sources s ON s.id=t.source_id ",
            "WHERE t.source_id=? AND t.config_revision=s.config_revision"
        ))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| Error::Storage)?;
        let sync_status = status
            .map(|row| SourceSyncStatus::from_row(row).ok_or(Error::Storage))
            .transpose()?;
        let mut deliveries: Vec<DeliveryDiagnostic> = sqlx::query_as(
            "SELECT j.id AS job_id,l.rule_id,l.output_id,j.state,j.due_at,j.next_attempt_at,l.accepted_at,a.outcome AS last_outcome,a.started_at,a.sent_at,a.finished_at FROM delivery_jobs j JOIN logical_deliveries l ON l.id=j.logical_delivery_id JOIN events e ON e.id=l.event_id LEFT JOIN delivery_attempts a ON a.id=(SELECT x.id FROM delivery_attempts x WHERE x.job_id=j.id ORDER BY x.attempt_no DESC LIMIT 1) WHERE e.source_id=? ORDER BY j.rowid DESC LIMIT 21"
        ).bind(id).fetch_all(&mut *tx).await.map_err(|_| Error::Storage)?;
        let more_deliveries = deliveries.len() > 20;
        deliveries.truncate(20);
        tx.commit().await.map_err(|_| Error::Storage)?;
        Ok(SourceDiagnostic {
            configuration,
            retired_at,
            last_verified_at,
            snapshot_revision,
            sync_status,
            deliveries,
            more_deliveries,
        })
    }
}
