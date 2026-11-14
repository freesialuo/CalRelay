//! Source-scoped producer tokens of push-only sources (#178). The secret is returned once at
//! issue and only its digest is stored; listings and audit never contain either.
use crate::{
    security::{issue, random},
    Storage,
};
use calendar_application::configuration::ConfigurationError as Error;
use calendar_application::SecretBytes;
use sha2::{Digest, Sha256};

/// Unrevoked, unexpired tokens one source may hold at a time.
pub const MAX_LIVE_TOKENS: i64 = 16;
/// Newest first; older tokens are reported through `more`.
pub const PAGE_SIZE: i64 = 100;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ProducerToken {
    pub token_id: String,
    pub can_upsert: bool,
    pub can_delete: bool,
    pub expires_at: Option<i64>,
    pub revoked_at: Option<i64>,
    pub issued_at: Option<i64>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProducerTokenPage {
    pub items: Vec<ProducerToken>,
    pub more: bool,
}
pub struct IssuedProducerToken {
    pub token: ProducerToken,
    pub secret: SecretBytes,
}

fn actor_digest(actor: &str) -> String {
    format!("{:x}", Sha256::digest(actor.as_bytes()))
}
async fn audit(
    tx: &mut sqlx::SqliteConnection,
    token: &str,
    source: &str,
    action: &str,
    actor: &str,
    now: i64,
) -> Result<(), Error> {
    sqlx::query("INSERT INTO producer_token_audit(token_id,source_id,action,actor_digest,changed_at) VALUES(?,?,?,?,?)")
        .bind(token).bind(source).bind(action).bind(actor_digest(actor)).bind(now)
        .execute(tx).await.map_err(|_| Error::Storage)?;
    Ok(())
}
/// Whether a managed source can hold producer tokens (push-only standard-calendar, not retired).
async fn source(tx: &mut sqlx::SqliteConnection, id: &str) -> Result<bool, Error> {
    let row: Option<bool> = sqlx::query_scalar("SELECT retired_at IS NULL AND json_extract(config_json,'$.mode')='push-only' AND json_extract(config_json,'$.adapter')='standard-calendar' FROM sources WHERE id=? AND config_revision>0")
        .bind(id).fetch_optional(tx).await.map_err(|_| Error::Storage)?;
    row.ok_or(Error::NotFound)
}

impl Storage {
    pub async fn list_producer_tokens(&self, source_id: &str) -> Result<ProducerTokenPage, Error> {
        let mut tx = self.pool().begin().await.map_err(|_| Error::Storage)?;
        source(&mut tx, source_id).await?;
        let mut items: Vec<ProducerToken> = sqlx::query_as("SELECT id AS token_id,can_upsert,can_delete,expires_at,revoked_at,issued_at FROM api_tokens WHERE source_id=? ORDER BY issued_at IS NULL,issued_at DESC,rowid DESC LIMIT ?")
            .bind(source_id).bind(PAGE_SIZE + 1).fetch_all(&mut *tx).await.map_err(|_| Error::Storage)?;
        tx.commit().await.map_err(|_| Error::Storage)?;
        let more = items.len() as i64 > PAGE_SIZE;
        items.truncate(PAGE_SIZE as usize);
        Ok(ProducerTokenPage { items, more })
    }

    pub async fn issue_producer_token(
        &self,
        source_id: &str,
        can_upsert: bool,
        can_delete: bool,
        expires_at: Option<i64>,
        actor: &str,
        now: i64,
    ) -> Result<IssuedProducerToken, Error> {
        if !(can_upsert || can_delete) || expires_at.is_some_and(|at| at <= now) {
            return Err(Error::Invalid);
        }
        let mut raw = [0u8; 12];
        random(&mut raw).map_err(|_| Error::Storage)?;
        let token_id = format!(
            "pt_{}",
            raw.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        let (secret, digest) = issue("api").map_err(|_| Error::Storage)?;
        let mut tx = self
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|_| Error::Storage)?;
        if !source(&mut tx, source_id).await? {
            return Err(Error::Conflict);
        }
        let live: i64 = sqlx::query_scalar("SELECT count(*) FROM api_tokens WHERE source_id=? AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at>?)")
            .bind(source_id).bind(now).fetch_one(&mut *tx).await.map_err(|_| Error::Storage)?;
        if live >= MAX_LIVE_TOKENS {
            return Err(Error::Conflict);
        }
        sqlx::query("INSERT INTO api_tokens(id,token_digest,source_id,can_upsert,can_delete,expires_at,issued_at) VALUES(?,?,?,?,?,?,?)")
            .bind(&token_id).bind(digest).bind(source_id).bind(can_upsert).bind(can_delete).bind(expires_at).bind(now)
            .execute(&mut *tx).await.map_err(|_| Error::Storage)?;
        audit(&mut tx, &token_id, source_id, "issue", actor, now).await?;
        tx.commit().await.map_err(|_| Error::Storage)?;
        Ok(IssuedProducerToken {
            token: ProducerToken {
                token_id,
                can_upsert,
                can_delete,
                expires_at,
                revoked_at: None,
                issued_at: Some(now),
            },
            secret,
        })
    }

    /// Repeating a revocation changes nothing and is not audited again.
    pub async fn revoke_producer_token(
        &self,
        source_id: &str,
        token_id: &str,
        actor: &str,
        now: i64,
    ) -> Result<(), Error> {
        let mut tx = self
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|_| Error::Storage)?;
        source(&mut tx, source_id).await?;
        let revoked: Option<Option<i64>> =
            sqlx::query_scalar("SELECT revoked_at FROM api_tokens WHERE id=? AND source_id=?")
                .bind(token_id)
                .bind(source_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| Error::Storage)?;
        match revoked {
            None => return Err(Error::NotFound),
            Some(Some(_)) => return Ok(()),
            Some(None) => {}
        }
        sqlx::query("UPDATE api_tokens SET revoked_at=? WHERE id=?")
            .bind(now)
            .bind(token_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| Error::Storage)?;
        audit(&mut tx, token_id, source_id, "revoke", actor, now).await?;
        tx.commit().await.map_err(|_| Error::Storage)
    }
}
