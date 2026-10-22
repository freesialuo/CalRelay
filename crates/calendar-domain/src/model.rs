//! Typed form of the `EventV1` contract (`schemas/event-v1.schema.json`).
//!
//! Deserialization enforces shape only; cross-field and calendar semantics live in
//! [`crate::validate`]. Optional arrays are `Option<Vec<_>>` so that an absent field and an
//! empty array stay distinguishable and documents round-trip unchanged.

use chrono::{DateTime, LocalResult, NaiveDate, NaiveDateTime, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Deserializer, Serialize};

use crate::Code;

/// Contract version. Unknown versions are rejected at the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchemaVersion {
    #[serde(rename = "1.0")]
    V1,
}

/// A point in time whose calendar semantics are preserved (never flattened to UTC).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Temporal {
    /// All-day value, `YYYY-MM-DD`.
    Date { date: String },
    /// Absolute instant, `YYYY-MM-DDTHH:MM:SSZ`.
    Utc { value: String },
    /// Local wall-clock time bound to a time zone identifier.
    Zoned { local: String, tzid: String },
    /// Local wall-clock time with no zone; needs an interpretation zone before planning.
    Floating { local: String },
}

pub(crate) const LOCAL_FORMAT: &str = "%Y-%m-%dT%H:%M:%S";

pub(crate) fn parse_date(s: &str) -> Option<NaiveDate> {
    if s.len() != 10 {
        return None;
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

pub(crate) fn parse_local(s: &str) -> Option<NaiveDateTime> {
    if s.len() != 19 {
        return None;
    }
    NaiveDateTime::parse_from_str(s, LOCAL_FORMAT).ok()
}

pub(crate) fn parse_utc(s: &str) -> Option<DateTime<Utc>> {
    let body = s.strip_suffix('Z')?;
    parse_local(body).map(|n| n.and_utc())
}

/// Resolve a local time in an IANA zone using RFC 5545 rules: an ambiguous time takes the first
/// occurrence; a nonexistent time uses the offset in force before the transition.
fn resolve_zoned(local: NaiveDateTime, tz: Tz) -> DateTime<Utc> {
    match tz.from_local_datetime(&local) {
        LocalResult::Single(t) => t.with_timezone(&Utc),
        LocalResult::Ambiguous(first, _) => first.with_timezone(&Utc),
        LocalResult::None => {
            let offset = chrono_tz::GapInfo::new(&local, &tz)
                .and_then(|gap| gap.begin.map(|(_, before_offset)| before_offset.fix()))
                .unwrap_or_else(|| tz.offset_from_utc_datetime(&local).fix());
            (local - chrono::Duration::seconds(i64::from(offset.local_minus_utc()))).and_utc()
        }
    }
}

impl Temporal {
    /// Absolute instant for `utc` and IANA-zoned values when no source `VTIMEZONE` definitions apply.
    ///
    /// `Ok(None)` means no instant is available without calendar-level interpretation (`date` or
    /// `floating`). `Err` means the value is malformed (`Code::InvalidTemporal`) or names an
    /// unknown zone (`Code::UnknownTimezone`).
    pub fn to_utc(&self) -> Result<Option<DateTime<Utc>>, Code> {
        self.to_utc_with_timezones(&[])
    }

    /// Absolute instant for `utc` and IANA-zoned values, considering source `VTIMEZONE` definitions.
    ///
    /// Source `timezones` take precedence over same-name IANA identifiers. Because custom
    /// `VTIMEZONE` rules are not interpreted in v1, any zoned value bound to a non-empty source
    /// definition is rejected with [`Code::UnsupportedTimezone`] rather than falling back to IANA
    /// or the host time zone.
    pub fn to_utc_with_timezones(
        &self,
        timezones: &[Timezone],
    ) -> Result<Option<DateTime<Utc>>, Code> {
        match self {
            Temporal::Date { date } => parse_date(date).map(|_| None).ok_or(Code::InvalidTemporal),
            Temporal::Floating { local } => parse_local(local)
                .map(|_| None)
                .ok_or(Code::InvalidTemporal),
            Temporal::Utc { value } => parse_utc(value).map(Some).ok_or(Code::InvalidTemporal),
            Temporal::Zoned { local, tzid } => {
                let local = parse_local(local).ok_or(Code::InvalidTemporal)?;
                let mut saw_source_tzid = false;
                for tz in timezones.iter().filter(|z| z.tzid == *tzid) {
                    saw_source_tzid = true;
                    if !tz.vtimezone.trim().is_empty() {
                        return Err(Code::UnsupportedTimezone);
                    }
                }
                if saw_source_tzid {
                    return Err(Code::UnknownTimezone);
                }
                let tz = tzid.parse::<Tz>().map_err(|_| Code::UnknownTimezone)?;
                Ok(Some(resolve_zoned(local, tz)))
            }
        }
    }

    /// Canonical identity used to locate recurrence exceptions: kind, zone and original value.
    pub fn recurrence_key(&self) -> String {
        match self {
            Temporal::Date { date } => format!("date:{date}"),
            Temporal::Utc { value } => format!("utc:{value}"),
            Temporal::Zoned { local, tzid } => format!("zoned:{tzid}:{local}"),
            Temporal::Floating { local } => format!("floating:{local}"),
        }
    }
}

/// Fixed key for non-recurring events.
pub const SINGLE_KEY: &str = "single";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Tentative,
    Confirmed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    Public,
    Private,
    Confidential,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transparency {
    Opaque,
    Transparent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Chair,
    Required,
    Optional,
    NonParticipant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Participation {
    NeedsAction,
    Accepted,
    Declined,
    Tentative,
    Delegated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Extension {
    pub name: String,
    pub parameters: std::collections::BTreeMap<String, Vec<String>>,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Person {
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub participation: Option<Participation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rsvp: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<Extension>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attachment {
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timezone {
    pub tzid: String,
    pub vtimezone: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlarmAction {
    Display,
    Audio,
    Email,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Related {
    Start,
    End,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AlarmTrigger {
    Relative { related: Related, offset: String },
    Absolute { at: Temporal },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Alarm {
    pub id: String,
    pub action: AlarmAction,
    pub trigger: AlarmTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_interval: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attendees: Option<Vec<Person>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<Attachment>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<Extension>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeriodKind {
    Period,
}

/// RDATE `PERIOD`: independent start and exactly one of end or duration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Period {
    pub kind: PeriodKind,
    pub start: Temporal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<Temporal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RDate {
    Period(Period),
    Temporal(Temporal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecurrenceRange {
    Single,
    ThisAndFuture,
}

fn double_option<'de, T, D>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// Fields an override changes. Absent = inherit; explicit `null` (`Some(None)`) clears a
/// clearable field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub description: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub location: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub url: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification: Option<Classification>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transparency: Option<Transparency>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<Temporal>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub end: Option<Option<Temporal>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub duration: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub organizer: Option<Option<Person>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attendees: Option<Vec<Person>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alarms: Option<Vec<Alarm>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<Attachment>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<Extension>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Change {
    Cancelled,
    Override { patch: Box<EventPatch> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecurrenceException {
    pub recurrence_id: Temporal,
    pub range: RecurrenceRange,
    pub change: Change,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recurrence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rrule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rdates: Option<Vec<RDate>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exdates: Option<Vec<Temporal>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exceptions: Option<Vec<RecurrenceException>>,
}

/// Normalized source event. Server-assigned identity and revision live outside this document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventV1 {
    pub schema_version: SchemaVersion,
    pub external_uid: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<String>>,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification: Option<Classification>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transparency: Option<Transparency>,
    pub start: Temporal,
    /// Mutually exclusive with `duration`; enforced by [`crate::validate_event`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<Temporal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<Recurrence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organizer: Option<Person>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attendees: Option<Vec<Person>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alarms: Option<Vec<Alarm>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<Attachment>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_modified_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezones: Option<Vec<Timezone>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<Extension>>,
}

impl EventV1 {
    /// Resolve a temporal value to a UTC instant using this event's source `timezones` context.
    pub fn resolve_utc(&self, temporal: &Temporal) -> Result<Option<DateTime<Utc>>, Code> {
        temporal.to_utc_with_timezones(self.timezones.as_deref().unwrap_or(&[]))
    }

    /// Key identifying an occurrence of this event: `single` for non-recurring events, else the
    /// canonical key of the original `RECURRENCE-ID` (never the rescheduled start).
    pub fn original_recurrence_key(&self, recurrence_id: Option<&Temporal>) -> String {
        recurrence_id.map_or_else(|| SINGLE_KEY.to_owned(), Temporal::recurrence_key)
    }
}
