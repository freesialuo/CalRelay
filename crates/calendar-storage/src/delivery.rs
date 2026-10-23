use crate::Storage;
use calendar_application::{
    Clock, DeliveryEnvelope, DeliveryError, DeliveryLease, DeliveryOutcome, DeliveryPolicy,
    DeliveryRepository,
};
use sqlx::{Sqlite, Transaction};

fn db_error(error: sqlx::Error) -> DeliveryError {
    match &error {
        sqlx::Error::Database(e)
            if e.code()
                .and_then(|c| c.parse::<i32>().ok())
                .is_some_and(|code| matches!(code & 255, 5 | 6)) =>
        {
            DeliveryError::Busy
        }
        _ => DeliveryError::Database,
    }
}

#[derive(sqlx::FromRow)]
struct Job {
    id: String,
    logical_id: String,
    state: String,
    event_revision: i64,
    rule_revision: i64,
    output_revision: i64,
    due_at: i64,
    deadline_at: Option<i64>,
    lease_owner: Option<String>,
    lease_token: Option<String>,
    lease_until: Option<i64>,
    planning_context: Option<String>,
    accepted_at: Option<i64>,
    document_json: String,
    deleted_at: Option<i64>,
    current_event: i64,
    source_enabled: bool,
    source_revision: i64,
    last_verified_at: Option<i64>,
    rule_enabled: bool,
    current_rule: i64,
    output_enabled: bool,
    current_output: i64,
    calendar_id: String,
    timezone_policy: String,
    policy_json: String,
    output_id: String,
    original_key: String,
    watermark_source: Option<i64>,
    watermark_rule: Option<i64>,
    watermark_output: Option<i64>,
    watermark_tzdata: Option<String>,
    attempt_no: i64,
    latest_generation: bool,
    occurrence_cancelled: bool,
}
const JOB_SELECT: &str = "SELECT j.id,j.logical_delivery_id AS logical_id,j.state,
 j.event_revision,j.rule_revision,j.output_revision,j.due_at,j.deadline_at,j.lease_owner,j.lease_token,j.lease_until,j.planning_context,
 l.accepted_at,e.document_json,e.deleted_at,e.revision AS current_event,
 s.enabled AS source_enabled,s.revision AS source_revision,s.last_verified_at,
 r.enabled AS rule_enabled,r.generation AS current_rule,o.enabled AS output_enabled,o.generation AS current_output,
 e.calendar_id,c.timezone_policy,r.policy_json,l.output_id,l.original_key,
 w.source_revision AS watermark_source,w.rule_revision AS watermark_rule,w.output_revision AS watermark_output,w.tzdata_version AS watermark_tzdata,
 NOT EXISTS(SELECT 1 FROM delivery_jobs newer WHERE newer.logical_delivery_id=j.logical_delivery_id AND newer.generation>j.generation) AS latest_generation,
 COALESCE((SELECT oc.cancelled FROM occurrences oc WHERE oc.event_id=l.event_id AND oc.original_key=l.original_key),0) AS occurrence_cancelled,
 (SELECT COALESCE(MAX(a.attempt_no),0) FROM delivery_attempts a WHERE a.job_id=j.id) AS attempt_no
 FROM delivery_jobs j JOIN logical_deliveries l ON l.id=j.logical_delivery_id
 JOIN events e ON e.id=l.event_id JOIN sources s ON s.id=e.source_id
 JOIN calendars c ON c.id=e.calendar_id JOIN reminder_rules r ON r.id=l.rule_id
 JOIN outputs o ON o.id=l.output_id
 LEFT JOIN planning_watermarks w ON w.source_id=s.id AND w.rule_id=r.id AND w.output_id=o.id";

async fn load(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<Job, DeliveryError> {
    sqlx::query_as(&format!("{JOB_SELECT} WHERE j.id=?"))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_error)?
        .ok_or(DeliveryError::StaleLease)
}

impl Job {
    fn owned(&self, lease: &DeliveryLease, now: i64) -> bool {
        self.lease_owner.as_ref() == Some(&lease.owner)
            && self.lease_token.as_ref() == Some(&lease.token)
            && self.lease_until.is_some_and(|until| until > now)
    }
    /// None means paused while source freshness/planning catches up; terminal reasons retire jobs.
    fn eligibility(
        &self,
        policy: &DeliveryPolicy,
        now: i64,
    ) -> Result<Option<&'static str>, DeliveryError> {
        if self.accepted_at.is_some() {
            return Ok(Some("superseded"));
        }
        let document: serde_json::Value =
            serde_json::from_str(&self.document_json).map_err(|_| DeliveryError::InvalidData)?;
        let rule: serde_json::Value =
            serde_json::from_str(&self.policy_json).map_err(|_| DeliveryError::InvalidData)?;
        if self.deleted_at.is_some()
            || document["status"] == "cancelled"
            || self.occurrence_cancelled
            || !self.source_enabled
            || !self.rule_enabled
            || !self.output_enabled
            || !rule["calendar_ids"]
                .as_array()
                .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(&self.calendar_id)))
        {
            return Ok(Some("cancelled"));
        }
        if !self.latest_generation
            || self.event_revision != self.current_event
            || self.rule_revision != self.current_rule
            || self.output_revision != self.current_output
        {
            return Ok(Some("superseded"));
        }
        let Some(context) = &self.planning_context else {
            return Ok(None);
        };
        let context: serde_json::Value =
            serde_json::from_str(context).map_err(|_| DeliveryError::InvalidData)?;
        let tz: serde_json::Value =
            serde_json::from_str(&self.timezone_policy).map_err(|_| DeliveryError::InvalidData)?;
        let interpretation = match document["start"]["kind"].as_str() {
            Some("date" | "floating") => tz.get("tzid").cloned().unwrap_or(serde_json::Value::Null),
            _ => serde_json::Value::Null,
        };
        if context[0] != interpretation || context[1].as_str() != Some(&policy.tzdata_version) {
            return Ok(None);
        }
        if self.watermark_source != Some(self.source_revision)
            || self.watermark_rule != Some(self.current_rule)
            || self.watermark_output != Some(self.current_output)
            || self.watermark_tzdata.as_deref() != Some(&policy.tzdata_version)
            || !self
                .last_verified_at
                .is_some_and(|time| time >= now.saturating_sub(policy.source_max_age_ms))
        {
            return Ok(None);
        }
        Ok(Some("ready"))
    }
}

async fn set_state(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    state: &str,
) -> Result<(), DeliveryError> {
    sqlx::query("UPDATE delivery_jobs SET state=? WHERE id=?")
        .bind(state)
        .bind(id)
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    Ok(())
}

/// Retry scheduling is capped by persisted deadline. A non-negative Retry-After is a lower bound.
fn retry_state(
    policy: &DeliveryPolicy,
    job: &Job,
    now: i64,
    retry_after: Option<i64>,
    entropy: u64,
) -> (&'static str, i64) {
    let delay = policy
        .retry_delay_ms(job.attempt_no, entropy)
        .max(retry_after.unwrap_or(0).max(0));
    let next = now.saturating_add(delay);
    if next >= job.deadline_at.unwrap_or(i64::MIN) {
        ("expired", next)
    } else if job.attempt_no >= policy.max_attempts {
        ("failed_permanent", next)
    } else {
        ("retry_wait", next)
    }
}

fn entropy(token: &str) -> u64 {
    token
        .bytes()
        .fold(0_u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64))
}

async fn record(
    tx: &mut Transaction<'_, Sqlite>,
    token: &str,
    now: i64,
    outcome: &str,
    disposition: &str,
    retry_after: Option<i64>,
) -> Result<(), DeliveryError> {
    let changed = sqlx::query("UPDATE delivery_attempts SET finished_at=?,outcome=?,disposition=?,retry_after_ms=? WHERE lease_token=? AND finished_at IS NULL")
        .bind(now).bind(outcome).bind(disposition).bind(retry_after.map(|ms| ms.max(0))).bind(token)
        .execute(&mut **tx).await.map_err(db_error)?.rows_affected();
    if changed != 1 {
        return Err(DeliveryError::StaleLease);
    }
    Ok(())
}

impl DeliveryRepository for Storage {
    async fn claim<C: Clock + ?Sized>(
        &self,
        owner: &str,
        policy: &DeliveryPolicy,
        clock: &C,
    ) -> Result<Option<DeliveryLease>, DeliveryError> {
        policy.validate()?;
        if owner.is_empty() || owner.len() > 128 {
            return Err(DeliveryError::InvalidPolicy);
        }
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(db_error)?;
        // Sample UTC after lock acquisition; lock waiting must not consume a new lease.
        let now = clock.utc_now_ms();
        let expired: Vec<String> = sqlx::query_scalar("SELECT id FROM delivery_jobs WHERE lease_until<=? AND lease_token IS NOT NULL ORDER BY lease_until,id LIMIT 64")
            .bind(now).fetch_all(&mut *tx).await.map_err(db_error)?;
        for id in expired {
            let job = load(&mut tx, &id).await?;
            let unfinished: Option<(String, Option<i64>)> = sqlx::query_as("SELECT lease_token,sent_at FROM delivery_attempts WHERE job_id=? AND finished_at IS NULL")
                .bind(&id).fetch_optional(&mut *tx).await.map_err(db_error)?;
            if let Some((token, sent_at)) = unfinished {
                let (state, next) = if job.state == "leased" {
                    retry_state(
                        policy,
                        &job,
                        now,
                        None,
                        entropy(job.lease_token.as_deref().unwrap_or_default()),
                    )
                } else {
                    (job.state.as_str(), now)
                };
                record(
                    &mut tx,
                    &token,
                    now,
                    if sent_at.is_some() {
                        "unknown"
                    } else {
                        "not_sent"
                    },
                    state,
                    None,
                )
                .await?;
                sqlx::query("UPDATE delivery_jobs SET state=?,next_attempt_at=? WHERE id=?")
                    .bind(state)
                    .bind(next)
                    .bind(&id)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_error)?;
            }
            sqlx::query("UPDATE delivery_jobs SET lease_owner=NULL,lease_token=NULL,lease_until=NULL WHERE id=?").bind(id).execute(&mut *tx).await.map_err(db_error)?;
        }
        // Scan a bounded candidate set. Paused sources must not starve other sources.
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT j.id FROM delivery_jobs j
             JOIN logical_deliveries l ON l.id=j.logical_delivery_id
             JOIN events e ON e.id=l.event_id JOIN sources s ON s.id=e.source_id
             JOIN calendars c ON c.id=e.calendar_id
             JOIN reminder_rules r ON r.id=l.rule_id JOIN outputs o ON o.id=l.output_id
             LEFT JOIN planning_watermarks w ON w.source_id=s.id AND w.rule_id=r.id AND w.output_id=o.id
             WHERE j.state IN ('pending','retry_wait') AND j.due_at<=? AND j.next_attempt_at<=?
             AND (COALESCE(j.deadline_at,j.due_at+?)<=?
               OR l.accepted_at IS NOT NULL OR e.deleted_at IS NOT NULL
               OR s.enabled=0 OR r.enabled=0 OR o.enabled=0
               OR j.event_revision<>e.revision OR j.rule_revision<>r.generation OR j.output_revision<>o.generation
               OR (w.source_revision=s.revision AND w.rule_revision=r.generation AND w.output_revision=o.generation
                 AND s.last_verified_at>=? AND w.tzdata_version=?
                 AND json_extract(j.planning_context,'$[1]')=?
                 AND json_extract(j.planning_context,'$[0]') IS
                   CASE WHEN json_extract(e.document_json,'$.start.kind') IN ('date','floating')
                   THEN json_extract(c.timezone_policy,'$.tzid') ELSE NULL END))
             ORDER BY j.next_attempt_at,j.id LIMIT 64")
            .bind(now).bind(now).bind(policy.catch_up_ms).bind(now)
            .bind(now.saturating_sub(policy.source_max_age_ms))
            .bind(&policy.tzdata_version).bind(&policy.tzdata_version)
            .fetch_all(&mut *tx).await.map_err(db_error)?;
        for id in ids {
            let mut job = load(&mut tx, &id).await?;
            let deadline = job
                .deadline_at
                .unwrap_or(job.due_at.saturating_add(policy.catch_up_ms));
            job.deadline_at = Some(deadline);
            if now >= deadline {
                set_state(&mut tx, &id, "expired").await?;
                continue;
            }
            match job.eligibility(policy, now)? {
                Some("ready") => {}
                Some(state) => {
                    set_state(&mut tx, &id, state).await?;
                    continue;
                }
                None => continue,
            }
            if job.attempt_no >= policy.max_attempts {
                set_state(&mut tx, &id, "failed_permanent").await?;
                continue;
            }
            let until = now
                .checked_add(policy.lease_ms)
                .ok_or(DeliveryError::InvalidPolicy)?;
            let token: String = sqlx::query_scalar("SELECT lower(hex(randomblob(16)))")
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
            let changed = sqlx::query("UPDATE delivery_jobs SET state='leased',deadline_at=?,lease_owner=?,lease_token=?,lease_until=? WHERE id=? AND state IN ('pending','retry_wait')")
                .bind(deadline).bind(owner).bind(&token).bind(until).bind(&id).execute(&mut *tx).await.map_err(db_error)?.rows_affected();
            if changed != 1 {
                return Err(DeliveryError::StaleLease);
            }
            sqlx::query("INSERT INTO delivery_attempts(id,job_id,attempt_no,lease_owner,lease_token,request_id,started_at) VALUES (lower(hex(randomblob(16))),?,?,?,?,lower(hex(randomblob(16))),?)")
                .bind(&id).bind(job.attempt_no.checked_add(1).ok_or(DeliveryError::InvalidData)?).bind(owner).bind(&token).bind(now).execute(&mut *tx).await.map_err(db_error)?;
            tx.commit().await.map_err(db_error)?;
            return Ok(Some(DeliveryLease {
                job_id: id,
                owner: owner.into(),
                token,
            }));
        }
        tx.commit().await.map_err(db_error)?;
        Ok(None)
    }

    async fn prepare<C: Clock + ?Sized>(
        &self,
        lease: &DeliveryLease,
        policy: &DeliveryPolicy,
        clock: &C,
    ) -> Result<Option<DeliveryEnvelope>, DeliveryError> {
        policy.validate()?;
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(db_error)?;
        let now = clock.utc_now_ms();
        let job = load(&mut tx, &lease.job_id).await?;
        if !job.owned(lease, now) {
            return Err(DeliveryError::StaleLease);
        }
        let valid = job.eligibility(policy, now)?;
        if job.state != "leased"
            || valid != Some("ready")
            || now >= job.deadline_at.unwrap_or(i64::MIN)
        {
            let state = if job.state != "leased" {
                job.state.as_str()
            } else if now >= job.deadline_at.unwrap_or(i64::MIN) {
                "expired"
            } else {
                valid.filter(|s| *s != "ready").unwrap_or("retry_wait")
            };
            record(&mut tx, &lease.token, now, "not_sent", state, None).await?;
            set_state(&mut tx, &job.id, state).await?;
            tx.commit().await.map_err(db_error)?;
            return Ok(None);
        }
        // Repeated prepare must not dispatch twice. Renew before I/O so the full timeout fits.
        let request_id: Option<String> = sqlx::query_scalar("UPDATE delivery_attempts SET sent_at=? WHERE lease_token=? AND sent_at IS NULL AND finished_at IS NULL RETURNING request_id")
            .bind(now).bind(&lease.token).fetch_optional(&mut *tx).await.map_err(db_error)?;
        let request_id = request_id.ok_or(DeliveryError::StaleLease)?;
        sqlx::query("UPDATE delivery_jobs SET lease_until=? WHERE id=?")
            .bind(
                now.checked_add(policy.lease_ms)
                    .ok_or(DeliveryError::InvalidPolicy)?,
            )
            .bind(&job.id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let event =
            serde_json::from_str(&job.document_json).map_err(|_| DeliveryError::InvalidData)?;
        tx.commit().await.map_err(db_error)?;
        Ok(Some(DeliveryEnvelope {
            job_id: job.id,
            request_id,
            output_id: job.output_id,
            event,
            original_key: job.original_key,
            due_at: job.due_at,
        }))
    }

    async fn finish<C: Clock + ?Sized>(
        &self,
        lease: &DeliveryLease,
        outcome: DeliveryOutcome,
        policy: &DeliveryPolicy,
        clock: &C,
    ) -> Result<(), DeliveryError> {
        policy.validate()?;
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(db_error)?;
        let now = clock.utc_now_ms();
        let job = load(&mut tx, &lease.job_id).await?;
        if !job.owned(lease, now) {
            return Err(DeliveryError::StaleLease);
        }
        let sent: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM delivery_attempts WHERE lease_token=? AND sent_at IS NOT NULL AND finished_at IS NULL)")
            .bind(&lease.token).fetch_one(&mut *tx).await.map_err(db_error)?;
        if !sent {
            return Err(DeliveryError::StaleLease);
        }
        let entropy = entropy(&lease.token);
        let (name, after, normal_state, next) = match outcome {
            DeliveryOutcome::Accepted => ("accepted", None, "accepted", now),
            DeliveryOutcome::PermanentFailure => {
                ("permanent_failure", None, "failed_permanent", now)
            }
            DeliveryOutcome::Retryable { retry_after_ms }
            | DeliveryOutcome::Unknown { retry_after_ms } => {
                let (state, next) = retry_state(policy, &job, now, retry_after_ms, entropy);
                (
                    if matches!(outcome, DeliveryOutcome::Unknown { .. }) {
                        "unknown"
                    } else {
                        "retryable"
                    },
                    retry_after_ms,
                    state,
                    next,
                )
            }
        };
        let state = if job.state != "leased" {
            job.state.as_str()
        } else if let Some(reason @ ("superseded" | "cancelled")) = job.eligibility(policy, now)? {
            reason
        } else {
            normal_state
        };
        record(&mut tx, &lease.token, now, name, state, after).await?;
        sqlx::query("UPDATE delivery_jobs SET state=?,next_attempt_at=?,lease_owner=NULL,lease_token=NULL,lease_until=NULL WHERE id=?")
            .bind(state).bind(next).bind(&job.id).execute(&mut *tx).await.map_err(db_error)?;
        if outcome == DeliveryOutcome::Accepted {
            // Actual acceptance also deduplicates a concurrently superseding generation.
            sqlx::query(
                "UPDATE logical_deliveries SET accepted_at=COALESCE(accepted_at,?) WHERE id=?",
            )
            .bind(now)
            .bind(&job.logical_id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
            sqlx::query("UPDATE delivery_jobs SET state='superseded' WHERE logical_delivery_id=? AND id<>? AND state IN ('pending','retry_wait','leased')")
                .bind(&job.logical_id).bind(&job.id).execute(&mut *tx).await.map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }
}
