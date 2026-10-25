//! Read-only feed repository. Authorization and calendar membership are never cached.
use crate::{security::digest, Storage};
use calendar_application::{FeedScope, SecretBytes, SecurityError};
use sha2::{Digest, Sha256};
use sqlx::SqliteConnection;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FeedKey {
    pub subscription_id: String,
    pub configuration_revision: i64,
    pub source_id: String,
    pub data_revision: i64,
    pub scope_digest: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedAuthorization {
    pub key: FeedKey,
    pub scope: FeedScope,
}
#[derive(sqlx::FromRow)]
struct AuthorizationRow {
    subscription_id: String,
    configuration_revision: i64,
    source_id: String,
    data_revision: i64,
    scope_json: String,
}
#[derive(sqlx::FromRow)]
pub struct FeedRecord {
    pub event_id: String,
    pub revision: i64,
    pub changed_at: i64,
    pub deleted: bool,
    pub document_json: String,
}
pub struct FeedSnapshot {
    pub authorization: FeedAuthorization,
    pub records: Vec<FeedRecord>,
}
fn failure(_: sqlx::Error) -> SecurityError {
    SecurityError::Storage
}
async fn check_calendars(
    connection: &mut SqliteConnection,
    source: &str,
    scope: &FeedScope,
) -> Result<(), SecurityError> {
    if let Some(ids) = scope.calendar_ids() {
        let json = serde_json::to_string(ids).map_err(|_| SecurityError::InvalidInput)?;
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM calendars WHERE source_id=? AND id IN (SELECT value FROM json_each(?))")
            .bind(source).bind(json).fetch_one(connection).await.map_err(failure)?;
        if count != ids.len() as i64 {
            return Err(SecurityError::Denied);
        }
    }
    Ok(())
}
async fn authorize(
    connection: &mut SqliteConnection,
    public_id: &str,
    secret: &[u8],
    now: i64,
) -> Result<FeedAuthorization, SecurityError> {
    let hash = digest("feed", secret)?;
    let row:AuthorizationRow=sqlx::query_as("SELECT f.id AS subscription_id,f.revision AS configuration_revision,f.source_id,s.revision AS data_revision,f.scope_json FROM feed_subscriptions f JOIN sources s ON s.id=f.source_id WHERE f.public_id=? AND f.token_digest=? AND f.revoked_at IS NULL AND (f.expires_at IS NULL OR f.expires_at>?) AND s.enabled=1")
        .bind(public_id).bind(hash).bind(now).fetch_optional(&mut *connection).await.map_err(failure)?.ok_or(SecurityError::Denied)?;
    let scope = FeedScope::parse(&row.scope_json).map_err(|_| SecurityError::Denied)?;
    check_calendars(connection, &row.source_id, &scope).await?;
    Ok(FeedAuthorization {
        key: FeedKey {
            subscription_id: row.subscription_id,
            configuration_revision: row.configuration_revision,
            source_id: row.source_id,
            data_revision: row.data_revision,
            scope_digest: Sha256::digest(row.scope_json.as_bytes()).into(),
        },
        scope,
    })
}
impl Storage {
    /// Create a restricted capability atomically; never publish a temporary legacy grant.
    pub async fn issue_scoped_feed_token(
        &self,
        id: &str,
        public_id: &str,
        source: &str,
        expires: Option<i64>,
        scope: &FeedScope,
    ) -> Result<SecretBytes, SecurityError> {
        if [id, public_id, source]
            .iter()
            .any(|s| s.is_empty() || s.len() > 256)
        {
            return Err(SecurityError::InvalidInput);
        }
        let scope = FeedScope::parse(&scope.to_json())?;
        let mut tx = self.pool().begin().await.map_err(failure)?;
        check_calendars(&mut tx, source, &scope).await?;
        let (secret, hash) = crate::security::issue("feed")?;
        sqlx::query("INSERT INTO feed_subscriptions(id,public_id,source_id,token_digest,scope_json,expires_at) VALUES (?,?,?,?,?,?)")
            .bind(id).bind(public_id).bind(source).bind(hash).bind(scope.to_json()).bind(expires).execute(&mut *tx).await.map_err(failure)?;
        tx.commit().await.map_err(failure)?;
        Ok(secret)
    }
    /// Full typed capability, including policy. The legacy source-only helper deliberately
    /// continues rejecting restricted scopes so old consumers cannot widen their access.
    pub async fn authorize_feed(
        &self,
        public_id: &str,
        secret: &[u8],
        now: i64,
    ) -> Result<FeedAuthorization, SecurityError> {
        let mut tx = self.pool().begin().await.map_err(failure)?;
        let authorization = authorize(&mut tx, public_id, secret, now).await?;
        tx.commit().await.map_err(failure)?;
        Ok(authorization)
    }
    /// Trusted configuration boundary (no HTTP management authentication here). CAS and
    /// calendar ownership checks share a transaction; cannot revive revoked subscriptions.
    pub async fn configure_feed_scope(
        &self,
        id: &str,
        expected_revision: i64,
        scope: &FeedScope,
    ) -> Result<(), SecurityError> {
        if !(0..i64::MAX).contains(&expected_revision) {
            return Err(SecurityError::InvalidInput);
        }
        let scope = FeedScope::parse(&scope.to_json())?;
        let mut tx = self.pool().begin().await.map_err(failure)?;
        let source:String=sqlx::query_scalar("SELECT source_id FROM feed_subscriptions WHERE id=? AND revision=? AND revoked_at IS NULL")
            .bind(id).bind(expected_revision).fetch_optional(&mut *tx).await.map_err(failure)?.ok_or(SecurityError::Conflict)?;
        check_calendars(&mut tx, &source, &scope).await?;
        let result=sqlx::query("UPDATE feed_subscriptions SET scope_json=?,revision=revision+1 WHERE id=? AND revision=? AND revoked_at IS NULL")
            .bind(scope.to_json()).bind(id).bind(expected_revision).execute(&mut *tx).await.map_err(failure)?;
        if result.rows_affected() != 1 {
            return Err(SecurityError::Conflict);
        }
        tx.commit().await.map_err(failure)?;
        Ok(())
    }
    /// Policy, source revision, membership and event revisions share one read snapshot.
    /// Tombstones retain their prior document; no reminder planning window is involved.
    pub async fn feed_snapshot(
        &self,
        public_id: &str,
        secret: &[u8],
        now: i64,
    ) -> Result<FeedSnapshot, SecurityError> {
        let mut tx = self.pool().begin().await.map_err(failure)?;
        let authorization = authorize(&mut tx, public_id, secret, now).await?;
        let calendars = authorization
            .scope
            .calendar_ids()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| SecurityError::InvalidInput)?;
        let (count,bytes):(i64,i64)=sqlx::query_as("SELECT count(*),coalesce(sum(length(CAST(document_json AS BLOB))),0) FROM events WHERE source_id=? AND (? IS NULL OR calendar_id IN (SELECT value FROM json_each(?)))")
            .bind(&authorization.key.source_id).bind(&calendars).bind(&calendars).fetch_one(&mut *tx).await.map_err(failure)?;
        if count > 10000 || bytes > 16 * 1024 * 1024 {
            return Err(SecurityError::Storage);
        }
        let records:Vec<FeedRecord>=sqlx::query_as("SELECT e.id AS event_id,e.revision,r.changed_at,e.deleted_at IS NOT NULL AS deleted,e.document_json FROM events e JOIN event_revisions r ON r.event_id=e.id AND r.revision=e.revision WHERE e.source_id=? AND (? IS NULL OR e.calendar_id IN (SELECT value FROM json_each(?))) ORDER BY e.id LIMIT 10001")
            .bind(&authorization.key.source_id).bind(&calendars).bind(&calendars).fetch_all(&mut *tx).await.map_err(failure)?;
        if count != records.len() as i64 {
            return Err(SecurityError::Storage);
        }
        tx.commit().await.map_err(failure)?;
        Ok(FeedSnapshot {
            authorization,
            records,
        })
    }
}
