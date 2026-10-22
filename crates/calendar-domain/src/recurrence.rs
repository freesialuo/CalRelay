//! Bounded recurrence expansion for [`EventV1`] (Issue #20).
//!
//! Evaluates the verified recurrence capability tier in local calendar coordinates before
//! resolving each occurrence through [`EventV1::resolve_utc`] or an explicit calendar time zone.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

use chrono::{
    DateTime, Datelike, Days, Duration, NaiveDate, NaiveDateTime, NaiveTime, Utc, Weekday,
};
use chrono_tz::Tz;

use crate::model::{parse_date, parse_local, parse_utc};
use crate::{
    Change, Code, EventPatch, EventV1, Period, RDate, RecurrenceException, Status, Temporal,
    ValidationError, SINGLE_KEY,
};

/// Bounded inputs for expanding an event within a half-open UTC window `[window_start, window_end)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpansionOptions<'a> {
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    /// Required when expanding `date` or `floating` events; ignored for `utc` and `zoned` events.
    pub calendar_tzid: Option<&'a str>,
    /// Maximum number of occurrences allowed within `[window_start, window_end)`.
    pub max_occurrences: usize,
    /// Maximum number of candidate steps evaluated before aborting with a budget error.
    pub max_iterations: usize,
}

/// A single expanded occurrence with preserved recurrence identity and resolved UTC span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpandedOccurrence {
    pub original_key: String,
    pub start: Temporal,
    pub start_utc: DateTime<Utc>,
    pub end_utc: DateTime<Utc>,
    pub local_date: NaiveDate,
    pub resolved_tzid: String,
    pub cancelled: bool,
    pub title: String,
}

/// Explicit failures returned when an event cannot be expanded within the verified tier and budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpansionError {
    Validation(Vec<ValidationError>),
    UnsupportedTimezone,
    MissingCalendarTimezone,
    InvalidWindow,
    InvalidDuration,
    UnsupportedRrule(String),
    OccurrenceLimitExceeded { max_occurrences: usize },
    IterationLimitExceeded { max_iterations: usize },
}

impl fmt::Display for ExpansionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(errs) => {
                write!(f, "event validation failed ({} error(s))", errs.len())
            }
            Self::UnsupportedTimezone => write!(f, "source VTIMEZONE is not interpreted"),
            Self::MissingCalendarTimezone => {
                write!(
                    f,
                    "date and floating events require a valid IANA calendar_tzid"
                )
            }
            Self::InvalidDuration => {
                write!(f, "occurrence duration is outside the supported range")
            }
            Self::InvalidWindow => write!(f, "expansion window, date range or budget is invalid"),
            Self::UnsupportedRrule(reason) => write!(f, "unsupported RRULE: {reason}"),
            Self::OccurrenceLimitExceeded { max_occurrences } => {
                write!(f, "occurrence limit ({max_occurrences}) exceeded")
            }
            Self::IterationLimitExceeded { max_iterations } => {
                write!(f, "iteration budget ({max_iterations}) exceeded")
            }
        }
    }
}

impl std::error::Error for ExpansionError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CalendarDuration {
    days: u64,
    seconds: i64,
}

fn parse_calendar_duration(s: &str) -> Option<CalendarDuration> {
    let rest = s.strip_prefix('+').unwrap_or(s).strip_prefix('P')?;
    let (date_part, time_part) = match rest.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (rest, None),
    };
    if time_part.is_none() && date_part.ends_with('W') {
        let weeks: u64 = date_part.strip_suffix('W')?.parse().ok()?;
        return Some(CalendarDuration {
            days: weeks.checked_mul(7)?,
            seconds: 0,
        });
    }
    let days: u64 = if date_part.is_empty() {
        0
    } else {
        date_part.strip_suffix('D')?.parse().ok()?
    };
    let mut seconds: i64 = 0;
    if let Some(mut t) = time_part {
        while !t.is_empty() {
            let digits = t.find(|c: char| !c.is_ascii_digit())?;
            let val: i64 = t[..digits].parse().ok()?;
            let unit = t[digits..].chars().next()?;
            let factor = match unit {
                'H' => 3600,
                'M' => 60,
                'S' => 1,
                _ => return None,
            };
            seconds = seconds.checked_add(val.checked_mul(factor)?)?;
            t = &t[digits + unit.len_utf8()..];
        }
    }
    Some(CalendarDuration { days, seconds })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UntilBound {
    Date(NaiveDate),
    Floating(NaiveDateTime),
    Instant(DateTime<Utc>),
}

#[derive(Debug, Clone)]
struct ParsedRrule {
    freq: Freq,
    interval: u32,
    count: Option<usize>,
    until: Option<UntilBound>,
    by_day: Option<Vec<Weekday>>,
    by_month_day: Option<Vec<u32>>,
    by_month: Option<Vec<u32>>,
}

fn parse_weekday(s: &str) -> Option<Weekday> {
    match s {
        "MO" => Some(Weekday::Mon),
        "TU" => Some(Weekday::Tue),
        "WE" => Some(Weekday::Wed),
        "TH" => Some(Weekday::Thu),
        "FR" => Some(Weekday::Fri),
        "SA" => Some(Weekday::Sat),
        "SU" => Some(Weekday::Sun),
        _ => None,
    }
}

fn parse_basic_date(s: &str) -> Option<NaiveDate> {
    if s.len() != 8 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    NaiveDate::parse_from_str(s, "%Y%m%d").ok()
}

fn parse_basic_local(s: &str) -> Option<NaiveDateTime> {
    let (d, t) = s.split_once('T')?;
    if t.len() != 6 || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let date = parse_basic_date(d)?;
    let time = NaiveTime::parse_from_str(t, "%H%M%S").ok()?;
    Some(date.and_time(time))
}

fn parse_until(raw: &str, start: &Temporal) -> Result<UntilBound, ExpansionError> {
    match start {
        Temporal::Date { .. } => parse_basic_date(raw)
            .map(UntilBound::Date)
            .ok_or_else(|| ExpansionError::UnsupportedRrule(format!("invalid DATE UNTIL: {raw}"))),
        Temporal::Floating { .. } => {
            parse_basic_local(raw)
                .map(UntilBound::Floating)
                .ok_or_else(|| {
                    ExpansionError::UnsupportedRrule(format!("invalid floating UNTIL: {raw}"))
                })
        }
        Temporal::Utc { .. } | Temporal::Zoned { .. } => {
            let body = raw.strip_suffix('Z').ok_or_else(|| {
                ExpansionError::UnsupportedRrule(format!("UTC/zoned UNTIL must end with Z: {raw}"))
            })?;
            parse_basic_local(body)
                .map(|dt| UntilBound::Instant(dt.and_utc()))
                .ok_or_else(|| {
                    ExpansionError::UnsupportedRrule(format!("invalid UTC UNTIL: {raw}"))
                })
        }
    }
}

fn parse_verified_rrule(rule: &str, start: &Temporal) -> Result<ParsedRrule, ExpansionError> {
    let mut freq = None;
    let mut interval = 1u32;
    let mut count = None;
    let mut until = None;
    let mut by_day = None;
    let mut by_month_day = None;
    let mut by_month = None;
    let mut seen_keys = HashSet::new();

    for part in rule.split(';') {
        let Some((key, value)) = part.split_once('=') else {
            return Err(ExpansionError::UnsupportedRrule(part.to_owned()));
        };
        if !seen_keys.insert(key) {
            return Err(ExpansionError::UnsupportedRrule(format!(
                "duplicate key {key}"
            )));
        }
        match key {
            "FREQ" => {
                freq = Some(match value {
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    "MONTHLY" => Freq::Monthly,
                    "YEARLY" => Freq::Yearly,
                    _ => {
                        return Err(ExpansionError::UnsupportedRrule(format!(
                            "unsupported FREQ={value}"
                        )))
                    }
                });
            }
            "INTERVAL" => {
                let v: u32 = value.parse().map_err(|_| {
                    ExpansionError::UnsupportedRrule(format!("invalid INTERVAL={value}"))
                })?;
                if v == 0 {
                    return Err(ExpansionError::UnsupportedRrule(
                        "INTERVAL must be >= 1".to_owned(),
                    ));
                }
                interval = v;
            }
            "COUNT" => {
                let v: usize = value.parse().map_err(|_| {
                    ExpansionError::UnsupportedRrule(format!("invalid COUNT={value}"))
                })?;
                if v == 0 {
                    return Err(ExpansionError::UnsupportedRrule(
                        "COUNT must be >= 1".to_owned(),
                    ));
                }
                count = Some(v);
            }
            "UNTIL" => {
                until = Some(parse_until(value, start)?);
            }
            "BYDAY" => {
                let mut days = Vec::new();
                for item in value.split(',') {
                    let wd = parse_weekday(item).ok_or_else(|| {
                        ExpansionError::UnsupportedRrule(format!("unsupported BYDAY={item}"))
                    })?;
                    if !days.contains(&wd) {
                        days.push(wd);
                    }
                }
                days.sort_by_key(|d| d.num_days_from_monday());
                by_day = Some(days);
            }
            "BYMONTHDAY" => {
                let mut doms = Vec::new();
                for item in value.split(',') {
                    let dom: i32 = item.parse().map_err(|_| {
                        ExpansionError::UnsupportedRrule(format!("invalid BYMONTHDAY={item}"))
                    })?;
                    if !(1..=31).contains(&dom) {
                        return Err(ExpansionError::UnsupportedRrule(format!(
                            "unsupported BYMONTHDAY={dom}"
                        )));
                    }
                    let dom = dom as u32;
                    if !doms.contains(&dom) {
                        doms.push(dom);
                    }
                }
                doms.sort_unstable();
                by_month_day = Some(doms);
            }
            "BYMONTH" => {
                let mut months = Vec::new();
                for item in value.split(',') {
                    let m: u32 = item.parse().map_err(|_| {
                        ExpansionError::UnsupportedRrule(format!("invalid BYMONTH={item}"))
                    })?;
                    if !(1..=12).contains(&m) {
                        return Err(ExpansionError::UnsupportedRrule(format!(
                            "invalid BYMONTH={m}"
                        )));
                    }
                    if !months.contains(&m) {
                        months.push(m);
                    }
                }
                months.sort_unstable();
                by_month = Some(months);
            }
            _ => {
                return Err(ExpansionError::UnsupportedRrule(format!(
                    "unsupported clause {key}={value}"
                )))
            }
        }
    }

    let freq = freq.ok_or_else(|| ExpansionError::UnsupportedRrule("missing FREQ".to_owned()))?;
    if by_day.is_some() && freq != Freq::Weekly {
        return Err(ExpansionError::UnsupportedRrule(
            "BYDAY is only supported with FREQ=WEEKLY in v1".to_owned(),
        ));
    }
    if by_month_day.is_some() && !matches!(freq, Freq::Monthly | Freq::Yearly) {
        return Err(ExpansionError::UnsupportedRrule(
            "BYMONTHDAY is only supported with FREQ=MONTHLY or FREQ=YEARLY".to_owned(),
        ));
    }
    if by_month.is_some() && freq != Freq::Yearly {
        return Err(ExpansionError::UnsupportedRrule(
            "BYMONTH is only supported with FREQ=YEARLY in v1".to_owned(),
        ));
    }

    let (start_date, _) = extract_start_date_time(start);
    if let Some(days) = &by_day {
        if !days.contains(&start_date.weekday()) {
            return Err(ExpansionError::UnsupportedRrule(
                "DTSTART weekday must match BYDAY".to_owned(),
            ));
        }
    }
    if let Some(doms) = &by_month_day {
        if !doms.contains(&start_date.day()) {
            return Err(ExpansionError::UnsupportedRrule(
                "DTSTART day-of-month must match BYMONTHDAY".to_owned(),
            ));
        }
    }
    if let Some(months) = &by_month {
        if !months.contains(&start_date.month()) {
            return Err(ExpansionError::UnsupportedRrule(
                "DTSTART month must match BYMONTH".to_owned(),
            ));
        }
    }

    Ok(ParsedRrule {
        freq,
        interval,
        count,
        until,
        by_day,
        by_month_day,
        by_month,
    })
}

struct Resolver<'a> {
    event: &'a EventV1,
    calendar_tz: Option<(&'a str, Tz)>,
}

impl<'a> Resolver<'a> {
    fn new(event: &'a EventV1, options: &ExpansionOptions<'a>) -> Result<Self, ExpansionError> {
        let timezones = event.timezones.as_deref().unwrap_or(&[]);
        let calendar_tz = match options.calendar_tzid {
            Some(tzid) => {
                if timezones
                    .iter()
                    .any(|z| z.tzid == tzid && !z.vtimezone.trim().is_empty())
                {
                    return Err(ExpansionError::UnsupportedTimezone);
                }
                if timezones.iter().any(|z| z.tzid == tzid) {
                    return Err(ExpansionError::MissingCalendarTimezone);
                }
                let tz = tzid
                    .parse::<Tz>()
                    .map_err(|_| ExpansionError::MissingCalendarTimezone)?;
                Some((tzid, tz))
            }
            None => None,
        };
        Ok(Self { event, calendar_tz })
    }

    fn resolve_temporal(
        &self,
        temporal: &Temporal,
    ) -> Result<(DateTime<Utc>, NaiveDate, String), ExpansionError> {
        match temporal {
            Temporal::Utc { value } => {
                let dt = parse_utc(value).expect("validated UTC temporal");
                Ok((dt, dt.date_naive(), "UTC".to_owned()))
            }
            Temporal::Zoned { local, tzid } => {
                let dt = self
                    .event
                    .resolve_utc(temporal)
                    .map_err(|code| match code {
                        Code::UnsupportedTimezone => ExpansionError::UnsupportedTimezone,
                        other => ExpansionError::Validation(vec![ValidationError {
                            code: other,
                            path: "/start".to_owned(),
                        }]),
                    })?
                    .expect("validated zoned temporal resolves to UTC");
                let local_dt = parse_local(local).expect("validated zoned local");
                Ok((dt, local_dt.date(), tzid.clone()))
            }
            Temporal::Floating { local } => {
                let (cal_tzid, _) = self
                    .calendar_tz
                    .ok_or(ExpansionError::MissingCalendarTimezone)?;
                let local_dt = parse_local(local).expect("validated floating local");
                let zoned = Temporal::Zoned {
                    local: local.clone(),
                    tzid: cal_tzid.to_owned(),
                };
                let dt = zoned
                    .to_utc()
                    .expect("validated calendar tz")
                    .expect("zoned resolves to Some");
                Ok((dt, local_dt.date(), cal_tzid.to_owned()))
            }
            Temporal::Date { date } => {
                let (cal_tzid, _) = self
                    .calendar_tz
                    .ok_or(ExpansionError::MissingCalendarTimezone)?;
                let d = parse_date(date).expect("validated date");
                let zoned = Temporal::Zoned {
                    local: format!("{date}T00:00:00"),
                    tzid: cal_tzid.to_owned(),
                };
                let dt = zoned
                    .to_utc()
                    .expect("validated calendar tz")
                    .expect("zoned resolves to Some");
                Ok((dt, d, cal_tzid.to_owned()))
            }
        }
    }

    fn add_duration(
        &self,
        start: &Temporal,
        start_utc: DateTime<Utc>,
        dur: CalendarDuration,
    ) -> Result<DateTime<Utc>, ExpansionError> {
        let day_shifted_utc = if dur.days == 0 {
            start_utc
        } else {
            let (date, _) = extract_start_date_time(start);
            let shifted_date = date
                .checked_add_days(Days::new(dur.days))
                // Temporal values use four-digit years, even though Chrono supports more.
                .filter(|date| (0..=9999).contains(&date.year()))
                .ok_or(ExpansionError::InvalidDuration)?;
            let shifted_temporal = temporal_on_date(start, shifted_date);
            self.resolve_temporal(&shifted_temporal)?.0
        };
        let seconds = Duration::try_seconds(dur.seconds).ok_or(ExpansionError::InvalidDuration)?;
        day_shifted_utc
            .checked_add_signed(seconds)
            .ok_or(ExpansionError::InvalidDuration)
    }
}

fn temporal_on_date(template: &Temporal, date: NaiveDate) -> Temporal {
    match template {
        Temporal::Date { .. } => Temporal::Date {
            date: date.format("%Y-%m-%d").to_string(),
        },
        Temporal::Utc { value } => {
            let time = parse_utc(value).unwrap().time();
            Temporal::Utc {
                value: date.and_time(time).format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            }
        }
        Temporal::Zoned { local, tzid } => {
            let time = parse_local(local).unwrap().time();
            Temporal::Zoned {
                local: date.and_time(time).format("%Y-%m-%dT%H:%M:%S").to_string(),
                tzid: tzid.clone(),
            }
        }
        Temporal::Floating { local } => {
            let time = parse_local(local).unwrap().time();
            Temporal::Floating {
                local: date.and_time(time).format("%Y-%m-%dT%H:%M:%S").to_string(),
            }
        }
    }
}

fn extract_start_date_time(start: &Temporal) -> (NaiveDate, Option<NaiveTime>) {
    match start {
        Temporal::Date { date } => (parse_date(date).unwrap(), None),
        Temporal::Utc { value } => {
            let dt = parse_utc(value).unwrap().naive_utc();
            (dt.date(), Some(dt.time()))
        }
        Temporal::Zoned { local, .. } | Temporal::Floating { local } => {
            let dt = parse_local(local).unwrap();
            (dt.date(), Some(dt.time()))
        }
    }
}

fn overlaps_window(
    start_utc: DateTime<Utc>,
    end_utc: DateTime<Utc>,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
) -> bool {
    if end_utc > start_utc {
        start_utc < window_end && end_utc > window_start
    } else {
        start_utc >= window_start && start_utc < window_end
    }
}

/// Expand `event` into occurrences overlapping `[options.window_start, options.window_end)`.
pub fn expand_occurrences(
    event: &EventV1,
    options: &ExpansionOptions<'_>,
) -> Result<Vec<ExpandedOccurrence>, ExpansionError> {
    crate::validate_event(event).map_err(ExpansionError::Validation)?;
    if options.window_end <= options.window_start
        || options.max_occurrences == 0
        || options.max_iterations == 0
    {
        return Err(ExpansionError::InvalidWindow);
    }

    let resolver = Resolver::new(event, options)?;
    let (master_start_utc, _, _) = resolver.resolve_temporal(&event.start)?;
    let Some(recurrence) = &event.recurrence else {
        let end_utc = resolve_span_end(
            &resolver,
            &event.start,
            master_start_utc,
            event.end.as_ref(),
            event.duration.as_deref(),
            false,
        )?;
        let (_, local_date, resolved_tzid) = resolver.resolve_temporal(&event.start)?;
        if !overlaps_window(
            start_utc_or(master_start_utc),
            end_utc,
            options.window_start,
            options.window_end,
        ) {
            return Ok(Vec::new());
        }
        return Ok(vec![ExpandedOccurrence {
            original_key: SINGLE_KEY.to_owned(),
            start: event.start.clone(),
            start_utc: master_start_utc,
            end_utc,
            local_date,
            resolved_tzid,
            cancelled: event.status == Status::Cancelled,
            title: event.title.clone(),
        }]);
    };

    let exdates: HashSet<String> = recurrence
        .exdates
        .iter()
        .flatten()
        .map(Temporal::recurrence_key)
        .collect();

    let mut exceptions_by_key: HashMap<String, &RecurrenceException> = HashMap::new();
    let mut stop_horizon = options.window_end + Duration::days(2);
    for exc in recurrence.exceptions.iter().flatten() {
        let (rid_utc, _, _) = resolver.resolve_temporal(&exc.recurrence_id)?;
        if rid_utc + Duration::days(2) > stop_horizon {
            stop_horizon = rid_utc + Duration::days(2);
        }
        if let Change::Override { patch } = &exc.change {
            if let Some(s) = &patch.start {
                resolver.resolve_temporal(s)?;
            }
            if let Some(Some(e)) = &patch.end {
                resolver.resolve_temporal(e)?;
            }
        }
        exceptions_by_key.insert(exc.recurrence_id.recurrence_key(), exc);
    }

    // Map from original_key -> (base Temporal, optional Period override)
    let mut candidates: CandidateMap<'_> = BTreeMap::new();

    if let Some(rrule_str) = &recurrence.rrule {
        let parsed = parse_verified_rrule(rrule_str, &event.start)?;
        generate_rrule_candidates(
            event,
            &resolver,
            &parsed,
            stop_horizon,
            options.max_iterations,
            &mut candidates,
        )?;
    } else {
        candidates.insert(
            (master_start_utc, event.start.recurrence_key()),
            (event.start.clone(), None),
        );
    }

    for rdate in recurrence.rdates.iter().flatten() {
        match rdate {
            RDate::Temporal(t) => {
                let (utc_val, _, _) = resolver.resolve_temporal(t)?;
                candidates
                    .entry((utc_val, t.recurrence_key()))
                    .or_insert_with(|| (t.clone(), None));
            }
            RDate::Period(period) => {
                let (utc_val, _, _) = resolver.resolve_temporal(&period.start)?;
                if let Some(end) = &period.end {
                    resolver.resolve_temporal(end)?;
                }
                candidates.insert(
                    (utc_val, period.start.recurrence_key()),
                    (period.start.clone(), Some(period)),
                );
            }
        }
    }

    let mut results = Vec::new();
    for ((base_start_utc, original_key), (base_start, period)) in candidates {
        if exdates.contains(&original_key) {
            continue;
        }
        let mut effective_start = base_start.clone();
        let mut effective_title = event.title.clone();
        let mut cancelled = event.status == Status::Cancelled;
        let mut end_override: Option<Option<&Temporal>> = None;
        let mut duration_override: Option<Option<&str>> = None;

        if let Some(p) = period {
            end_override = Some(p.end.as_ref());
            duration_override = Some(p.duration.as_deref());
        }

        if let Some(exc) = exceptions_by_key.get(&original_key) {
            match &exc.change {
                Change::Cancelled => {
                    cancelled = true;
                }
                Change::Override { patch } => {
                    apply_patch(
                        patch,
                        &mut effective_start,
                        &mut effective_title,
                        &mut cancelled,
                        &mut end_override,
                        &mut duration_override,
                    );
                }
            }
        }

        let (start_utc, local_date, resolved_tzid) = if effective_start == base_start {
            let (_, local_date, resolved_tzid) = resolver.resolve_temporal(&base_start)?;
            (base_start_utc, local_date, resolved_tzid)
        } else {
            resolver.resolve_temporal(&effective_start)?
        };

        let effective_end = end_override.unwrap_or(event.end.as_ref());
        let effective_duration = duration_override.unwrap_or(event.duration.as_deref());
        let end_utc = resolve_span_end(
            &resolver,
            &effective_start,
            start_utc,
            effective_end,
            effective_duration,
            end_override.is_none(),
        )?;

        if overlaps_window(start_utc, end_utc, options.window_start, options.window_end) {
            if results.len() >= options.max_occurrences {
                return Err(ExpansionError::OccurrenceLimitExceeded {
                    max_occurrences: options.max_occurrences,
                });
            }
            results.push(ExpandedOccurrence {
                original_key,
                start: effective_start,
                start_utc,
                end_utc,
                local_date,
                resolved_tzid,
                cancelled,
                title: effective_title,
            });
        }
    }

    results.sort_by(|a, b| {
        a.start_utc
            .cmp(&b.start_utc)
            .then_with(|| a.original_key.cmp(&b.original_key))
    });
    Ok(results)
}

type CandidateMap<'a> = BTreeMap<(DateTime<Utc>, String), (Temporal, Option<&'a Period>)>;

const fn start_utc_or(dt: DateTime<Utc>) -> DateTime<Utc> {
    dt
}

fn apply_patch<'a>(
    patch: &'a EventPatch,
    effective_start: &mut Temporal,
    effective_title: &mut String,
    cancelled: &mut bool,
    end_override: &mut Option<Option<&'a Temporal>>,
    duration_override: &mut Option<Option<&'a str>>,
) {
    if let Some(s) = &patch.start {
        *effective_start = s.clone();
    }
    if let Some(t) = &patch.title {
        *effective_title = t.clone();
    }
    if let Some(status) = patch.status {
        *cancelled = status == Status::Cancelled;
    }
    if let Some(end_opt) = &patch.end {
        *end_override = Some(end_opt.as_ref());
    }
    if let Some(dur_opt) = &patch.duration {
        *duration_override = Some(dur_opt.as_deref());
    }
}

fn resolve_span_end(
    resolver: &Resolver<'_>,
    start: &Temporal,
    start_utc: DateTime<Utc>,
    explicit_end: Option<&Temporal>,
    duration: Option<&str>,
    inherited_end: bool,
) -> Result<DateTime<Utc>, ExpansionError> {
    if let Some(dur_str) = duration {
        let dur = parse_calendar_duration(dur_str).ok_or(ExpansionError::InvalidDuration)?;
        return resolver.add_duration(start, start_utc, dur);
    }
    if let Some(end_temp) = explicit_end {
        let (end_utc, _, _) = resolver.resolve_temporal(end_temp)?;
        if !inherited_end {
            return Ok(end_utc);
        }
        // Inherited ends describe the master's span; occurrence overrides are absolute.
        if let (Temporal::Date { date: s }, Temporal::Date { date: e }) =
            (&resolver.event.start, end_temp)
        {
            let days = (parse_date(e).unwrap() - parse_date(s).unwrap()).num_days();
            if days > 0 {
                return resolver.add_duration(
                    start,
                    start_utc,
                    CalendarDuration {
                        days: days as u64,
                        seconds: 0,
                    },
                );
            }
        }
        let master_start_utc = resolver.resolve_temporal(&resolver.event.start)?.0;
        return start_utc
            .checked_add_signed(end_utc - master_start_utc)
            .ok_or(ExpansionError::InvalidDuration);
    }
    if matches!(start, Temporal::Date { .. }) {
        resolver.add_duration(
            start,
            start_utc,
            CalendarDuration {
                days: 1,
                seconds: 0,
            },
        )
    } else {
        Ok(start_utc)
    }
}

fn generate_rrule_candidates<'a>(
    event: &'a EventV1,
    resolver: &Resolver<'a>,
    rrule: &ParsedRrule,
    stop_horizon: DateTime<Utc>,
    max_iterations: usize,
    candidates: &mut CandidateMap<'a>,
) -> Result<(), ExpansionError> {
    let (start_date, start_time) = extract_start_date_time(&event.start);
    let mut iterations = 0usize;
    let mut generated = 0usize;
    let mut period_idx = 0u64;

    loop {
        let mut period_dates: Vec<Option<NaiveDate>> = Vec::new();
        match rrule.freq {
            Freq::Daily => {
                let offset_days = period_idx
                    .checked_mul(u64::from(rrule.interval))
                    .ok_or(ExpansionError::InvalidWindow)?;
                period_dates.push(start_date.checked_add_days(Days::new(offset_days)));
            }
            Freq::Weekly => {
                let week_monday = start_date
                    .checked_sub_days(Days::new(u64::from(
                        start_date.weekday().num_days_from_monday(),
                    )))
                    .ok_or(ExpansionError::InvalidWindow)?;
                let week_offset = period_idx
                    .checked_mul(u64::from(rrule.interval))
                    .and_then(|w| w.checked_mul(7))
                    .ok_or(ExpansionError::InvalidWindow)?;
                let base_monday = week_monday
                    .checked_add_days(Days::new(week_offset))
                    .ok_or(ExpansionError::InvalidWindow)?;
                let days = rrule
                    .by_day
                    .clone()
                    .unwrap_or_else(|| vec![start_date.weekday()]);
                for wd in days {
                    let d = base_monday
                        .checked_add_days(Days::new(u64::from(wd.num_days_from_monday())));
                    if d.is_some_and(|d| d >= start_date) {
                        period_dates.push(d);
                    }
                }
            }
            Freq::Monthly => {
                let total_months = i64::from(start_date.year()) * 12
                    + i64::from(start_date.month() - 1)
                    + (period_idx as i64) * i64::from(rrule.interval);
                let year = i32::try_from(total_months.div_euclid(12))
                    .map_err(|_| ExpansionError::InvalidWindow)?;
                let month = (total_months.rem_euclid(12) + 1) as u32;
                let doms = rrule
                    .by_month_day
                    .clone()
                    .unwrap_or_else(|| vec![start_date.day()]);
                for dom in doms {
                    let candidate = NaiveDate::from_ymd_opt(year, month, dom);
                    if candidate.is_none_or(|d| d >= start_date) {
                        period_dates.push(candidate);
                    }
                }
            }
            Freq::Yearly => {
                let year = start_date
                    .year()
                    .checked_add(
                        i32::try_from(period_idx * u64::from(rrule.interval))
                            .map_err(|_| ExpansionError::InvalidWindow)?,
                    )
                    .ok_or(ExpansionError::InvalidWindow)?;
                let months = rrule.by_month.clone().unwrap_or_else(|| {
                    if rrule.by_month_day.is_some() {
                        (1..=12).collect()
                    } else {
                        vec![start_date.month()]
                    }
                });
                let doms = rrule
                    .by_month_day
                    .clone()
                    .unwrap_or_else(|| vec![start_date.day()]);
                for m in &months {
                    for dom in &doms {
                        let candidate = NaiveDate::from_ymd_opt(year, *m, *dom);
                        if candidate.is_none_or(|d| d >= start_date) {
                            period_dates.push(candidate);
                        }
                    }
                }
            }
        }

        for maybe_date in period_dates {
            iterations += 1;
            if iterations > max_iterations {
                return Err(ExpansionError::IterationLimitExceeded { max_iterations });
            }
            // RFC 5545 Section 3.3.10: nonexistent calendar dates (e.g., Feb 31 or non-leap Feb 29)
            // are ignored and not counted toward COUNT.
            let Some(date) = maybe_date else {
                continue;
            };
            // Chrono permits expanded years, but Temporal requires four-digit years.
            if !(0..=9999).contains(&date.year()) {
                return Err(ExpansionError::InvalidWindow);
            }
            let candidate_temporal = temporal_on_date(&event.start, date);
            let (candidate_utc, _, _) = resolver.resolve_temporal(&candidate_temporal)?;

            if let Some(until) = rrule.until {
                let past_until = match until {
                    UntilBound::Date(u) => date > u,
                    UntilBound::Floating(u) => {
                        date.and_time(start_time.expect("floating has time")) > u
                    }
                    UntilBound::Instant(u) => candidate_utc > u,
                };
                if past_until {
                    return Ok(());
                }
            }

            candidates.insert(
                (candidate_utc, candidate_temporal.recurrence_key()),
                (candidate_temporal, None),
            );
            generated += 1;
            if rrule.count.is_some_and(|max_c| generated >= max_c) {
                return Ok(());
            }
            if candidate_utc > stop_horizon {
                return Ok(());
            }
        }

        period_idx += 1;
    }
}
