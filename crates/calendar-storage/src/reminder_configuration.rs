//! Managed reminder/output persistence over the existing planner and delivery tables.
use crate::Storage;
use calendar_application::configuration::*;
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection};
use ConfigurationError as E;
type OutputRow = (String, i64, bool, Option<i64>, Option<String>);
fn output_record(id: &str, row: OutputRow) -> Result<BarkOutputConfigurationRecord, E> {
    let (json, generation, enabled, retired_at, credential) = row;
    let mut configuration: BarkOutputConfigurationV1 =
        serde_json::from_str(&json).map_err(|_| E::Storage)?;
    configuration.enabled = enabled;
    configuration.credential_id = credential.ok_or(E::Storage)?;
    Ok(BarkOutputConfigurationRecord {
        output_id: id.into(),
        generation,
        retired_at,
        configuration,
    })
}
fn next(
    condition: ConfigurationCondition,
    old: Option<(i64, Option<i64>, bool)>,
    enabled: bool,
) -> Result<(i64, &'static str), E> {
    match (condition, old) {
        (ConfigurationCondition::Absent, None) if !enabled => Ok((1, "create")),
        (ConfigurationCondition::Revision(expected), Some((actual, None, true)))
            if expected == actual && expected > 0 =>
        {
            Ok((actual.checked_add(1).ok_or(E::Conflict)?, "update"))
        }
        _ => Err(E::Conflict),
    }
}
fn validate_context(id: &str, actor: &str, now: i64) -> Result<(), E> {
    if !valid_id(id) || actor.is_empty() || now < 0 {
        Err(E::Invalid)
    } else {
        Ok(())
    }
}
async fn audit(
    tx: &mut SqliteConnection,
    kind: &str,
    id: &str,
    generation: i64,
    action: &str,
    actor: &str,
    now: i64,
) -> Result<(), E> {
    sqlx::query("INSERT INTO reminder_configuration_audit(entity_kind,entity_id,generation,action,actor_digest,changed_at) VALUES(?,?,?,?,?,?)")
        .bind(kind).bind(id).bind(generation).bind(action).bind(format!("{:x}",Sha256::digest(actor.as_bytes()))).bind(now).execute(tx).await.map_err(|_|E::Storage)?;
    Ok(())
}
impl Storage {
    pub async fn bark_output_configuration(
        &self,
        id: &str,
    ) -> Result<BarkOutputConfigurationRecord, E> {
        let row:Option<OutputRow>=sqlx::query_as("SELECT config_json,generation,enabled,retired_at,credential_id FROM outputs WHERE id=? AND managed=1")
            .bind(id).fetch_optional(self.pool()).await.map_err(|_|E::Storage)?;
        output_record(id, row.ok_or(E::NotFound)?)
    }
    pub async fn put_bark_output_configuration(
        &self,
        id: &str,
        condition: ConfigurationCondition,
        config: BarkOutputConfigurationV1,
        actor: &str,
        now: i64,
    ) -> Result<BarkOutputConfigurationRecord, E> {
        validate_context(id, actor, now)?;
        config.validate()?;
        let json = serde_json::to_string(&config).map_err(|_| E::Invalid)?;
        let mut tx = self
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|_| E::Storage)?;
        let old: Option<(i64, Option<i64>, bool)> =
            sqlx::query_as("SELECT generation,retired_at,managed FROM outputs WHERE id=?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| E::Storage)?;
        let (generation, action) = next(condition, old, config.enabled)?;
        let valid: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM credentials WHERE id=? AND kind='bark')",
        )
        .bind(&config.credential_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| E::Storage)?;
        if !valid {
            return Err(E::Invalid);
        }
        if action == "create" {
            sqlx::query("INSERT INTO outputs(id,kind,config_json,credential_id,generation,enabled,managed) VALUES(?,'bark',?,?,?,?,1)")
                .bind(id).bind(&json).bind(&config.credential_id).bind(generation).bind(config.enabled).execute(&mut *tx).await.map_err(|_|E::Storage)?;
        } else {
            sqlx::query("UPDATE outputs SET config_json=?,credential_id=?,generation=?,enabled=? WHERE id=?")
                .bind(&json).bind(&config.credential_id).bind(generation).bind(config.enabled).bind(id).execute(&mut *tx).await.map_err(|_|E::Storage)?;
        }
        audit(&mut tx, "output", id, generation, action, actor, now).await?;
        tx.commit().await.map_err(|_| E::Storage)?;
        Ok(BarkOutputConfigurationRecord {
            output_id: id.into(),
            generation,
            retired_at: None,
            configuration: config,
        })
    }
    pub async fn rule_configuration(&self, id: &str) -> Result<RuleConfigurationRecord, E> {
        let mut tx = self.pool().begin().await.map_err(|_| E::Storage)?;
        type RuleRow = (String, String, i64, bool, Option<i64>);
        let row:Option<RuleRow>=sqlx::query_as("SELECT display_name,policy_json,generation,enabled,retired_at FROM reminder_rules WHERE id=? AND managed=1")
            .bind(id).fetch_optional(&mut *tx).await.map_err(|_|E::Storage)?;
        let (name, json, generation, enabled, retired_at) = row.ok_or(E::NotFound)?;
        let policy = serde_json::from_str(&json).map_err(|_| E::Storage)?;
        let output_ids = sqlx::query_scalar(
            "SELECT output_id FROM rule_outputs WHERE rule_id=? AND enabled=1 ORDER BY output_id",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await
        .map_err(|_| E::Storage)?;
        tx.commit().await.map_err(|_| E::Storage)?;
        Ok(RuleConfigurationRecord {
            rule_id: id.into(),
            generation,
            retired_at,
            configuration: RuleConfigurationV1 {
                version: ConfigurationVersion::V1,
                name,
                enabled,
                policy,
                output_ids,
            },
        })
    }
    pub async fn put_rule_configuration(
        &self,
        id: &str,
        condition: ConfigurationCondition,
        mut config: RuleConfigurationV1,
        actor: &str,
        now: i64,
    ) -> Result<RuleConfigurationRecord, E> {
        validate_context(id, actor, now)?;
        config.validate()?;
        // Bindings are sets; canonical order makes PUT receipts and later GETs agree.
        config.output_ids.sort();
        config.policy.calendar_ids.sort();
        let json = serde_json::to_string(&config.policy).map_err(|_| E::Invalid)?;
        let mut tx = self
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|_| E::Storage)?;
        let old: Option<(i64, Option<i64>, bool)> =
            sqlx::query_as("SELECT generation,retired_at,managed FROM reminder_rules WHERE id=?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| E::Storage)?;
        let (generation, action) = next(condition, old, config.enabled)?;
        for calendar in &config.policy.calendar_ids {
            let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM calendars c JOIN sources s ON s.id=c.source_id WHERE c.id=? AND s.config_revision>0 AND s.retired_at IS NULL)")
                .bind(calendar).fetch_one(&mut *tx).await.map_err(|_|E::Storage)?;
            if !valid {
                return Err(E::Invalid);
            }
        }
        for output in &config.output_ids {
            let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM outputs WHERE id=? AND managed=1 AND retired_at IS NULL)")
                .bind(output).fetch_one(&mut *tx).await.map_err(|_|E::Storage)?;
            if !valid {
                return Err(E::Invalid);
            }
        }
        if action == "create" {
            sqlx::query("INSERT INTO reminder_rules(id,policy_json,generation,enabled,managed,display_name) VALUES(?,?,?,?,1,?)")
                .bind(id).bind(&json).bind(generation).bind(config.enabled).bind(&config.name).execute(&mut *tx).await.map_err(|_|E::Storage)?;
        } else {
            sqlx::query("UPDATE reminder_rules SET policy_json=?,generation=?,enabled=?,display_name=? WHERE id=?")
                .bind(&json).bind(generation).bind(config.enabled).bind(&config.name).bind(id).execute(&mut *tx).await.map_err(|_|E::Storage)?;
        }
        sqlx::query("DELETE FROM rule_calendars WHERE rule_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|_| E::Storage)?;
        for calendar in &config.policy.calendar_ids {
            sqlx::query("INSERT INTO rule_calendars(rule_id,calendar_id) VALUES(?,?)")
                .bind(id)
                .bind(calendar)
                .execute(&mut *tx)
                .await
                .map_err(|_| E::Storage)?;
        }
        // Do not remove historical bindings referenced by logical deliveries/watermarks.
        sqlx::query("UPDATE rule_outputs SET enabled=0 WHERE rule_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|_| E::Storage)?;
        for output in &config.output_ids {
            sqlx::query("INSERT INTO rule_outputs(rule_id,output_id,enabled) VALUES(?,?,1) ON CONFLICT(rule_id,output_id) DO UPDATE SET enabled=1")
                .bind(id).bind(output).execute(&mut *tx).await.map_err(|_|E::Storage)?;
        }
        audit(&mut tx, "rule", id, generation, action, actor, now).await?;
        tx.commit().await.map_err(|_| E::Storage)?;
        Ok(RuleConfigurationRecord {
            rule_id: id.into(),
            generation,
            retired_at: None,
            configuration: config,
        })
    }
    pub async fn retire_rule_configuration(
        &self,
        id: &str,
        generation: i64,
        actor: &str,
        now: i64,
    ) -> Result<(), E> {
        self.retire_reminder_configuration(id, generation, actor, now, false)
            .await
    }
    pub async fn retire_bark_output_configuration(
        &self,
        id: &str,
        generation: i64,
        actor: &str,
        now: i64,
    ) -> Result<(), E> {
        self.retire_reminder_configuration(id, generation, actor, now, true)
            .await
    }
    async fn retire_reminder_configuration(
        &self,
        id: &str,
        generation: i64,
        actor: &str,
        now: i64,
        output: bool,
    ) -> Result<(), E> {
        validate_context(id, actor, now)?;
        let next = generation
            .checked_add(1)
            .filter(|_| generation > 0)
            .ok_or(E::Conflict)?;
        let mut tx = self
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|_| E::Storage)?;
        let sql = if output {
            "UPDATE outputs SET generation=?,enabled=0,retired_at=? WHERE id=? AND generation=? AND managed=1 AND retired_at IS NULL"
        } else {
            "UPDATE reminder_rules SET generation=?,enabled=0,retired_at=? WHERE id=? AND generation=? AND managed=1 AND retired_at IS NULL"
        };
        let changed = sqlx::query(sql)
            .bind(next)
            .bind(now)
            .bind(id)
            .bind(generation)
            .execute(&mut *tx)
            .await
            .map_err(|_| E::Storage)?;
        if changed.rows_affected() != 1 {
            return Err(E::Conflict);
        }
        audit(
            &mut tx,
            if output { "output" } else { "rule" },
            id,
            next,
            "retire",
            actor,
            now,
        )
        .await?;
        tx.commit().await.map_err(|_| E::Storage)?;
        Ok(())
    }
    pub async fn list_reminder_configurations(
        &self,
        after: Option<&str>,
        output: bool,
    ) -> Result<ReminderConfigurationPage, E> {
        if after.is_some_and(|id| !valid_id(id)) {
            return Err(E::Invalid);
        }
        let sql = if output {
            "SELECT id,json_extract(config_json,'$.name') AS name,enabled,generation,retired_at FROM outputs WHERE managed=1 AND id>? ORDER BY id LIMIT 101"
        } else {
            "SELECT id,display_name AS name,enabled,generation,retired_at FROM reminder_rules WHERE managed=1 AND id>? ORDER BY id LIMIT 101"
        };
        let rows = sqlx::query(sql)
            .bind(after.unwrap_or(""))
            .fetch_all(self.pool())
            .await
            .map_err(|_| E::Storage)?;
        let more = rows.len() > 100;
        let items: Vec<_> = rows
            .into_iter()
            .take(100)
            .map(|r| {
                Ok(ReminderConfigurationSummary {
                    id: r.try_get("id")?,
                    name: r.try_get("name")?,
                    enabled: r.try_get("enabled")?,
                    generation: r.try_get("generation")?,
                    retired_at: r.try_get("retired_at")?,
                })
            })
            .collect::<Result<_, sqlx::Error>>()
            .map_err(|_| E::Storage)?;
        let next_after = if more {
            items.last().map(|s| s.id.clone())
        } else {
            None
        };
        Ok(ReminderConfigurationPage { items, next_after })
    }
}
impl ReminderConfigurationRepository for Storage {
    async fn planning_configurations(
        &self,
        after: Option<&str>,
    ) -> Result<PlanningConfigurationPage, E> {
        if after.is_some_and(|s| s.len() > 386) {
            return Err(E::Invalid);
        }
        let cursor = after.unwrap_or("::").split(':').collect::<Vec<_>>();
        if cursor.len() != 3 || (after.is_some() && cursor.iter().any(|id| !valid_id(id))) {
            return Err(E::Invalid);
        }
        let rows=sqlx::query("SELECT s.id AS source_id,s.revision,s.config_revision,json_extract(s.config_json,'$.mode') AS mode,json_extract(s.config_json,'$.freshness_sla_ms') AS freshness,r.id AS rule_id,r.generation AS rule_generation,o.id AS output_id,o.generation AS output_generation FROM sources s JOIN calendars c ON c.source_id=s.id AND c.selected=1 JOIN rule_calendars rc ON rc.calendar_id=c.id JOIN reminder_rules r ON r.id=rc.rule_id JOIN rule_outputs ro ON ro.rule_id=r.id AND ro.enabled=1 JOIN outputs o ON o.id=ro.output_id WHERE s.enabled=1 AND s.retired_at IS NULL AND s.config_revision>0 AND r.enabled=1 AND r.managed=1 AND r.retired_at IS NULL AND o.enabled=1 AND o.managed=1 AND o.retired_at IS NULL AND (s.id,r.id,o.id)>(?,?,?) GROUP BY s.id,r.id,o.id ORDER BY s.id,r.id,o.id LIMIT 101")
            .bind(cursor[0]).bind(cursor[1]).bind(cursor[2]).fetch_all(self.pool()).await.map_err(|_|E::Storage)?;
        let more = rows.len() > 100;
        let mut items = Vec::new();
        for row in rows.into_iter().take(100) {
            let mode: String = row.try_get("mode").map_err(|_| E::Storage)?;
            let source_mode =
                serde_json::from_value(serde_json::Value::String(mode)).map_err(|_| E::Storage)?;
            items.push(PlanningConfiguration {
                source_id: row.try_get("source_id").map_err(|_| E::Storage)?,
                source_revision: row.try_get("revision").map_err(|_| E::Storage)?,
                source_config_revision: row.try_get("config_revision").map_err(|_| E::Storage)?,
                source_mode,
                freshness_sla_ms: row.try_get("freshness").map_err(|_| E::Storage)?,
                rule_id: row.try_get("rule_id").map_err(|_| E::Storage)?,
                rule_generation: row.try_get("rule_generation").map_err(|_| E::Storage)?,
                output_id: row.try_get("output_id").map_err(|_| E::Storage)?,
                output_generation: row.try_get("output_generation").map_err(|_| E::Storage)?,
            });
        }
        let next_after = if more {
            items
                .last()
                .map(|s| format!("{}:{}:{}", s.source_id, s.rule_id, s.output_id))
        } else {
            None
        };
        Ok(PlanningConfigurationPage { items, next_after })
    }
    async fn output_for_delivery(
        &self,
        job_id: &str,
        output_id: &str,
    ) -> Result<Option<BarkOutputConfigurationRecord>, E> {
        let row:Option<OutputRow>=sqlx::query_as("SELECT o.config_json,o.generation,o.enabled,o.retired_at,o.credential_id FROM delivery_jobs j JOIN logical_deliveries l ON l.id=j.logical_delivery_id JOIN outputs o ON o.id=l.output_id JOIN reminder_rules r ON r.id=l.rule_id JOIN rule_outputs ro ON ro.rule_id=r.id AND ro.output_id=o.id JOIN events e ON e.id=l.event_id JOIN sources s ON s.id=e.source_id WHERE j.id=? AND o.id=? AND j.state='leased' AND j.output_revision=o.generation AND j.rule_revision=r.generation AND j.event_revision=e.revision AND o.managed=1 AND o.enabled=1 AND o.retired_at IS NULL AND r.enabled=1 AND ro.enabled=1 AND s.enabled=1 AND e.deleted_at IS NULL AND l.accepted_at IS NULL AND EXISTS(SELECT 1 FROM delivery_attempts a WHERE a.job_id=j.id AND a.lease_token=j.lease_token AND a.sent_at IS NOT NULL AND a.finished_at IS NULL)")
            .bind(job_id).bind(output_id).fetch_optional(self.pool()).await.map_err(|_|E::Storage)?;
        row.map(|row| output_record(output_id, row)).transpose()
    }
}
