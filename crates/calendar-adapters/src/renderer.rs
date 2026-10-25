//! Restricted, loss-aware RFC 5545 subscription renderer. See docs/ics-rendering.md.

use std::collections::{BTreeMap, HashSet};

use calendar_domain::{
    validate_event, Change, Classification, EventV1, RDate, RecurrenceRange, Status, Temporal,
    Transparency,
};
use sha2::{Digest, Sha256};

use crate::{normalize_ics, IcsLimits};

/// Metadata is supplied from one consistent repository snapshot. It must describe the latest
/// internal event revision, including deletion; source SEQUENCE/DTSTAMP are never used.
#[derive(Debug, Clone, Copy)]
pub struct FeedEvent<'a> {
    pub event_id: &'a str,
    /// Positive internal revision, mapped to SEQUENCE = revision - 1.
    pub revision: u64,
    /// UTC YYYY-MM-DDTHH:MM:SSZ, stable until the revision changes.
    pub changed_at: &'a str,
    /// Retain the last document when deleting, so the tombstone keeps DTSTART and UID.
    pub deleted: bool,
    pub document: &'a EventV1,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    #[error("invalid renderer metadata: {0}")]
    Metadata(&'static str),
    #[error("invalid event: {0}")]
    InvalidEvent(String),
    #[error("unsupported renderer capability: {0}")]
    Unsupported(&'static str),
    #[error("invalid or conflicting VTIMEZONE: {0}")]
    Timezone(String),
}

/// Render atomically in memory. Input ordering is preserved; no clock, I/O, or recurrence
/// expansion occurs. Errors return no partial feed. See the documented support profile.
pub fn render_ics(events: &[FeedEvent<'_>]) -> Result<String, RenderError> {
    let mut zones = BTreeMap::new();
    let mut ids = HashSet::new();
    for item in events {
        if item.event_id.is_empty() || !ids.insert(item.event_id) {
            return Err(RenderError::Metadata(
                "empty or duplicate internal event ID",
            ));
        }
        if item.revision == 0 || item.revision - 1 > i32::MAX as u64 {
            return Err(RenderError::Metadata(
                "revision outside SEQUENCE integer range",
            ));
        }
        let stamp = Temporal::Utc {
            value: item.changed_at.to_owned(),
        };
        if stamp.to_utc().is_err() || !four_digit_year(item.changed_at) {
            return Err(RenderError::Metadata("invalid revision timestamp"));
        }
        validate_event(item.document).map_err(|e| RenderError::InvalidEvent(format!("{e:?}")))?;
        check_profile(item.document)?;
        let mut bound_zones = BTreeMap::new();
        for zone in item.document.timezones.as_deref().unwrap_or_default() {
            let canonical = validate_zone(&zone.tzid, &zone.vtimezone)?;
            bound_zones.insert(zone.tzid.clone(), canonical.clone());
            if let Some(old) = zones.insert(zone.tzid.clone(), canonical.clone()) {
                if old != canonical {
                    return Err(RenderError::Timezone(zone.tzid.clone()));
                }
            }
        }
        // Bind each document before combining definitions. A custom definition from another
        // event must never silently override this event's unresolved IANA identifier.
        check_bindings(item.document, &bound_zones)?;
    }
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_owned(),
        "VERSION:2.0".to_owned(),
        "PRODID:-//CalRelay//Restricted Feed v1//EN".to_owned(),
        "CALSCALE:GREGORIAN".to_owned(),
    ];
    for zone in zones.values() {
        lines.extend(unfold(zone));
    }
    for item in events {
        write_event(&mut lines, item, &zones, None)?;
        if !item.deleted && item.document.status != Status::Cancelled {
            if let Some(recurrence) = &item.document.recurrence {
                for exception in recurrence.exceptions.as_deref().unwrap_or_default() {
                    write_event(&mut lines, item, &zones, Some(&exception.recurrence_id))?;
                }
            }
        }
    }
    lines.push("END:VCALENDAR".to_owned());
    let mut output = String::new();
    for line in lines {
        fold(&line, &mut output)?;
    }
    Ok(output)
}

fn check_bindings(event: &EventV1, zones: &BTreeMap<String, String>) -> Result<(), RenderError> {
    let mut values = vec![&event.start];
    values.extend(event.end.as_ref());
    if let Some(recurrence) = &event.recurrence {
        values.extend(recurrence.exdates.as_deref().unwrap_or_default());
        for date in recurrence.rdates.as_deref().unwrap_or_default() {
            match date {
                RDate::Temporal(value) => values.push(value),
                RDate::Period(period) => {
                    values.push(&period.start);
                    values.extend(period.end.as_ref());
                }
            }
        }
        for exception in recurrence.exceptions.as_deref().unwrap_or_default() {
            values.push(&exception.recurrence_id);
        }
    }
    for value in values {
        temporal("CHECK", value, zones)?;
    }
    Ok(())
}

fn four_digit_year(s: &str) -> bool {
    s.as_bytes()
        .get(..4)
        .is_some_and(|v| v.iter().all(u8::is_ascii_digit))
        && !s.starts_with("0000")
}

fn check_profile(event: &EventV1) -> Result<(), RenderError> {
    if event.organizer.is_some()
        || event.attendees.as_ref().is_some_and(|v| !v.is_empty())
        || event.alarms.as_ref().is_some_and(|v| !v.is_empty())
        || event.attachments.as_ref().is_some_and(|v| !v.is_empty())
        || event.extensions.as_ref().is_some_and(|v| !v.is_empty())
    {
        return Err(RenderError::Unsupported(
            "people, alarms, attachments or extensions",
        ));
    }
    if let Some(recurrence) = &event.recurrence {
        for date in recurrence.rdates.as_deref().unwrap_or_default() {
            if let RDate::Period(period) = date {
                if matches!(period.start, Temporal::Date { .. }) {
                    return Err(RenderError::Unsupported("DATE-valued RDATE PERIOD"));
                }
                if let Some(end) = &period.end {
                    let compatible = match (&period.start, end) {
                        (Temporal::Utc { .. }, Temporal::Utc { .. })
                        | (Temporal::Floating { .. }, Temporal::Floating { .. }) => true,
                        (
                            Temporal::Zoned { tzid: start, .. },
                            Temporal::Zoned { tzid: end, .. },
                        ) => start == end,
                        _ => false,
                    };
                    if !compatible {
                        return Err(RenderError::Unsupported(
                            "RDATE PERIOD endpoint timezone bindings",
                        ));
                    }
                }
            }
        }
        for exception in recurrence.exceptions.as_deref().unwrap_or_default() {
            if exception.range != RecurrenceRange::Single
                || !matches!(exception.change, Change::Cancelled)
            {
                return Err(RenderError::Unsupported(
                    "override or ranged recurrence exception",
                ));
            }
        }
    }
    Ok(())
}

/// Reuse the bounded ingestion grammar to verify a source component before copying it into a
/// feed. A synthetic event exposes its timezone bindings; extra injected components are rejected.
fn validate_zone(tzid: &str, raw: &str) -> Result<String, RenderError> {
    if tzid.is_empty()
        || tzid
            .chars()
            .any(|c| c.is_control() || matches!(c, '"' | ';' | ':' | ','))
    {
        return Err(RenderError::Timezone(tzid.to_owned()));
    }
    let probe = format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//CalRelay//Zone probe//EN\r\n{raw}\r\nBEGIN:VEVENT\r\nUID:zone-probe\r\nDTSTAMP:20260101T000000Z\r\nDTSTART;TZID=\"{tzid}\":20260101T000000\r\nSUMMARY:probe\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n");
    let parsed = normalize_ics(&probe, &IcsLimits::default())
        .map_err(|_| RenderError::Timezone(tzid.to_owned()))?;
    if parsed.events.len() != 1 || !parsed.diagnostics.is_empty() {
        return Err(RenderError::Timezone(tzid.to_owned()));
    }
    let event = &parsed.events[0];
    let zones = event.timezones.as_deref().unwrap_or_default();
    if event.external_uid != "zone-probe"
        || zones.len() != 1
        || zones[0].tzid != tzid
        || unfold(raw) != unfold(&zones[0].vtimezone)
    {
        return Err(RenderError::Timezone(tzid.to_owned()));
    }
    Ok(zones[0].vtimezone.clone())
}

fn write_event(
    lines: &mut Vec<String>,
    item: &FeedEvent<'_>,
    zones: &BTreeMap<String, String>,
    rid: Option<&Temporal>,
) -> Result<(), RenderError> {
    let event = item.document;
    lines.push("BEGIN:VEVENT".to_owned());
    lines.push(format!(
        "UID:{:x}@calrelay",
        Sha256::digest(item.event_id.as_bytes())
    ));
    lines.push(format!("DTSTAMP:{}", compact(item.changed_at)));
    lines.push(format!("SEQUENCE:{}", item.revision - 1));
    lines.push(temporal("DTSTART", rid.unwrap_or(&event.start), zones)?);
    let cancelled = item.deleted || event.status == Status::Cancelled || rid.is_some();
    if let Some(rid) = rid {
        lines.push(temporal("RECURRENCE-ID", rid, zones)?);
    }
    if rid.is_none() {
        if let Some(end) = &event.end {
            lines.push(temporal("DTEND", end, zones)?);
        }
        if let Some(duration) = &event.duration {
            lines.push(format!("DURATION:{duration}"));
        }
    }
    lines.push(format!("SUMMARY:{}", text(&event.title)?));
    lines.push(format!(
        "STATUS:{}",
        if cancelled {
            "CANCELLED"
        } else {
            match event.status {
                Status::Tentative => "TENTATIVE",
                _ => "CONFIRMED",
            }
        }
    ));
    if rid.is_none() {
        for (name, value) in [
            ("DESCRIPTION", &event.description),
            ("LOCATION", &event.location),
        ] {
            if let Some(value) = value {
                lines.push(format!("{name}:{}", text(value)?));
            }
        }
        if let Some(url) = &event.url {
            lines.push(format!("URL:{url}"));
        }
        if let Some(categories) = &event.categories {
            if !categories.is_empty() {
                lines.push(format!(
                    "CATEGORIES:{}",
                    categories
                        .iter()
                        .map(|v| text(v))
                        .collect::<Result<Vec<_>, _>>()?
                        .join(",")
                ));
            }
        }
        if let Some(class) = event.classification {
            lines.push(format!(
                "CLASS:{}",
                match class {
                    Classification::Public => "PUBLIC",
                    Classification::Private => "PRIVATE",
                    Classification::Confidential => "CONFIDENTIAL",
                }
            ));
        }
        if let Some(transp) = event.transparency {
            lines.push(format!(
                "TRANSP:{}",
                match transp {
                    Transparency::Opaque => "OPAQUE",
                    Transparency::Transparent => "TRANSPARENT",
                }
            ));
        }
        if let Some(recurrence) = &event.recurrence {
            if let Some(rule) = &recurrence.rrule {
                lines.push(format!("RRULE:{rule}"));
            }
            for date in recurrence.exdates.as_deref().unwrap_or_default() {
                lines.push(temporal("EXDATE", date, zones)?);
            }
            for date in recurrence.rdates.as_deref().unwrap_or_default() {
                match date {
                    RDate::Temporal(value) => lines.push(temporal("RDATE", value, zones)?),
                    RDate::Period(period) => {
                        let start = temporal("RDATE", &period.start, zones)?;
                        let (head, value) = start
                            .split_once(':')
                            .ok_or(RenderError::Unsupported("RDATE PERIOD"))?;
                        let end = if let Some(end) = &period.end {
                            temporal("END", end, zones)?
                                .split_once(':')
                                .ok_or(RenderError::Unsupported("RDATE PERIOD"))?
                                .1
                                .to_owned()
                        } else {
                            period
                                .duration
                                .clone()
                                .ok_or(RenderError::Unsupported("RDATE PERIOD"))?
                        };
                        lines.push(format!("{head};VALUE=PERIOD:{value}/{end}"));
                    }
                }
            }
        }
    }
    lines.push("END:VEVENT".to_owned());
    Ok(())
}

fn compact(value: &str) -> String {
    value.replace(['-', ':'], "")
}

fn temporal(
    name: &str,
    value: &Temporal,
    zones: &BTreeMap<String, String>,
) -> Result<String, RenderError> {
    let (head, body) = match value {
        Temporal::Date { date } => (format!("{name};VALUE=DATE"), date.as_str()),
        Temporal::Utc { value } => (name.to_owned(), value.as_str()),
        Temporal::Floating { local } => (name.to_owned(), local.as_str()),
        Temporal::Zoned { local, tzid } => {
            if !zones.contains_key(tzid) {
                return Err(RenderError::Unsupported(
                    "zoned time requires source VTIMEZONE",
                ));
            }
            if tzid
                .chars()
                .any(|c| c.is_control() || matches!(c, '"' | ';' | ':' | ','))
            {
                return Err(RenderError::Unsupported("TZID parameter encoding"));
            }
            (format!("{name};TZID=\"{tzid}\""), local.as_str())
        }
    };
    if !four_digit_year(body) {
        return Err(RenderError::Unsupported("year outside 0001..9999"));
    }
    Ok(format!("{head}:{}", compact(body)))
}

fn text(value: &str) -> Result<String, RenderError> {
    if value
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(RenderError::Unsupported("TEXT control character"));
    }
    Ok(value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace(';', "\\;")
        .replace(',', "\\,"))
}

fn unfold(raw: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix([' ', '\t']) {
            if let Some(last) = lines.last_mut() {
                last.push_str(rest);
            }
        } else {
            lines.push(line.to_owned());
        }
    }
    lines
}

fn fold(line: &str, output: &mut String) -> Result<(), RenderError> {
    if line.chars().any(|c| c.is_control() && c != '\t') {
        return Err(RenderError::Unsupported("content line control character"));
    }
    let mut octets = 0;
    for c in line.chars() {
        if octets + c.len_utf8() > 75 {
            output.push_str("\r\n ");
            octets = 1;
        }
        output.push(c);
        octets += c.len_utf8();
    }
    output.push_str("\r\n");
    Ok(())
}
