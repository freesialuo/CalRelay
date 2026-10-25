use crate::Storage;
use calendar_application::{IngestionError, RemoteAuthentication};

impl Storage {
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
        query.push(") AND config_revision>0 AND enabled=1 AND retired_at IS NULL AND json_extract(config_json,'$.enabled')=1 AND json_extract(config_json,'$.mode')='pull' AND json_extract(config_json,'$.adapter')='ics' AND json_extract(config_json,'$.capabilities.snapshot')=1");
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
