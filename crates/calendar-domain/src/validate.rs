//! Semantic validation of [`EventV1`]: the rules JSON Schema cannot express.

use std::collections::HashSet;
use std::fmt;

use chrono_tz::Tz;

use crate::model::{
    parse_date, parse_local, parse_utc, Alarm, AlarmTrigger, Change, EventPatch, EventV1, RDate,
    Recurrence, RecurrenceRange, Temporal, Timezone,
};

/// Stable machine-readable failure classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Code {
    EndAndDurationExclusive,
    InvalidTemporal,
    EndTypeMismatch,
    EndBeforeStart,
    InvalidDuration,
    NegativeDuration,
    DateDurationHasTime,
    UnknownTimezone,
    UnsupportedTimezone,
    RecurrenceTypeMismatch,
    DuplicateException,
    UnsupportedRange,
    UnsupportedRrule,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub code: Code,
    /// JSON-pointer-like location inside the event document.
    pub path: String,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} at {}", self.code, self.path)
    }
}

impl std::error::Error for ValidationError {}

/// Parsed ISO-8601 duration components; `has_time` is true when a `T` section is present.
struct Duration {
    negative: bool,
    has_time: bool,
}

fn parse_duration(s: &str) -> Option<Duration> {
    let (negative, rest) = match s.as_bytes().first()? {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    let rest = rest.strip_prefix('P')?;
    let (date_part, time_part) = match rest.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (rest, None),
    };
    // Consume `<digits><unit>` groups in strictly increasing unit order.
    fn groups(mut s: &str, units: &[char]) -> Option<usize> {
        let (mut count, mut next_unit) = (0, 0);
        while !s.is_empty() {
            let digits = s.find(|c: char| !c.is_ascii_digit())?;
            if digits == 0 {
                return None;
            }
            let unit = s[digits..].chars().next()?;
            let pos = units.iter().position(|&u| u == unit)?;
            if pos < next_unit {
                return None;
            }
            next_unit = pos + 1;
            count += 1;
            s = &s[digits + unit.len_utf8()..];
        }
        Some(count)
    }
    if let Some(weeks) = (time_part.is_none() && date_part.ends_with('W')).then_some(date_part) {
        return (groups(weeks, &['W'])? == 1).then_some(Duration {
            negative,
            has_time: false,
        });
    }
    let days = groups(date_part, &['D'])?;
    let times = match time_part {
        Some(t) => {
            let n = groups(t, &['H', 'M', 'S'])?;
            if n == 0 {
                return None;
            }
            n
        }
        None => 0,
    };
    (days + times > 0).then_some(Duration {
        negative,
        has_time: time_part.is_some(),
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Date,
    Floating,
    Instant,
}

fn family(t: &Temporal) -> Family {
    match t {
        Temporal::Date { .. } => Family::Date,
        Temporal::Floating { .. } => Family::Floating,
        Temporal::Utc { .. } | Temporal::Zoned { .. } => Family::Instant,
    }
}

struct Ctx<'a> {
    errors: Vec<ValidationError>,
    timezones: &'a [Timezone],
}

impl Ctx<'_> {
    fn has_source_timezone(&self, tzid: &str) -> bool {
        self.timezones
            .iter()
            .any(|z| z.tzid == tzid && !z.vtimezone.trim().is_empty())
    }

    fn has_any_source_timezone_entry(&self, tzid: &str) -> bool {
        self.timezones.iter().any(|z| z.tzid == tzid)
    }

    fn err(&mut self, code: Code, path: &str) {
        self.errors.push(ValidationError {
            code,
            path: path.to_owned(),
        });
    }

    /// Calendar validity and time zone resolvability of a single value.
    fn temporal(&mut self, t: &Temporal, path: &str) {
        let valid = match t {
            Temporal::Date { date } => parse_date(date).is_some(),
            Temporal::Utc { value } => parse_utc(value).is_some(),
            Temporal::Floating { local } => parse_local(local).is_some(),
            Temporal::Zoned { local, tzid } => {
                let known = if self.has_any_source_timezone_entry(tzid) {
                    self.has_source_timezone(tzid)
                } else {
                    tzid.parse::<Tz>().is_ok()
                };
                if !known {
                    self.err(Code::UnknownTimezone, path);
                }
                parse_local(local).is_some()
            }
        };
        if !valid {
            self.err(Code::InvalidTemporal, path);
        }
    }

    /// Returns the parsed duration when well formed. `allow_negative` is false for spans.
    fn duration(&mut self, s: &str, path: &str, allow_negative: bool) -> Option<Duration> {
        let Some(d) = parse_duration(s) else {
            self.err(Code::InvalidDuration, path);
            return None;
        };
        if d.negative && !allow_negative {
            self.err(Code::NegativeDuration, path);
        }
        Some(d)
    }

    /// Checks a start plus exactly-one end/duration pair.
    fn span(
        &mut self,
        start: &Temporal,
        end: Option<&Temporal>,
        duration: Option<&str>,
        path: &str,
    ) {
        self.span_shape(start, end, duration, path);
        if let Some(end) = end {
            self.temporal(end, &format!("{path}/end"));
            self.end_vs_start(start, end, &format!("{path}/end"));
        }
    }

    /// Checks span fields without ordering an inherited absolute end against a new start.
    fn span_shape(
        &mut self,
        start: &Temporal,
        end: Option<&Temporal>,
        duration: Option<&str>,
        path: &str,
    ) {
        if end.is_some() && duration.is_some() {
            self.err(Code::EndAndDurationExclusive, path);
        }
        if end.is_some_and(|end| family(start) != family(end)) {
            self.err(Code::EndTypeMismatch, &format!("{path}/end"));
        }
        if let Some(d) = duration {
            let p = format!("{path}/duration");
            if let Some(d) = self.duration(d, &p, false) {
                if family(start) == Family::Date && d.has_time {
                    self.err(Code::DateDurationHasTime, &p);
                }
            }
        }
    }

    fn end_vs_start(&mut self, start: &Temporal, end: &Temporal, path: &str) {
        if family(start) != family(end) {
            return;
        }
        let ordered = match (start, end) {
            (Temporal::Date { date: s }, Temporal::Date { date: e }) => {
                parse_date(s).zip(parse_date(e)).map(|(s, e)| e > s)
            }
            (Temporal::Floating { local: s }, Temporal::Floating { local: e }) => {
                parse_local(s).zip(parse_local(e)).map(|(s, e)| e >= s)
            }
            _ => match (
                start.to_utc_with_timezones(self.timezones),
                end.to_utc_with_timezones(self.timezones),
            ) {
                (Ok(Some(s)), Ok(Some(e))) => Some(e >= s),
                // Malformed values are reported elsewhere; source-defined zones are not comparable.
                _ => None,
            },
        };
        if ordered == Some(false) {
            self.err(Code::EndBeforeStart, path);
        }
    }

    /// Recurrence values must have the same kind (and zone) as the parent start.
    fn same_kind(&mut self, start: &Temporal, value: &Temporal, path: &str) {
        let ok = match (start, value) {
            (Temporal::Date { .. }, Temporal::Date { .. })
            | (Temporal::Utc { .. }, Temporal::Utc { .. })
            | (Temporal::Floating { .. }, Temporal::Floating { .. }) => true,
            (Temporal::Zoned { tzid: a, .. }, Temporal::Zoned { tzid: b, .. }) => a == b,
            _ => false,
        };
        if !ok {
            self.err(Code::RecurrenceTypeMismatch, path);
        }
    }

    fn rrule(&mut self, rule: &str, path: &str) {
        const FREQS: [&str; 7] = [
            "SECONDLY", "MINUTELY", "HOURLY", "DAILY", "WEEKLY", "MONTHLY", "YEARLY",
        ];
        let (mut freq_ok, mut count, mut until) = (false, false, false);
        for part in rule.split(';') {
            match part.split_once('=') {
                Some(("FREQ", v)) => freq_ok = FREQS.contains(&v),
                Some(("COUNT", _)) => count = true,
                Some(("UNTIL", _)) => until = true,
                Some((k, v)) if !k.is_empty() && !v.is_empty() => {}
                _ => {
                    self.err(Code::UnsupportedRrule, path);
                    return;
                }
            }
        }
        if !freq_ok || (count && until) {
            self.err(Code::UnsupportedRrule, path);
        }
    }

    fn alarm(&mut self, alarm: &Alarm, path: &str) {
        match &alarm.trigger {
            AlarmTrigger::Relative { offset, .. } => {
                self.duration(offset, &format!("{path}/trigger/offset"), true);
            }
            AlarmTrigger::Absolute { at } => {
                let p = format!("{path}/trigger/at");
                self.temporal(at, &p);
                if !matches!(at, Temporal::Utc { .. }) {
                    self.err(Code::InvalidTemporal, &p);
                }
            }
        }
        if let Some(interval) = &alarm.repeat_interval {
            let p = format!("{path}/repeat_interval");
            if let Some(d) = self.duration(interval, &p, false) {
                if d.negative {
                    self.err(Code::NegativeDuration, &p);
                }
            }
        }
    }

    fn patch(&mut self, event: &EventV1, occurrence: &Temporal, patch: &EventPatch, path: &str) {
        let start = patch.start.as_ref().unwrap_or(occurrence);
        if let Some(s) = &patch.start {
            self.temporal(s, &format!("{path}/start"));
        }
        // Omitted fields inherit; explicit null clears the inherited value.
        let end = patch.end.as_ref().unwrap_or(&event.end).as_ref();
        let duration = patch
            .duration
            .as_ref()
            .unwrap_or(&event.duration)
            .as_deref();
        self.span(start, end, duration, path);
        for (i, a) in patch.alarms.iter().flatten().enumerate() {
            self.alarm(a, &format!("{path}/alarms/{i}"));
        }
    }

    fn recurrence(&mut self, event: &EventV1, r: &Recurrence, path: &str) {
        let start = &event.start;
        if let Some(rule) = &r.rrule {
            self.rrule(rule, &format!("{path}/rrule"));
        }
        for (i, d) in r.rdates.iter().flatten().enumerate() {
            let p = format!("{path}/rdates/{i}");
            match d {
                RDate::Temporal(t) => {
                    self.temporal(t, &p);
                    self.same_kind(start, t, &p);
                }
                RDate::Period(period) => {
                    self.temporal(&period.start, &format!("{p}/start"));
                    self.same_kind(start, &period.start, &format!("{p}/start"));
                    if period.end.is_none() && period.duration.is_none() {
                        self.err(Code::EndAndDurationExclusive, &p);
                    }
                    self.span(
                        &period.start,
                        period.end.as_ref(),
                        period.duration.as_deref(),
                        &p,
                    );
                }
            }
        }
        for (i, t) in r.exdates.iter().flatten().enumerate() {
            let p = format!("{path}/exdates/{i}");
            self.temporal(t, &p);
            self.same_kind(start, t, &p);
        }
        let mut seen = HashSet::new();
        for (i, e) in r.exceptions.iter().flatten().enumerate() {
            let p = format!("{path}/exceptions/{i}");
            self.temporal(&e.recurrence_id, &format!("{p}/recurrence_id"));
            self.same_kind(start, &e.recurrence_id, &format!("{p}/recurrence_id"));
            if !seen.insert(e.recurrence_id.recurrence_key()) {
                self.err(Code::DuplicateException, &format!("{p}/recurrence_id"));
            }
            if e.range == RecurrenceRange::ThisAndFuture {
                self.err(Code::UnsupportedRange, &format!("{p}/range"));
            }
            if let Change::Override { patch } = &e.change {
                self.patch(event, &e.recurrence_id, patch, &format!("{p}/change/patch"));
            }
        }
    }
}

/// Validate merged occurrence span fields before resolving an inherited or explicit end.
pub(crate) fn validate_span_shape(
    start: &Temporal,
    end: Option<&Temporal>,
    duration: Option<&str>,
) -> Result<(), Vec<ValidationError>> {
    let mut cx = Ctx {
        errors: Vec::new(),
        timezones: &[],
    };
    cx.span_shape(start, end, duration, "/recurrence");
    if cx.errors.is_empty() {
        Ok(())
    } else {
        Err(cx.errors)
    }
}

/// Validate the cross-field, type and time semantics of an event. All problems are reported.
pub fn validate_event(event: &EventV1) -> Result<(), Vec<ValidationError>> {
    let mut cx = Ctx {
        errors: Vec::new(),
        timezones: event.timezones.as_deref().unwrap_or(&[]),
    };
    cx.temporal(&event.start, "/start");
    cx.span(
        &event.start,
        event.end.as_ref(),
        event.duration.as_deref(),
        "",
    );
    if let Some(r) = &event.recurrence {
        cx.recurrence(event, r, "/recurrence");
    }
    for (i, a) in event.alarms.iter().flatten().enumerate() {
        cx.alarm(a, &format!("/alarms/{i}"));
    }
    if cx.errors.is_empty() {
        Ok(())
    } else {
        Err(cx.errors)
    }
}
