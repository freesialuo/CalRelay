use crate::Storage;
use calendar_application::{IngestionError, RemoteAuthentication};

impl Storage {
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
