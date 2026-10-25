//! Aggregate persistence: policy, members, revision and audit commit atomically.
use crate::{security, Storage};
use calendar_application::{
    aggregate::*,
    configuration::{valid_id, ConfigurationCondition, SourceConfigurationV1, SourceMode},
    SecretBytes,
};
use sha2::{Digest, Sha256};
use sqlx::SqliteConnection;
fn failure(_: sqlx::Error) -> AggregateError {
    AggregateError::Storage
}
pub struct AggregateGrant {
    pub record: AggregateRecord,
    pub secret: SecretBytes,
}
#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    public_id: String,
    revision: i64,
    revoked_at: Option<i64>,
    configuration_json: String,
}
fn record(row: Row) -> Result<AggregateRecord, AggregateError> {
    let configuration: AggregateConfiguration =
        serde_json::from_str(&row.configuration_json).map_err(|_| AggregateError::Invalid)?;
    configuration.validate()?;
    Ok(AggregateRecord {
        subscription_id: row.id,
        public_id: row.public_id,
        revision: row.revision,
        revoked_at: row.revoked_at,
        configuration,
    })
}
async fn read(conn: &mut SqliteConnection, id: &str) -> Result<AggregateRecord, AggregateError> {
    record(sqlx::query_as("SELECT id,public_id,revision,revoked_at,configuration_json FROM aggregate_subscriptions WHERE id=?").bind(id).fetch_optional(conn).await.map_err(failure)?.ok_or(AggregateError::NotFound)?)
}
async fn audit(
    conn: &mut SqliteConnection,
    id: &str,
    revision: i64,
    action: &str,
    actor: &str,
    now: i64,
) -> Result<(), AggregateError> {
    sqlx::query("INSERT INTO aggregate_subscription_audit(subscription_id,revision,action,actor_digest,changed_at) VALUES (?,?,?,?,?)").bind(id).bind(revision).bind(action).bind(format!("{:x}",Sha256::digest(actor.as_bytes()))).bind(now).execute(conn).await.map_err(failure)?;
    Ok(())
}
#[derive(sqlx::FromRow)]
struct Source {
    id: String,
    config_json: String,
    config_revision: i64,
    revision: i64,
    enabled: bool,
    retired_at: Option<i64>,
    snapshot_revision: Option<i64>,
    last_verified_at: Option<i64>,
}
async fn sources(
    conn: &mut SqliteConnection,
    config: &AggregateConfiguration,
    live: bool,
    now: i64,
) -> Result<Vec<AggregateSourceFence>, AggregateError> {
    let mut fences = Vec::new();
    for member in &config.sources {
        let row:Source=sqlx::query_as("SELECT id,config_json,config_revision,revision,enabled,retired_at,snapshot_revision,last_verified_at FROM sources WHERE id=?")
            .bind(&member.source_id).fetch_optional(&mut *conn).await.map_err(failure)?.ok_or(AggregateError::Denied)?;
        if row.config_revision <= 0 || row.retired_at.is_some() {
            return Err(AggregateError::Denied);
        }
        let source: SourceConfigurationV1 =
            serde_json::from_str(&row.config_json).map_err(|_| AggregateError::Denied)?;
        source.validate().map_err(|_| AggregateError::Denied)?;
        if live && (!row.enabled || !source.enabled) {
            return Err(AggregateError::Denied);
        }
        let ids =
            serde_json::to_string(&member.calendar_ids).map_err(|_| AggregateError::Invalid)?;
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM calendars WHERE source_id=? AND selected=1 AND id IN (SELECT value FROM json_each(?))").bind(&row.id).bind(ids).fetch_one(&mut *conn).await.map_err(failure)?;
        if count != member.calendar_ids.len() as i64 {
            return Err(AggregateError::Denied);
        }
        let mut fresh_until = None;
        if live {
            if matches!(source.mode, SourceMode::Pull)
                && !row
                    .snapshot_revision
                    .is_some_and(|v| v > 0 && v <= row.revision)
            {
                return Err(AggregateError::Unavailable);
            }
            if let Some(sla) = source.freshness_sla_ms {
                let verified = row
                    .last_verified_at
                    .filter(|v| *v >= 0 && *v <= now)
                    .ok_or(AggregateError::Unavailable)?;
                let until = verified
                    .checked_add(i64::try_from(sla).map_err(|_| AggregateError::Unavailable)?)
                    .ok_or(AggregateError::Unavailable)?;
                if now > until {
                    return Err(AggregateError::Unavailable);
                }
                fresh_until = Some(until);
            }
        }
        let mut calendar_ids = member.calendar_ids.clone();
        calendar_ids.sort();
        fences.push(AggregateSourceFence {
            source_id: row.id,
            configuration_revision: row.config_revision,
            data_revision: row.revision,
            configuration_digest: Sha256::digest(row.config_json.as_bytes()).into(),
            calendar_ids,
            snapshot_revision: row.snapshot_revision,
            last_verified_at: row.last_verified_at,
            fresh_until,
        });
    }
    fences.sort_by(|a, b| a.source_id.cmp(&b.source_id));
    Ok(fences)
}
impl Storage {
    /// Disabled creation returns its capability exactly once; edits never rotate it implicitly.
    pub async fn put_aggregate(
        &self,
        id: &str,
        condition: ConfigurationCondition,
        config: AggregateConfiguration,
        actor: &str,
        now: i64,
    ) -> Result<(AggregateRecord, Option<SecretBytes>), AggregateError> {
        config.validate()?;
        if !valid_id(id) || now < 0 || actor.is_empty() {
            return Err(AggregateError::Invalid);
        }
        let json = serde_json::to_string(&config).map_err(|_| AggregateError::Invalid)?;
        if json.len() > 65536 {
            return Err(AggregateError::Invalid);
        }
        let mut tx = self
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(failure)?;
        let old: Option<(i64, Option<i64>)> =
            sqlx::query_as("SELECT revision,revoked_at FROM aggregate_subscriptions WHERE id=?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(failure)?;
        let (revision, created) = match (condition, old) {
            (ConfigurationCondition::Absent, None) if !config.enabled => (1, true),
            (ConfigurationCondition::Revision(n), Some((actual, None))) if n > 0 && n == actual => {
                (n.checked_add(1).ok_or(AggregateError::Conflict)?, false)
            }
            _ => return Err(AggregateError::Conflict),
        };
        if config.enabled && config.expires_at.is_some_and(|v| v <= now) {
            return Err(AggregateError::Invalid);
        }
        sources(&mut tx, &config, false, now).await?;
        let secret = if created {
            let (secret, hash) =
                security::issue("aggregate-feed").map_err(|_| AggregateError::Storage)?;
            sqlx::query("INSERT INTO aggregate_subscriptions(id,public_id,token_digest,configuration_json,revision) VALUES (?,lower(hex(randomblob(16))),?,?,1)").bind(id).bind(hash).bind(&json).execute(&mut *tx).await.map_err(failure)?;
            Some(secret)
        } else {
            sqlx::query(
                "UPDATE aggregate_subscriptions SET configuration_json=?,revision=? WHERE id=?",
            )
            .bind(&json)
            .bind(revision)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(failure)?;
            None
        };
        sqlx::query("DELETE FROM aggregate_subscription_sources WHERE subscription_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(failure)?;
        for member in &config.sources {
            sqlx::query("INSERT INTO aggregate_subscription_sources(subscription_id,source_id) VALUES (?,?)").bind(id).bind(&member.source_id).execute(&mut *tx).await.map_err(failure)?;
        }
        audit(
            &mut tx,
            id,
            revision,
            if created { "create" } else { "update" },
            actor,
            now,
        )
        .await?;
        let result = read(&mut tx, id).await?;
        tx.commit().await.map_err(failure)?;
        Ok((result, secret))
    }
    pub async fn aggregate(&self, id: &str) -> Result<AggregateRecord, AggregateError> {
        let mut c = self.pool().acquire().await.map_err(failure)?;
        read(&mut c, id).await
    }
    pub async fn aggregates(&self, after: Option<&str>) -> Result<AggregatePage, AggregateError> {
        if after.is_some_and(|s| !valid_id(s)) {
            return Err(AggregateError::Invalid);
        }
        let rows:Vec<Row>=sqlx::query_as("SELECT id,public_id,revision,revoked_at,configuration_json FROM aggregate_subscriptions WHERE id>? ORDER BY id LIMIT 101").bind(after.unwrap_or("")).fetch_all(self.pool()).await.map_err(failure)?;
        let more = rows.len() > 100;
        let mut items = Vec::new();
        for row in rows.into_iter().take(100) {
            let r = record(row)?;
            items.push(AggregateSummary {
                subscription_id: r.subscription_id,
                public_id: r.public_id,
                revision: r.revision,
                name: r.configuration.name,
                enabled: r.configuration.enabled,
                expires_at: r.configuration.expires_at,
                revoked_at: r.revoked_at,
            });
        }
        let next_after = if more {
            items.last().map(|i| i.subscription_id.clone())
        } else {
            None
        };
        Ok(AggregatePage { items, next_after })
    }
    pub async fn rotate_aggregate(
        &self,
        id: &str,
        expected: i64,
        actor: &str,
        now: i64,
    ) -> Result<AggregateGrant, AggregateError> {
        if expected <= 0 || expected == i64::MAX || now < 0 || actor.is_empty() {
            return Err(AggregateError::Invalid);
        }
        let (secret, hash) =
            security::issue("aggregate-feed").map_err(|_| AggregateError::Storage)?;
        let mut tx = self
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(failure)?;
        let changed=sqlx::query("UPDATE aggregate_subscriptions SET token_digest=?,revision=revision+1 WHERE id=? AND revision=? AND revoked_at IS NULL").bind(hash).bind(id).bind(expected).execute(&mut *tx).await.map_err(failure)?;
        if changed.rows_affected() != 1 {
            return Err(AggregateError::Conflict);
        }
        audit(&mut tx, id, expected + 1, "rotate", actor, now).await?;
        let record = read(&mut tx, id).await?;
        tx.commit().await.map_err(failure)?;
        Ok(AggregateGrant { record, secret })
    }
    pub async fn retire_aggregate(
        &self,
        id: &str,
        expected: i64,
        actor: &str,
        now: i64,
    ) -> Result<(), AggregateError> {
        let mut tx = self
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(failure)?;
        let mut r = read(&mut tx, id).await?;
        if expected <= 0
            || r.revision != expected
            || r.revoked_at.is_some()
            || now < 0
            || actor.is_empty()
        {
            return Err(AggregateError::Conflict);
        }
        let revision = expected.checked_add(1).ok_or(AggregateError::Conflict)?;
        r.configuration.enabled = false;
        sqlx::query("UPDATE aggregate_subscriptions SET configuration_json=?,revision=?,revoked_at=? WHERE id=?").bind(serde_json::to_string(&r.configuration).map_err(|_|AggregateError::Invalid)?).bind(revision).bind(now).bind(id).execute(&mut *tx).await.map_err(failure)?;
        audit(&mut tx, id, revision, "retire", actor, now).await?;
        tx.commit().await.map_err(failure)?;
        Ok(())
    }
}
async fn authorize(
    conn: &mut SqliteConnection,
    public_id: &str,
    secret: &[u8],
    now: i64,
) -> Result<AggregateAuthorization, AggregateError> {
    if now < 0 {
        return Err(AggregateError::Unavailable);
    }
    if public_id.len() != 32
        || !public_id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(AggregateError::Denied);
    }
    let hash = security::digest("aggregate-feed", secret).map_err(|_| AggregateError::Denied)?;
    let row:Row=sqlx::query_as("SELECT id,public_id,revision,revoked_at,configuration_json FROM aggregate_subscriptions WHERE public_id=? AND token_digest=? AND revoked_at IS NULL")
        .bind(public_id).bind(hash).fetch_optional(&mut *conn).await.map_err(failure)?.ok_or(AggregateError::Denied)?;
    let policy_digest = Sha256::digest(row.configuration_json.as_bytes()).into();
    let r = record(row).map_err(|_| AggregateError::Denied)?;
    if !r.configuration.enabled || r.configuration.expires_at.is_some_and(|v| v <= now) {
        return Err(AggregateError::Denied);
    }
    let members:Vec<String>=sqlx::query_scalar("SELECT source_id FROM aggregate_subscription_sources WHERE subscription_id=? ORDER BY source_id").bind(&r.subscription_id).fetch_all(&mut *conn).await.map_err(failure)?;
    let mut configured: Vec<_> = r
        .configuration
        .sources
        .iter()
        .map(|s| s.source_id.clone())
        .collect();
    configured.sort();
    if members != configured {
        return Err(AggregateError::Denied);
    }
    let sources = sources(conn, &r.configuration, true, now).await?;
    Ok(AggregateAuthorization {
        key: AggregateKey {
            subscription_id: r.subscription_id,
            revision: r.revision,
            policy_digest,
            sources,
        },
        configuration: r.configuration,
    })
}
#[derive(sqlx::FromRow)]
struct EventRow {
    event_id: String,
    revision: i64,
    changed_at: i64,
    deleted: bool,
    document_json: String,
}
impl AggregateReadRepository for Storage {
    async fn authorize_aggregate(
        &self,
        public_id: &str,
        secret: &[u8],
        now: i64,
    ) -> Result<AggregateAuthorization, AggregateError> {
        let mut tx = self.pool().begin().await.map_err(failure)?;
        let a = authorize(&mut tx, public_id, secret, now).await?;
        tx.commit().await.map_err(failure)?;
        Ok(a)
    }
    async fn aggregate_snapshot(
        &self,
        public_id: &str,
        secret: &[u8],
        now: i64,
    ) -> Result<AggregateSnapshot, AggregateError> {
        let mut tx = self.pool().begin().await.map_err(failure)?;
        let authorization = authorize(&mut tx, public_id, secret, now).await?;
        let mut remaining_count = 10000i64;
        let mut remaining_bytes = 16 * 1024 * 1024i64;
        let mut events = Vec::new();
        for source in &authorization.key.sources {
            let ids =
                serde_json::to_string(&source.calendar_ids).map_err(|_| AggregateError::Invalid)?;
            let (count,bytes):(i64,i64)=sqlx::query_as("SELECT count(*),coalesce(sum(length(CAST(document_json AS BLOB))),0) FROM events WHERE source_id=? AND calendar_id IN (SELECT value FROM json_each(?))")
                .bind(&source.source_id).bind(&ids).fetch_one(&mut *tx).await.map_err(failure)?;
            if count < 0 || bytes < 0 || count > remaining_count || bytes > remaining_bytes {
                return Err(AggregateError::Unavailable);
            }
            remaining_count -= count;
            remaining_bytes -= bytes;
            let rows:Vec<EventRow>=sqlx::query_as("SELECT e.id AS event_id,e.revision,r.changed_at,e.deleted_at IS NOT NULL AS deleted,e.document_json FROM events e JOIN event_revisions r ON r.event_id=e.id AND r.revision=e.revision WHERE e.source_id=? AND e.calendar_id IN (SELECT value FROM json_each(?)) ORDER BY e.id LIMIT 10001")
                .bind(&source.source_id).bind(&ids).fetch_all(&mut *tx).await.map_err(failure)?;
            if rows.len() as i64 != count {
                return Err(AggregateError::Storage);
            }
            events.extend(rows.into_iter().map(|r| AggregateEvent {
                event_id: r.event_id,
                revision: r.revision,
                changed_at: r.changed_at,
                deleted: r.deleted,
                document_json: r.document_json,
            }));
        }
        tx.commit().await.map_err(failure)?;
        Ok(AggregateSnapshot {
            authorization,
            events,
        })
    }
}
