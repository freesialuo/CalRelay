//! Application contracts. No database or network dependencies.

use std::{collections::HashSet, future::Future, time::Duration};

pub use calendar_domain::EventV1;
use calendar_domain::{
    expand_occurrences, validate_event, ExpandedOccurrence, ExpansionError, ExpansionOptions,
    Temporal, ValidationError,
};

pub const MAX_BATCH_RECORDS: usize = 10_000;
const MIN_UTC_MS: i64 = -62_167_219_200_000; // 0000-01-01T00:00:00Z
const MAX_UTC_MS: i64 = 253_402_300_799_999; // 9999-12-31T23:59:59.999Z

#[derive(Debug, Clone, Copy)]
pub enum DeletionPolicy {
    Limit(usize),
    /// Only a trusted caller that has obtained administrator approval may use this.
    Approved,
}

#[derive(Debug, Clone, Copy)]
pub enum SyncMode {
    Delta,
    FullSnapshot {
        complete: bool,
        deletion_policy: DeletionPolicy,
    },
}

#[derive(Debug, Clone)]
pub enum SyncRecord {
    Upsert {
        calendar_id: String,
        event: Box<EventV1>,
    },
    Delete {
        external_uid: String,
    },
}

impl SyncRecord {
    pub fn external_uid(&self) -> &str {
        match self {
            Self::Upsert { event, .. } => &event.external_uid,
            Self::Delete { external_uid } => external_uid,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SyncBatch {
    /// Injected by the trusted source context, never taken from an event document.
    pub source_id: String,
    pub base_revision: i64,
    pub mode: SyncMode,
    pub records: Vec<SyncRecord>,
    pub proposed_checkpoint: Option<String>,
    pub verified_at: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum BatchError {
    #[error("invalid sync batch: {0}")]
    Shape(&'static str),
    #[error("event failed domain validation")]
    InvalidEvent { errors: Vec<ValidationError> },
}

impl SyncBatch {
    /// Pure validation before acquiring a database write lock.
    pub fn validate(&self) -> Result<(), BatchError> {
        if self.source_id.is_empty()
            || self.base_revision < 0
            || self.records.len() > MAX_BATCH_RECORDS
        {
            return Err(BatchError::Shape("invalid source, revision or batch size"));
        }
        if matches!(
            self.mode,
            SyncMode::FullSnapshot {
                complete: false,
                ..
            }
        ) {
            return Err(BatchError::Shape("incomplete snapshot"));
        }
        let mut seen = HashSet::with_capacity(self.records.len());
        for record in &self.records {
            if record.external_uid().is_empty() || !seen.insert(record.external_uid()) {
                return Err(BatchError::Shape("empty or duplicate UID"));
            }
            match record {
                SyncRecord::Upsert { calendar_id, event } => {
                    if calendar_id.is_empty() {
                        return Err(BatchError::Shape("empty calendar"));
                    }
                    validate_event(event).map_err(|errors| BatchError::InvalidEvent { errors })?;
                }
                SyncRecord::Delete { .. } if !matches!(self.mode, SyncMode::Delta) => {
                    return Err(BatchError::Shape("snapshot contains explicit deletion"));
                }
                SyncRecord::Delete { .. } => {}
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommittedSourceRevision {
    pub source_revision: i64,
    pub changed_events: usize,
}

pub trait EventRepository {
    type Error: std::error::Error + Send + Sync + 'static;
    fn commit_sync(
        &self,
        batch: SyncBatch,
    ) -> impl Future<Output = Result<CommittedSourceRevision, Self::Error>> + Send;
}

pub trait Clock: Send + Sync {
    fn utc_now_ms(&self) -> i64;
    fn monotonic_now_ms(&self) -> u64;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanningLimits {
    pub max_occurrences: usize,
    pub max_iterations: usize,
    pub max_elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanningRequest {
    pub source_id: String,
    pub base_revision: i64,
    pub rule_id: String,
    pub rule_generation: i64,
    pub output_id: String,
    pub output_generation: i64,
    pub horizon_ms: i64,
    pub max_lead_ms: i64,
    pub extension_interval_ms: i64,
    pub tzdata_version: String,
    pub limits: PlanningLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanningError {
    #[error("planning window or request parameters are invalid")]
    InvalidWindow,
    #[error("unsupported capability: {0}")]
    UnsupportedCapability(String),
    #[error("occurrence limit exceeded")]
    OccurrenceLimitExceeded,
    #[error("iteration limit exceeded")]
    IterationLimitExceeded,
    #[error("planning time budget exceeded")]
    TimeBudgetExceeded,
    #[error("source, rule or output revision changed")]
    Conflict,
    #[error("database error: {0}")]
    Database(String),
}

pub trait ReminderPlanner {
    fn reconcile<C: Clock + ?Sized>(
        &self,
        request: PlanningRequest,
        clock: &C,
    ) -> impl Future<Output = Result<(), PlanningError>> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulePolicy {
    pub calendar_ids: Vec<String>,
    pub lead_ms: i64,
    pub all_day_time: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanningEventInput {
    pub event_id: String,
    pub event_revision: i64,
    pub calendar_tzid: Option<String>,
    pub event: EventV1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedOccurrence {
    pub event_id: String,
    pub event_revision: i64,
    pub original_key: String,
    pub start_utc: i64,
    pub end_utc: i64,
    pub local_date: String,
    pub resolved_tzid: String,
    pub cancelled: bool,
    pub due_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedBatch {
    pub window_start: i64,
    pub planned_until: i64,
    pub occurrences: Vec<PlannedOccurrence>,
}

impl PlanningRequest {
    pub fn validate(&self) -> Result<(), PlanningError> {
        if self.source_id.is_empty()
            || self.rule_id.is_empty()
            || self.output_id.is_empty()
            || self.tzdata_version.is_empty()
            || self.base_revision < 0
            || self.rule_generation < 1
            || self.output_generation < 1
            || self.max_lead_ms < 0
            || self.extension_interval_ms <= 0
            || self.horizon_ms <= 0
            || self.limits.max_occurrences == 0
            || self.limits.max_iterations == 0
            || self.limits.max_elapsed_ms == 0
        {
            return Err(PlanningError::InvalidWindow);
        }
        let min_horizon = self
            .max_lead_ms
            .checked_add(self.extension_interval_ms)
            .ok_or(PlanningError::InvalidWindow)?;
        if self.horizon_ms < min_horizon {
            return Err(PlanningError::InvalidWindow);
        }
        Ok(())
    }
}

fn check_elapsed<C: Clock + ?Sized>(
    clock: &C,
    started_monotonic: u64,
    max_elapsed_ms: u64,
) -> Result<(), PlanningError> {
    if clock.monotonic_now_ms().saturating_sub(started_monotonic) >= max_elapsed_ms {
        return Err(PlanningError::TimeBudgetExceeded);
    }
    Ok(())
}

fn normalize_all_day_time(raw: &str) -> Result<String, PlanningError> {
    let parts: Vec<&str> = raw.split(':').collect();
    let (h_str, m_str, s_str) = match parts.as_slice() {
        [h, m] => (*h, *m, "00"),
        [h, m, s] => (*h, *m, *s),
        _ => return Err(PlanningError::InvalidWindow),
    };
    if h_str.len() != 2 || m_str.len() != 2 || s_str.len() != 2 {
        return Err(PlanningError::InvalidWindow);
    }
    let h: u8 = h_str.parse().map_err(|_| PlanningError::InvalidWindow)?;
    let m: u8 = m_str.parse().map_err(|_| PlanningError::InvalidWindow)?;
    let s: u8 = s_str.parse().map_err(|_| PlanningError::InvalidWindow)?;
    if h > 23 || m > 59 || s > 59 {
        return Err(PlanningError::InvalidWindow);
    }
    Ok(format!("{h:02}:{m:02}:{s:02}"))
}

fn map_expansion_error(error: ExpansionError) -> PlanningError {
    match error {
        ExpansionError::OccurrenceLimitExceeded { .. } => PlanningError::OccurrenceLimitExceeded,
        ExpansionError::IterationLimitExceeded { .. } => PlanningError::IterationLimitExceeded,
        ExpansionError::InvalidWindow => PlanningError::InvalidWindow,
        other => PlanningError::UnsupportedCapability(other.to_string()),
    }
}

/// Pure, bounded occurrence and reminder due-time calculation across a batch of events.
pub fn plan_events<C: Clock + ?Sized>(
    request: &PlanningRequest,
    policy: &RulePolicy,
    events: &[PlanningEventInput],
    clock: &C,
) -> Result<PlannedBatch, PlanningError> {
    request.validate()?;
    if policy.lead_ms < 0 || policy.lead_ms > request.max_lead_ms {
        return Err(PlanningError::InvalidWindow);
    }
    let all_day_hms = normalize_all_day_time(&policy.all_day_time)?;

    let started_monotonic = clock.monotonic_now_ms();
    let window_start = clock.utc_now_ms();
    let planned_until = window_start
        .checked_add(request.horizon_ms)
        .ok_or(PlanningError::InvalidWindow)?;
    if !(MIN_UTC_MS..=MAX_UTC_MS).contains(&window_start)
        || !(MIN_UTC_MS..=MAX_UTC_MS).contains(&planned_until)
    {
        return Err(PlanningError::InvalidWindow);
    }

    let epoch = Temporal::Utc {
        value: "1970-01-01T00:00:00Z".to_owned(),
    }
    .to_utc()
    .expect("valid epoch")
    .expect("utc epoch");
    let to_utc = |ms: i64| {
        if ms >= 0 {
            epoch + Duration::from_millis(ms as u64)
        } else {
            epoch - Duration::from_millis(ms.unsigned_abs())
        }
    };
    let window_start_dt = to_utc(window_start);
    let window_end_dt = to_utc(planned_until);

    let mut remaining_occurrences = request.limits.max_occurrences;
    let mut remaining_iterations = request.limits.max_iterations;
    let mut occurrences = Vec::new();

    let last_recurring_idx = events.iter().rposition(|e| e.event.recurrence.is_some());

    for (idx, item) in events.iter().enumerate() {
        check_elapsed(clock, started_monotonic, request.limits.max_elapsed_ms)?;
        let options = ExpansionOptions {
            window_start: window_start_dt,
            window_end: window_end_dt,
            calendar_tzid: item.calendar_tzid.as_deref(),
            max_occurrences: remaining_occurrences.max(1),
            max_iterations: remaining_iterations.max(1),
        };
        let expanded = expand_occurrences(&item.event, &options).map_err(map_expansion_error)?;

        if let Some(rec) = &item.event.recurrence {
            let consumes_iterations =
                rec.rrule.is_some() || rec.rdates.as_ref().is_some_and(|r| !r.is_empty());
            if consumes_iterations {
                if remaining_iterations == 0 {
                    return Err(PlanningError::IterationLimitExceeded);
                }
                if last_recurring_idx.is_some_and(|last| idx < last) {
                    let mut low = 1usize;
                    let mut high = remaining_iterations;
                    while low < high {
                        let mid = low + (high - low) / 2;
                        let probe = ExpansionOptions {
                            max_iterations: mid,
                            ..options.clone()
                        };
                        match expand_occurrences(&item.event, &probe) {
                            Ok(_) => high = mid,
                            Err(ExpansionError::IterationLimitExceeded { .. }) => low = mid + 1,
                            Err(other) => return Err(map_expansion_error(other)),
                        }
                    }
                    remaining_iterations -= low;
                }
            }
        }

        if expanded.len() > remaining_occurrences {
            return Err(PlanningError::OccurrenceLimitExceeded);
        }
        remaining_occurrences -= expanded.len();

        for occ in expanded {
            let planned = build_planned_occurrence(item, occ, policy.lead_ms, &all_day_hms)?;
            occurrences.push(planned);
        }
    }

    check_elapsed(clock, started_monotonic, request.limits.max_elapsed_ms)?;
    Ok(PlannedBatch {
        window_start,
        planned_until,
        occurrences,
    })
}

fn build_planned_occurrence(
    item: &PlanningEventInput,
    occ: ExpandedOccurrence,
    lead_ms: i64,
    all_day_hms: &str,
) -> Result<PlannedOccurrence, PlanningError> {
    let start_utc = occ.start_utc.timestamp_millis();
    let end_utc = occ.end_utc.timestamp_millis();
    let local_date = occ.local_date.to_string();
    let due_at = if matches!(occ.start, Temporal::Date { .. }) {
        let reminder_temporal = Temporal::Zoned {
            local: format!("{local_date}T{all_day_hms}"),
            tzid: occ.resolved_tzid.clone(),
        };
        item.event
            .resolve_utc(&reminder_temporal)
            .map_err(|code| PlanningError::UnsupportedCapability(format!("{code:?}")))?
            .ok_or(PlanningError::InvalidWindow)?
            .timestamp_millis()
    } else {
        start_utc
            .checked_sub(lead_ms)
            .ok_or(PlanningError::InvalidWindow)?
    };
    Ok(PlannedOccurrence {
        event_id: item.event_id.clone(),
        event_revision: item.event_revision,
        original_key: occ.original_key,
        start_utc,
        end_utc,
        local_date,
        resolved_tzid: occ.resolved_tzid,
        cancelled: occ.cancelled,
        due_at,
    })
}
