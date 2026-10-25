use std::collections::{HashMap, HashSet};

use calendar_application::{
    plan_events, Clock, EventV1, PlanningError, PlanningEventInput, PlanningRequest,
    ReminderPlanner, RulePolicy,
};
use serde_json::Value;

use crate::Storage;

// generation, event/rule/output revisions, due time, state, interpretation context
type JobVersion = (i64, i64, i64, i64, i64, String, Option<String>);

fn parse_timezone_policy(raw: &str) -> Result<Option<String>, PlanningError> {
    let val: Value = serde_json::from_str(raw)
        .map_err(|e| PlanningError::UnsupportedCapability(e.to_string()))?;
    let obj = val
        .as_object()
        .ok_or_else(|| PlanningError::UnsupportedCapability("invalid timezone_policy".into()))?;
    match obj.get("tzid") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(PlanningError::UnsupportedCapability(
            "invalid timezone_policy.tzid".into(),
        )),
    }
}

fn parse_rule_policy(raw: &str) -> Result<RulePolicy, PlanningError> {
    let val: Value = serde_json::from_str(raw)
        .map_err(|e| PlanningError::UnsupportedCapability(e.to_string()))?;
    let obj = val
        .as_object()
        .ok_or_else(|| PlanningError::UnsupportedCapability("invalid policy_json".into()))?;
    let calendar_ids = obj
        .get("calendar_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| PlanningError::UnsupportedCapability("missing calendar_ids".into()))?
        .iter()
        .map(|v| {
            v.as_str().map(str::to_owned).ok_or_else(|| {
                PlanningError::UnsupportedCapability("invalid calendar_ids entry".into())
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let lead_ms = obj
        .get("lead_ms")
        .and_then(Value::as_i64)
        .ok_or_else(|| PlanningError::UnsupportedCapability("missing lead_ms".into()))?;
    let all_day_time = obj
        .get("all_day_time")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| PlanningError::UnsupportedCapability("missing all_day_time".into()))?;
    Ok(RulePolicy {
        calendar_ids,
        lead_ms,
        all_day_time,
    })
}

pub(crate) fn db_err(error: sqlx::Error) -> PlanningError {
    match &error {
        sqlx::Error::PoolTimedOut => PlanningError::Busy,
        sqlx::Error::Database(e)
            if e.code()
                .and_then(|c| c.parse::<i32>().ok())
                .is_some_and(|c| matches!(c & 255, 5 | 6)) =>
        {
            PlanningError::Busy
        }
        _ => PlanningError::Database("planning storage failed".into()),
    }
}

impl ReminderPlanner for Storage {
    async fn reconcile<C: Clock + ?Sized>(
        &self,
        request: PlanningRequest,
        clock: &C,
    ) -> Result<(), PlanningError> {
        request.validate()?;
        let started = clock.monotonic_now_ms();
        let check_budget = || {
            if clock.monotonic_now_ms().saturating_sub(started) >= request.limits.max_elapsed_ms {
                Err(PlanningError::TimeBudgetExceeded)
            } else {
                Ok(())
            }
        };

        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(db_err)?;
        check_budget()?;

        let source: Option<(i64, bool)> =
            sqlx::query_as("SELECT revision, enabled FROM sources WHERE id = ?")
                .bind(&request.source_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        check_budget()?;
        match source {
            Some((rev, true)) if rev == request.base_revision => {}
            _ => return Err(PlanningError::Conflict),
        }

        let rule: Option<(String, i64, bool)> = sqlx::query_as(
            "SELECT policy_json, generation, enabled FROM reminder_rules WHERE id = ?",
        )
        .bind(&request.rule_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        check_budget()?;
        let policy_json = match rule {
            Some((json, gen, true)) if gen == request.rule_generation => json,
            _ => return Err(PlanningError::Conflict),
        };

        let output: Option<(i64, bool)> =
            sqlx::query_as("SELECT generation, enabled FROM outputs WHERE id = ?")
                .bind(&request.output_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        check_budget()?;
        match output {
            Some((gen, true)) if gen == request.output_generation => {}
            _ => return Err(PlanningError::Conflict),
        }

        let bound: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM rule_outputs WHERE rule_id = ? AND output_id = ? AND enabled=1)",
        )
        .bind(&request.rule_id)
        .bind(&request.output_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_err)?;
        check_budget()?;
        if !bound {
            return Err(PlanningError::Conflict);
        }

        let existing_wm: Option<(i64, i64, i64)> = sqlx::query_as(
            "SELECT source_revision, rule_revision, output_revision FROM planning_watermarks WHERE source_id = ? AND rule_id = ? AND output_id = ?",
        )
        .bind(&request.source_id)
        .bind(&request.rule_id)
        .bind(&request.output_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        check_budget()?;
        if let Some((src_rev, rule_rev, out_rev)) = existing_wm {
            if src_rev > request.base_revision
                || rule_rev > request.rule_generation
                || out_rev > request.output_generation
            {
                return Err(PlanningError::Conflict);
            }
        }

        let policy = parse_rule_policy(&policy_json)?;
        let allowed_calendars: HashSet<&str> =
            policy.calendar_ids.iter().map(String::as_str).collect();

        let calendar_rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT id, timezone_policy FROM calendars WHERE source_id = ? AND selected=1",
        )
        .bind(&request.source_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_err)?;
        check_budget()?;
        let mut calendar_tzids: HashMap<String, Option<String>> =
            HashMap::with_capacity(calendar_rows.len());
        for (cal_id, tz_json) in calendar_rows {
            check_budget()?;
            calendar_tzids.insert(cal_id, parse_timezone_policy(&tz_json)?);
        }

        let event_rows: Vec<(String, String, i64, String, Option<i64>)> = sqlx::query_as(
            "SELECT id, calendar_id, revision, document_json, deleted_at FROM events WHERE source_id = ? ORDER BY external_uid, id",
        )
        .bind(&request.source_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_err)?;
        check_budget()?;

        let mut inputs = Vec::new();
        let mut contexts = HashMap::new();
        let mut live_event_revisions: HashMap<String, i64> = HashMap::new();
        for (event_id, calendar_id, revision, document_json, deleted_at) in event_rows {
            check_budget()?;
            if deleted_at.is_some()
                || !allowed_calendars.contains(calendar_id.as_str())
                || !calendar_tzids.contains_key(&calendar_id)
            {
                continue;
            }
            let event: EventV1 = serde_json::from_str(&document_json)
                .map_err(|e| PlanningError::UnsupportedCapability(e.to_string()))?;
            let calendar_tzid = calendar_tzids.get(&calendar_id).cloned().flatten();
            let interpretation_tzid = if event.start.to_utc().is_ok_and(|instant| instant.is_none())
            {
                calendar_tzid.as_deref()
            } else {
                None
            };
            contexts.insert(
                event_id.clone(),
                serde_json::json!([interpretation_tzid, request.tzdata_version]).to_string(),
            );
            live_event_revisions.insert(event_id.clone(), revision);
            inputs.push(PlanningEventInput {
                event_id,
                event_revision: revision,
                calendar_tzid,
                event,
            });
        }

        check_budget()?;
        let planned = plan_events(&request, &policy, &inputs, clock)?;
        check_budget()?;

        let mut seen_occurrences: HashSet<(String, String)> = HashSet::new();
        let mut active_occurrences: HashSet<(String, String)> = HashSet::new();

        for occ in &planned.occurrences {
            check_budget()?;
            let context = &contexts[&occ.event_id];
            seen_occurrences.insert((occ.event_id.clone(), occ.original_key.clone()));
            sqlx::query(
                "INSERT INTO occurrences(
                     id, event_id, original_key, event_revision, start_utc, end_utc, local_date, resolved_tzid, cancelled
                 ) VALUES (lower(hex(randomblob(16))), ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(event_id, original_key) DO UPDATE SET
                     event_revision = excluded.event_revision,
                     start_utc = excluded.start_utc,
                     end_utc = excluded.end_utc,
                     local_date = excluded.local_date,
                     resolved_tzid = excluded.resolved_tzid,
                     cancelled = excluded.cancelled",
            )
            .bind(&occ.event_id)
            .bind(&occ.original_key)
            .bind(occ.event_revision)
            .bind(occ.start_utc)
            .bind(occ.end_utc)
            .bind(&occ.local_date)
            .bind(&occ.resolved_tzid)
            .bind(i64::from(occ.cancelled))
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            check_budget()?;

            if occ.cancelled {
                sqlx::query(
                    "UPDATE delivery_jobs SET state = 'cancelled'
                     WHERE state IN ('pending', 'retry_wait', 'leased')
                       AND logical_delivery_id IN (
                           SELECT id FROM logical_deliveries
                           WHERE event_id = ? AND original_key = ? AND rule_id = ? AND output_id = ? AND notification_kind = 'reminder'
                       )",
                )
                .bind(&occ.event_id)
                .bind(&occ.original_key)
                .bind(&request.rule_id)
                .bind(&request.output_id)
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
                check_budget()?;
                continue;
            }

            active_occurrences.insert((occ.event_id.clone(), occ.original_key.clone()));

            let existing_logical: Option<(String, Option<i64>)> = sqlx::query_as(
                "SELECT id, accepted_at FROM logical_deliveries
                 WHERE event_id = ? AND original_key = ? AND rule_id = ? AND output_id = ? AND notification_kind = 'reminder'",
            )
            .bind(&occ.event_id)
            .bind(&occ.original_key)
            .bind(&request.rule_id)
            .bind(&request.output_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_err)?;
            check_budget()?;

            let logical_id = match existing_logical {
                Some((_, Some(_))) => continue,
                Some((id, None)) => id,
                None => {
                    sqlx::query_scalar(
                        "INSERT INTO logical_deliveries(id, event_id, original_key, rule_id, output_id, notification_kind)
                         VALUES (lower(hex(randomblob(16))), ?, ?, ?, ?, 'reminder')
                         RETURNING id",
                    )
                    .bind(&occ.event_id)
                    .bind(&occ.original_key)
                    .bind(&request.rule_id)
                    .bind(&request.output_id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(db_err)?
                }
            };
            check_budget()?;

            let latest_job: Option<JobVersion> = sqlx::query_as(
                "SELECT generation, event_revision, rule_revision, output_revision, due_at, state, planning_context
                 FROM delivery_jobs
                 WHERE logical_delivery_id = ?
                 ORDER BY generation DESC
                 LIMIT 1",
            )
            .bind(&logical_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_err)?;
            check_budget()?;

            let next_generation = match latest_job {
                None => 1,
                Some((_, _, _, _, _, ref state, _)) if state == "accepted" => continue,
                Some((_, ev_rev, r_rev, o_rev, due, ref state, ref old_context))
                    if matches!(
                        state.as_str(),
                        "pending" | "retry_wait" | "leased" | "failed_permanent" | "expired"
                    ) && ev_rev == occ.event_revision
                        && r_rev == request.rule_generation
                        && o_rev == request.output_generation
                        && due == occ.due_at
                        && old_context.as_ref().is_none_or(|old| old == context) =>
                {
                    if old_context.is_none() {
                        sqlx::query(
                            "UPDATE delivery_jobs SET planning_context = ?
                             WHERE logical_delivery_id = ? AND planning_context IS NULL",
                        )
                        .bind(context)
                        .bind(&logical_id)
                        .execute(&mut *tx)
                        .await
                        .map_err(db_err)?;
                        check_budget()?;
                    }
                    continue;
                }
                Some((gen, _, _, _, _, _, _)) => {
                    sqlx::query(
                        "UPDATE delivery_jobs SET state = 'superseded'
                         WHERE logical_delivery_id = ? AND state IN ('pending', 'retry_wait', 'leased')",
                    )
                    .bind(&logical_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
                    check_budget()?;
                    gen.checked_add(1).ok_or(PlanningError::InvalidWindow)?
                }
            };

            sqlx::query(
                "INSERT INTO delivery_jobs(
                     id, logical_delivery_id, generation, event_revision, rule_revision, output_revision, due_at, state, next_attempt_at, planning_context
                 ) VALUES (lower(hex(randomblob(16))), ?, ?, ?, ?, ?, ?, 'pending', ?, ?)",
            )
            .bind(&logical_id)
            .bind(next_generation)
            .bind(occ.event_revision)
            .bind(request.rule_generation)
            .bind(request.output_generation)
            .bind(occ.due_at)
            .bind(occ.due_at)
            .bind(context)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            check_budget()?;
        }

        let existing_active_jobs: Vec<(String, String, String, i64, Option<String>)> =
            sqlx::query_as(
                "SELECT j.id, l.event_id, l.original_key, j.event_revision, j.planning_context
             FROM delivery_jobs j
             JOIN logical_deliveries l ON l.id = j.logical_delivery_id
             JOIN events e ON e.id = l.event_id
             WHERE e.source_id = ? AND l.rule_id = ? AND l.output_id = ?
               AND j.state IN ('pending', 'retry_wait', 'leased')",
            )
            .bind(&request.source_id)
            .bind(&request.rule_id)
            .bind(&request.output_id)
            .fetch_all(&mut *tx)
            .await
            .map_err(db_err)?;
        check_budget()?;

        for (job_id, event_id, original_key, job_event_rev, old_context) in existing_active_jobs {
            check_budget()?;
            let interpretation_changed = contexts.get(&event_id) != old_context.as_ref();
            let key = (event_id.clone(), original_key);
            if active_occurrences.contains(&key) {
                continue;
            }
            let should_cancel = match live_event_revisions.get(&event_id) {
                None => true,
                Some(&current_rev) => {
                    current_rev != job_event_rev
                        || seen_occurrences.contains(&key)
                        || interpretation_changed
                }
            };
            if should_cancel {
                sqlx::query("UPDATE delivery_jobs SET state = 'cancelled' WHERE id = ?")
                    .bind(&job_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
                check_budget()?;
            }
        }

        // The component tuple is the identity; preserve existing opaque scope IDs on replay.
        sqlx::query(
            "INSERT INTO planning_watermarks(
                 scope_id, source_id, rule_id, output_id, source_revision, rule_revision, output_revision, planned_until, tzdata_version, last_error
             ) VALUES (lower(hex(randomblob(16))), ?, ?, ?, ?, ?, ?, ?, ?, NULL)
             ON CONFLICT(source_id, rule_id, output_id) DO UPDATE SET
                 source_revision = excluded.source_revision,
                 rule_revision = excluded.rule_revision,
                 output_revision = excluded.output_revision,
                 planned_until = excluded.planned_until,
                 tzdata_version = excluded.tzdata_version,
                 last_error = NULL",
        )
        .bind(&request.source_id)
        .bind(&request.rule_id)
        .bind(&request.output_id)
        .bind(request.base_revision)
        .bind(request.rule_generation)
        .bind(request.output_generation)
        .bind(planned.planned_until)
        .bind(&request.tzdata_version)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        check_budget()?;
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }
}
