use crate::Storage;
use calendar_application::{
    configuration::ConfigurationError, IngestionError, RemoteAuthentication,
};

macro_rules! active_pull {
    () => {
        "config_revision>0 AND enabled=1 AND retired_at IS NULL AND json_extract(config_json,'$.enabled')=1 AND json_extract(config_json,'$.mode')='pull' AND json_extract(config_json,'$.adapter')='ics' AND json_extract(config_json,'$.capabilities.snapshot')=1"
    };
}

/// A pending manual refresh. `sequence` identifies the exact request a scanner served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceRefreshRequest {
    pub config_revision: i64,
    pub sequence: i64,
    pub requested_at: i64,
}

impl Storage {
    /// Record at most one pending manual refresh for an active pull source. Repeats merge into
    /// the same row; sources that are never polled are rejected instead of being fake-polled.
    pub async fn request_source_refresh(
        &self,
        id: &str,
        now: i64,
    ) -> Result<(), ConfigurationError> {
        let written = sqlx::query(concat!(
            "INSERT INTO source_refresh_requests(source_id,config_revision,sequence,requested_at) ",
            "SELECT id,config_revision,1,? FROM sources WHERE id=? AND ",
            active_pull!(),
            " ON CONFLICT(source_id) DO UPDATE SET config_revision=excluded.config_revision,",
            "sequence=sequence+1,requested_at=excluded.requested_at"
        ))
        .bind(now)
        .bind(id)
        .execute(self.pool())
        .await
        .map_err(|_| ConfigurationError::Storage)?
        .rows_affected();
        if written > 0 {
            return Ok(());
        }
        let managed: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM sources WHERE id=? AND config_revision>0)",
        )
        .bind(id)
        .fetch_one(self.pool())
        .await
        .map_err(|_| ConfigurationError::Storage)?;
        Err(if managed {
            ConfigurationError::Conflict
        } else {
            ConfigurationError::NotFound
        })
    }
    pub async fn source_refresh_request(
        &self,
        id: &str,
    ) -> Result<Option<SourceRefreshRequest>, IngestionError> {
        let row: Option<(i64, i64, i64)> = sqlx::query_as(
            "SELECT config_revision,sequence,requested_at FROM source_refresh_requests WHERE source_id=?",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await
        .map_err(|_| IngestionError::Storage)?;
        Ok(row.map(
            |(config_revision, sequence, requested_at)| SourceRefreshRequest {
                config_revision,
                sequence,
                requested_at,
            },
        ))
    }
    /// Clear only the served request; one that arrived meanwhile has a newer sequence and stays.
    pub async fn clear_source_refresh(
        &self,
        id: &str,
        sequence: i64,
    ) -> Result<(), IngestionError> {
        sqlx::query("DELETE FROM source_refresh_requests WHERE source_id=? AND sequence=?")
            .bind(id)
            .bind(sequence)
            .execute(self.pool())
            .await
            .map(|_| ())
            .map_err(|_| IngestionError::Storage)
    }
    /// Revalidate bounded cached identities in one SQLite snapshot. No configuration or secret
    /// bodies leave storage, and active failed sources remain eligible for backoff retention.
    pub async fn active_pull_source_ids(
        &self,
        ids: &[String],
    ) -> Result<Vec<String>, IngestionError> {
        if ids.len() > 16_384 {
            return Err(IngestionError::Budget);
        }
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut query =
            sqlx::QueryBuilder::<sqlx::Sqlite>::new("SELECT id FROM sources WHERE id IN (");
        let mut separated = query.separated(",");
        for id in ids {
            separated.push_bind(id);
        }
        query.push(") AND ");
        query.push(active_pull!());
        query
            .build_query_scalar()
            .persistent(false)
            .fetch_all(self.pool())
            .await
            .map_err(|_| IngestionError::Storage)
    }
    /// Non-secret, immutable credential kind. Secret loading remains in RemoteFetcher.
    pub async fn source_authentication(
        &self,
        id: Option<&str>,
    ) -> Result<RemoteAuthentication, IngestionError> {
        let Some(id) = id else {
            return Ok(RemoteAuthentication::None);
        };
        let kind: Option<String> = sqlx::query_scalar("SELECT kind FROM credentials WHERE id=?")
            .bind(id)
            .fetch_optional(self.pool())
            .await
            .map_err(|_| IngestionError::Storage)?;
        match kind.as_deref() {
            Some("basic") => Ok(RemoteAuthentication::Basic {
                credential_id: id.into(),
            }),
            Some("bearer") => Ok(RemoteAuthentication::Bearer {
                credential_id: id.into(),
            }),
            _ => Err(IngestionError::Unavailable),
        }
    }
}
