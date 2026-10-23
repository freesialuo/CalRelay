//! Bounded RFC 5545 ICS file normalization into [`EventV1`] and [`SyncBatch`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Cursor;

use calendar_application::{
    BatchError, DeletionPolicy, EventV1, SyncBatch, SyncMode, SyncRecord, MAX_BATCH_RECORDS,
};
use calendar_domain::{
    validate_event, Alarm, AlarmAction, AlarmTrigger, Attachment, Change, Classification,
    EventPatch, Extension, Participation, Period, PeriodKind, Person, RDate, Recurrence,
    RecurrenceException, RecurrenceRange, Related, Role, SchemaVersion, Status, Temporal, Timezone,
    Transparency, ValidationError,
};

pub const DEFAULT_MAX_ICS_BYTES: usize = 1_048_576;
pub const DEFAULT_MAX_ICS_PROPERTIES: usize = 10_000;
pub const DEFAULT_MAX_ICS_EVENTS: usize = 1_000;
pub const DEFAULT_MAX_ICS_PARSE_STEPS: usize = 25_000;

const MAX_SHORT_STRING_LEN: usize = 1_024;
const MAX_TEXT_STRING_LEN: usize = 65_536;
const MAX_RRULE_LEN: usize = 8_192;
const MAX_VTIMEZONE_LEN: usize = 262_144;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IcsLimits {
    pub max_bytes: usize,
    pub max_properties: usize,
    pub max_events: usize,
    pub max_parse_steps: usize,
}

impl Default for IcsLimits {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_ICS_BYTES,
            max_properties: DEFAULT_MAX_ICS_PROPERTIES,
            max_events: DEFAULT_MAX_ICS_EVENTS,
            max_parse_steps: DEFAULT_MAX_ICS_PARSE_STEPS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IcsBudgetKind {
    InvalidLimits,
    Bytes,
    Properties,
    Events,
    ParseSteps,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetainedComponentKind {
    VTodo,
    VJournal,
    VFreeBusy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IcsDiagnostic {
    pub kind: RetainedComponentKind,
    pub uid: Option<String>,
    pub raw: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedIcs {
    pub events: Vec<EventV1>,
    pub diagnostics: Vec<IcsDiagnostic>,
}

#[derive(Debug, Clone)]
pub struct IcsSnapshotInput {
    pub source_id: String,
    pub calendar_id: String,
    pub base_revision: i64,
    pub deletion_policy: DeletionPolicy,
    pub proposed_checkpoint: Option<String>,
    pub verified_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IcsError {
    #[error("ICS budget exceeded ({kind:?}): limit {limit}, observed {observed}")]
    BudgetExceeded {
        kind: IcsBudgetKind,
        limit: usize,
        observed: usize,
    },
    #[error("malformed ICS syntax at {location}: {reason}")]
    Malformed { location: String, reason: String },
    #[error("unsupported ICS capability in {component} (uid={uid:?}, item={item}): {reason}")]
    Unsupported {
        component: String,
        uid: Option<String>,
        item: String,
        reason: String,
    },
    #[error("event `{uid}` failed contract validation: {reason}")]
    ContractViolation { uid: String, reason: String },
    #[error("event `{uid}` failed domain validation")]
    DomainValidation {
        uid: String,
        event: Box<EventV1>,
        errors: Vec<ValidationError>,
    },
    #[error("sync batch validation failed: {0}")]
    BatchShape(&'static str),
}

struct BudgetTracker {
    limits: IcsLimits,
    steps: usize,
    properties: usize,
    events: usize,
}

impl BudgetTracker {
    fn new(limits: &IcsLimits) -> Result<Self, IcsError> {
        if limits.max_bytes == 0
            || limits.max_properties == 0
            || limits.max_events == 0
            || limits.max_events > MAX_BATCH_RECORDS
            || limits.max_parse_steps == 0
        {
            return Err(IcsError::BudgetExceeded {
                kind: IcsBudgetKind::InvalidLimits,
                limit: 0,
                observed: 0,
            });
        }
        Ok(Self {
            limits: *limits,
            steps: 0,
            properties: 0,
            events: 0,
        })
    }

    fn step(&mut self) -> Result<(), IcsError> {
        self.steps = self.steps.saturating_add(1);
        if self.steps > self.limits.max_parse_steps {
            return Err(IcsError::BudgetExceeded {
                kind: IcsBudgetKind::ParseSteps,
                limit: self.limits.max_parse_steps,
                observed: self.steps,
            });
        }
        Ok(())
    }

    fn property(&mut self) -> Result<(), IcsError> {
        self.properties = self.properties.saturating_add(1);
        if self.properties > self.limits.max_properties {
            return Err(IcsError::BudgetExceeded {
                kind: IcsBudgetKind::Properties,
                limit: self.limits.max_properties,
                observed: self.properties,
            });
        }
        Ok(())
    }

    fn event(&mut self) -> Result<(), IcsError> {
        self.events = self.events.saturating_add(1);
        if self.events > self.limits.max_events {
            return Err(IcsError::BudgetExceeded {
                kind: IcsBudgetKind::Events,
                limit: self.limits.max_events,
                observed: self.events,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct ParsedProperty {
    line_no: usize,
    name: String,
    params: Vec<(String, Vec<String>)>,
    value: String,
}

impl ParsedProperty {
    fn location(&self) -> String {
        format!("line {} ({})", self.line_no, self.name)
    }

    fn single_param(&self, key: &str) -> Result<Option<&str>, IcsError> {
        let mut found: Option<&str> = None;
        for (k, vals) in &self.params {
            if k == key {
                if vals.len() != 1 || found.is_some() {
                    return Err(IcsError::Malformed {
                        location: self.location(),
                        reason: format!("parameter `{key}` must have a single value"),
                    });
                }
                found = Some(vals[0].as_str());
            }
        }
        Ok(found)
    }

    fn check_allowed_params(
        &self,
        component: &str,
        uid: Option<&str>,
        allowed: &[&str],
        allow_x_params: bool,
    ) -> Result<(), IcsError> {
        for (k, _) in &self.params {
            if allowed.contains(&k.as_str()) {
                continue;
            }
            if allow_x_params && k.starts_with("X-") {
                continue;
            }
            return Err(IcsError::Unsupported {
                component: component.to_owned(),
                uid: uid.map(str::to_owned),
                item: format!("{};{k}", self.name),
                reason: format!("unsupported parameter `{k}` on `{}`", self.name),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct ParsedAlarm {
    properties: Vec<ParsedProperty>,
}

#[derive(Debug, Clone)]
struct ParsedEventComponent {
    properties: Vec<ParsedProperty>,
    alarms: Vec<ParsedAlarm>,
}

#[derive(Debug, Clone)]
struct ParsedTransition {
    kind: &'static str,
    properties: Vec<ParsedProperty>,
}

#[derive(Debug, Clone)]
struct ParsedTimezoneComponent {
    properties: Vec<ParsedProperty>,
    transitions: Vec<ParsedTransition>,
    raw_lines: Vec<String>,
}

#[derive(Debug, Clone)]
struct RetainedBlock {
    kind: RetainedComponentKind,
    properties: Vec<ParsedProperty>,
    raw_lines: Vec<String>,
}

#[derive(Debug, Default)]
struct ScannedCalendar {
    properties: Vec<ParsedProperty>,
    timezones: Vec<ParsedTimezoneComponent>,
    events: Vec<ParsedEventComponent>,
    retained: Vec<RetainedBlock>,
}

pub fn normalize_ics(input: &str, limits: &IcsLimits) -> Result<NormalizedIcs, IcsError> {
    let mut budget = BudgetTracker::new(limits)?;
    if input.len() > limits.max_bytes {
        return Err(IcsError::BudgetExceeded {
            kind: IcsBudgetKind::Bytes,
            limit: limits.max_bytes,
            observed: input.len(),
        });
    }

    let unfolded = unfold_content_lines(input, &mut budget)?;
    let (scanned, canonical_crlf) = scan_and_validate_structure(&unfolded, &mut budget)?;

    // Also run the selected `ical 0.11.0` parser over the validated CRLF stream to ensure
    // compatibility with the #18 low-level parser candidate while keeping our strict value checks.
    verify_with_ical_parser(&canonical_crlf, &scanned)?;

    validate_vcalendar_properties(&scanned.properties)?;
    let timezone_defs = normalize_timezones(&scanned.timezones, &mut budget)?;
    let diagnostics = build_retained_diagnostics(&scanned.retained)?;
    let events = normalize_events(&scanned.events, &timezone_defs, &mut budget)?;

    Ok(NormalizedIcs {
        events,
        diagnostics,
    })
}

pub fn normalize_ics_to_snapshot_batch(
    input: &str,
    limits: &IcsLimits,
    snapshot: IcsSnapshotInput,
) -> Result<(SyncBatch, Vec<IcsDiagnostic>), IcsError> {
    if snapshot.source_id.is_empty()
        || snapshot.calendar_id.is_empty()
        || snapshot.base_revision < 0
    {
        return Err(IcsError::BatchShape(
            "invalid source, calendar or base revision",
        ));
    }
    let normalized = normalize_ics(input, limits)?;
    let records = normalized
        .events
        .into_iter()
        .map(|event| SyncRecord::Upsert {
            calendar_id: snapshot.calendar_id.clone(),
            event: Box::new(event),
        })
        .collect();
    let batch = SyncBatch {
        source_id: snapshot.source_id,
        base_revision: snapshot.base_revision,
        mode: SyncMode::FullSnapshot {
            complete: true,
            deletion_policy: snapshot.deletion_policy,
        },
        records,
        proposed_checkpoint: snapshot.proposed_checkpoint,
        verified_at: snapshot.verified_at,
    };
    batch.validate().map_err(|err| match err {
        BatchError::Shape(msg) => IcsError::BatchShape(msg),
        BatchError::InvalidEvent { errors } => IcsError::ContractViolation {
            uid: String::new(),
            reason: format!("batch event validation failed: {errors:?}"),
        },
    })?;
    Ok((batch, normalized.diagnostics))
}

fn unfold_content_lines(
    input: &str,
    budget: &mut BudgetTracker,
) -> Result<Vec<(usize, String)>, IcsError> {
    if input.is_empty() {
        return Err(IcsError::Malformed {
            location: "line 1".to_owned(),
            reason: "empty ICS input".to_owned(),
        });
    }

    let bytes = input.as_bytes();
    let mut physical_lines: Vec<(usize, &str)> = Vec::new();
    let mut start = 0usize;
    let mut line_no = 1usize;
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b'\r' => {
                if bytes.get(i + 1) != Some(&b'\n') {
                    return Err(IcsError::Malformed {
                        location: format!("line {line_no}"),
                        reason: "bare CR without LF".to_owned(),
                    });
                }
                budget.step()?;
                physical_lines.push((line_no, &input[start..i]));
                i += 2;
                start = i;
                line_no += 1;
            }
            b'\n' => {
                budget.step()?;
                physical_lines.push((line_no, &input[start..i]));
                i += 1;
                start = i;
                line_no += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    if start < bytes.len() {
        budget.step()?;
        physical_lines.push((line_no, &input[start..]));
    }

    let mut unfolded: Vec<(usize, String)> = Vec::new();
    for (idx, (num, raw_line)) in physical_lines.iter().enumerate() {
        if raw_line.is_empty() {
            if idx + 1 == physical_lines.len() {
                continue;
            }
            return Err(IcsError::Malformed {
                location: format!("line {num}"),
                reason: "empty physical line inside ICS content".to_owned(),
            });
        }
        if let Some(rest) = raw_line
            .strip_prefix(' ')
            .or_else(|| raw_line.strip_prefix('\t'))
        {
            let Some((_, prev)) = unfolded.last_mut() else {
                return Err(IcsError::Malformed {
                    location: format!("line {num}"),
                    reason: "folded continuation line at start of file".to_owned(),
                });
            };
            prev.push_str(rest);
        } else {
            unfolded.push((*num, (*raw_line).to_owned()));
        }
    }

    if unfolded.is_empty() {
        return Err(IcsError::Malformed {
            location: "line 1".to_owned(),
            reason: "no content lines found".to_owned(),
        });
    }

    Ok(unfolded)
}

fn parse_content_line(
    line_no: usize,
    line: &str,
    budget: &mut BudgetTracker,
) -> Result<ParsedProperty, IcsError> {
    budget.step()?;
    let bytes = line.as_bytes();
    let mut idx = 0usize;
    while idx < bytes.len() && bytes[idx] != b';' && bytes[idx] != b':' {
        let b = bytes[idx];
        if !(b.is_ascii_alphanumeric() || b == b'-') {
            return Err(IcsError::Malformed {
                location: format!("line {line_no}"),
                reason: format!("invalid character in property name `{line}`"),
            });
        }
        idx += 1;
    }
    if idx == 0 || idx >= bytes.len() {
        return Err(IcsError::Malformed {
            location: format!("line {line_no}"),
            reason: "missing property name or value delimiter".to_owned(),
        });
    }

    let name = line[..idx].to_ascii_uppercase();
    let mut params: Vec<(String, Vec<String>)> = Vec::new();

    while idx < bytes.len() && bytes[idx] == b';' {
        idx += 1; // skip ';'
        let key_start = idx;
        while idx < bytes.len() && bytes[idx] != b'=' && bytes[idx] != b';' && bytes[idx] != b':' {
            let b = bytes[idx];
            if !(b.is_ascii_alphanumeric() || b == b'-') {
                return Err(IcsError::Malformed {
                    location: format!("line {line_no}"),
                    reason: "invalid character in parameter name".to_owned(),
                });
            }
            idx += 1;
        }
        if idx == key_start || bytes.get(idx) != Some(&b'=') {
            return Err(IcsError::Malformed {
                location: format!("line {line_no}"),
                reason: "malformed property parameter key".to_owned(),
            });
        }
        let param_key = line[key_start..idx].to_ascii_uppercase();
        if param_key.len() > MAX_SHORT_STRING_LEN {
            return Err(IcsError::Malformed {
                location: format!("line {line_no}"),
                reason: "parameter name exceeds maximum length".to_owned(),
            });
        }
        idx += 1; // skip '='

        let mut param_values: Vec<String> = Vec::new();
        loop {
            budget.step()?;
            if bytes.get(idx) == Some(&b'"') {
                idx += 1; // skip opening quote
                let val_start = idx;
                while idx < bytes.len() && bytes[idx] != b'"' {
                    idx += 1;
                }
                if bytes.get(idx) != Some(&b'"') {
                    return Err(IcsError::Malformed {
                        location: format!("line {line_no}"),
                        reason: "unclosed quoted parameter value".to_owned(),
                    });
                }
                let val = line[val_start..idx].to_owned();
                if val.is_empty() || val.len() > MAX_SHORT_STRING_LEN {
                    return Err(IcsError::Malformed {
                        location: format!("line {line_no}"),
                        reason: "empty or oversized quoted parameter value".to_owned(),
                    });
                }
                param_values.push(val);
                idx += 1; // skip closing quote
            } else {
                let val_start = idx;
                while idx < bytes.len()
                    && bytes[idx] != b','
                    && bytes[idx] != b';'
                    && bytes[idx] != b':'
                {
                    if bytes[idx] == b'"' {
                        return Err(IcsError::Malformed {
                            location: format!("line {line_no}"),
                            reason: "unexpected quote inside unquoted parameter value".to_owned(),
                        });
                    }
                    idx += 1;
                }
                let val = line[val_start..idx].to_owned();
                if val.is_empty() || val.len() > MAX_SHORT_STRING_LEN {
                    return Err(IcsError::Malformed {
                        location: format!("line {line_no}"),
                        reason: "empty or oversized parameter value".to_owned(),
                    });
                }
                param_values.push(val);
            }

            if bytes.get(idx) == Some(&b',') {
                idx += 1;
            } else {
                break;
            }
        }
        if param_values.len() > 10 {
            return Err(IcsError::Malformed {
                location: format!("line {line_no}"),
                reason: "parameter exceeds maximum comma-separated values per attribute".to_owned(),
            });
        }
        params.push((param_key, param_values));
    }

    if bytes.get(idx) != Some(&b':') {
        return Err(IcsError::Malformed {
            location: format!("line {line_no}"),
            reason: "missing `:` before property value".to_owned(),
        });
    }
    let value = line[idx + 1..].to_owned();

    Ok(ParsedProperty {
        line_no,
        name,
        params,
        value,
    })
}

#[derive(Debug)]
enum ActiveFrame {
    VCalendar,
    VTimezone(ParsedTimezoneComponent),
    Transition {
        tz: ParsedTimezoneComponent,
        tr: ParsedTransition,
    },
    VEvent(ParsedEventComponent),
    VEventAlarm {
        event: ParsedEventComponent,
        alarm: ParsedAlarm,
    },
    Retained {
        block: RetainedBlock,
        nested_depth: usize,
    },
}

fn scan_and_validate_structure(
    unfolded: &[(usize, String)],
    budget: &mut BudgetTracker,
) -> Result<(ScannedCalendar, String), IcsError> {
    let mut scanned = ScannedCalendar::default();
    let mut stack: Vec<(String, ActiveFrame)> = Vec::new();
    let mut saw_vcalendar = false;
    let mut canonical_crlf = String::new();

    for (line_no, raw_line) in unfolded {
        let prop = parse_content_line(*line_no, raw_line, budget)?;
        if prop.name == "BEGIN" || prop.name == "END" {
            canonical_crlf.push_str(&prop.name);
            canonical_crlf.push(':');
            canonical_crlf.push_str(&prop.value.to_ascii_uppercase());
            canonical_crlf.push_str("\r\n");
        } else if prop.value.is_empty() {
            // `ical 0.11.0` requires `to_parse.len() > 1` at `:` and strips leading colons via
            // `trim_start_matches(':')`, so doubling the colon preserves an empty value in `ical`.
            canonical_crlf.push_str(raw_line);
            canonical_crlf.push_str(":\r\n");
        } else {
            canonical_crlf.push_str(raw_line);
            canonical_crlf.push_str("\r\n");
        }

        if prop.name == "BEGIN" {
            if !prop.params.is_empty() || prop.value.is_empty() {
                return Err(IcsError::Malformed {
                    location: prop.location(),
                    reason: "BEGIN line must not have parameters or empty component name"
                        .to_owned(),
                });
            }
            let comp = prop.value.to_ascii_uppercase();
            match stack.pop() {
                None => {
                    if comp != "VCALENDAR" || saw_vcalendar {
                        return Err(IcsError::Malformed {
                            location: prop.location(),
                            reason: "top-level component must be a single VCALENDAR".to_owned(),
                        });
                    }
                    saw_vcalendar = true;
                    stack.push((comp, ActiveFrame::VCalendar));
                }
                Some((parent_name, ActiveFrame::VCalendar)) => {
                    stack.push((parent_name, ActiveFrame::VCalendar));
                    match comp.as_str() {
                        "VTIMEZONE" => {
                            stack.push((
                                comp,
                                ActiveFrame::VTimezone(ParsedTimezoneComponent {
                                    properties: Vec::new(),
                                    transitions: Vec::new(),
                                    raw_lines: vec![raw_line.clone()],
                                }),
                            ));
                        }
                        "VEVENT" => {
                            stack.push((
                                comp,
                                ActiveFrame::VEvent(ParsedEventComponent {
                                    properties: Vec::new(),
                                    alarms: Vec::new(),
                                }),
                            ));
                        }
                        "VTODO" | "VJOURNAL" | "VFREEBUSY" => {
                            let kind = match comp.as_str() {
                                "VTODO" => RetainedComponentKind::VTodo,
                                "VJOURNAL" => RetainedComponentKind::VJournal,
                                _ => RetainedComponentKind::VFreeBusy,
                            };
                            stack.push((
                                comp,
                                ActiveFrame::Retained {
                                    block: RetainedBlock {
                                        kind,
                                        properties: Vec::new(),
                                        raw_lines: vec![raw_line.clone()],
                                    },
                                    nested_depth: 0,
                                },
                            ));
                        }
                        other => {
                            return Err(IcsError::Unsupported {
                                component: "VCALENDAR".to_owned(),
                                uid: None,
                                item: other.to_owned(),
                                reason: format!("unsupported calendar subcomponent `{other}`"),
                            });
                        }
                    }
                }
                Some((_, ActiveFrame::VTimezone(mut tz))) => {
                    tz.raw_lines.push(raw_line.clone());
                    let kind = match comp.as_str() {
                        "STANDARD" => "STANDARD",
                        "DAYLIGHT" => "DAYLIGHT",
                        other => {
                            return Err(IcsError::Unsupported {
                                component: "VTIMEZONE".to_owned(),
                                uid: None,
                                item: other.to_owned(),
                                reason: format!("unsupported VTIMEZONE subcomponent `{other}`"),
                            });
                        }
                    };
                    stack.push((
                        comp,
                        ActiveFrame::Transition {
                            tz,
                            tr: ParsedTransition {
                                kind,
                                properties: Vec::new(),
                            },
                        },
                    ));
                }
                Some((_, ActiveFrame::VEvent(event))) => {
                    if comp != "VALARM" {
                        return Err(IcsError::Unsupported {
                            component: "VEVENT".to_owned(),
                            uid: None,
                            item: comp,
                            reason: "only VALARM subcomponents are supported in VEVENT".to_owned(),
                        });
                    }
                    stack.push((
                        comp,
                        ActiveFrame::VEventAlarm {
                            event,
                            alarm: ParsedAlarm {
                                properties: Vec::new(),
                            },
                        },
                    ));
                }
                Some((
                    ret_name,
                    ActiveFrame::Retained {
                        mut block,
                        nested_depth,
                    },
                )) => {
                    if comp != "VALARM" || nested_depth > 0 || ret_name != "VTODO" {
                        return Err(IcsError::Unsupported {
                            component: ret_name,
                            uid: None,
                            item: comp,
                            reason: "unsupported nested subcomponent in retained component"
                                .to_owned(),
                        });
                    }
                    block.raw_lines.push(raw_line.clone());
                    stack.push((
                        ret_name,
                        ActiveFrame::Retained {
                            block,
                            nested_depth: 1,
                        },
                    ));
                }
                Some((parent_name, _)) => {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: format!("component `{parent_name}` cannot contain `{comp}`"),
                    });
                }
            }
        } else if prop.name == "END" {
            if !prop.params.is_empty() || prop.value.is_empty() {
                return Err(IcsError::Malformed {
                    location: prop.location(),
                    reason: "END line must not have parameters or empty component name".to_owned(),
                });
            }
            let comp = prop.value.to_ascii_uppercase();
            let Some((open_name, frame)) = stack.pop() else {
                return Err(IcsError::Malformed {
                    location: prop.location(),
                    reason: format!("unexpected END:{comp} with no open component"),
                });
            };
            match frame {
                ActiveFrame::Retained {
                    mut block,
                    nested_depth: 1,
                } => {
                    if comp != "VALARM" {
                        return Err(IcsError::Malformed {
                            location: prop.location(),
                            reason: format!("expected END:VALARM, found END:{comp}"),
                        });
                    }
                    block.raw_lines.push(raw_line.clone());
                    stack.push((
                        open_name,
                        ActiveFrame::Retained {
                            block,
                            nested_depth: 0,
                        },
                    ));
                }
                _ => {
                    if open_name != comp {
                        return Err(IcsError::Malformed {
                            location: prop.location(),
                            reason: format!("mismatched component close: expected END:{open_name}, found END:{comp}"),
                        });
                    }
                    match frame {
                        ActiveFrame::VCalendar => {}
                        ActiveFrame::VTimezone(mut tz) => {
                            tz.raw_lines.push(raw_line.clone());
                            scanned.timezones.push(tz);
                        }
                        ActiveFrame::Transition { mut tz, tr } => {
                            tz.raw_lines.push(raw_line.clone());
                            tz.transitions.push(tr);
                            stack.push(("VTIMEZONE".to_owned(), ActiveFrame::VTimezone(tz)));
                        }
                        ActiveFrame::VEvent(event) => {
                            scanned.events.push(event);
                        }
                        ActiveFrame::VEventAlarm { mut event, alarm } => {
                            event.alarms.push(alarm);
                            stack.push(("VEVENT".to_owned(), ActiveFrame::VEvent(event)));
                        }
                        ActiveFrame::Retained { mut block, .. } => {
                            block.raw_lines.push(raw_line.clone());
                            scanned.retained.push(block);
                        }
                    }
                }
            }
        } else {
            budget.property()?;
            let Some((open_name, frame)) = stack.pop() else {
                return Err(IcsError::Malformed {
                    location: prop.location(),
                    reason: "property outside VCALENDAR".to_owned(),
                });
            };
            match frame {
                ActiveFrame::VCalendar => {
                    scanned.properties.push(prop);
                    stack.push((open_name, ActiveFrame::VCalendar));
                }
                ActiveFrame::VTimezone(mut tz) => {
                    tz.raw_lines.push(raw_line.clone());
                    tz.properties.push(prop);
                    stack.push((open_name, ActiveFrame::VTimezone(tz)));
                }
                ActiveFrame::Transition { mut tz, mut tr } => {
                    tz.raw_lines.push(raw_line.clone());
                    tr.properties.push(prop);
                    stack.push((open_name, ActiveFrame::Transition { tz, tr }));
                }
                ActiveFrame::VEvent(mut event) => {
                    event.properties.push(prop);
                    stack.push((open_name, ActiveFrame::VEvent(event)));
                }
                ActiveFrame::VEventAlarm { event, mut alarm } => {
                    alarm.properties.push(prop);
                    stack.push((open_name, ActiveFrame::VEventAlarm { event, alarm }));
                }
                ActiveFrame::Retained {
                    mut block,
                    nested_depth,
                } => {
                    block.raw_lines.push(raw_line.clone());
                    if nested_depth == 0 {
                        block.properties.push(prop);
                    }
                    stack.push((
                        open_name,
                        ActiveFrame::Retained {
                            block,
                            nested_depth,
                        },
                    ));
                }
            }
        }
    }

    if !stack.is_empty() || !saw_vcalendar {
        return Err(IcsError::Malformed {
            location: "EOF".to_owned(),
            reason: "unclosed component at end of ICS input".to_owned(),
        });
    }

    Ok((scanned, canonical_crlf))
}

fn verify_with_ical_parser(
    canonical_crlf: &str,
    scanned: &ScannedCalendar,
) -> Result<(), IcsError> {
    let mut parser = ical::IcalParser::new(Cursor::new(canonical_crlf.as_bytes()));
    let calendar = parser
        .next()
        .ok_or_else(|| IcsError::Malformed {
            location: "VCALENDAR".to_owned(),
            reason: "ical parser found no VCALENDAR".to_owned(),
        })?
        .map_err(|err| IcsError::Malformed {
            location: "VCALENDAR".to_owned(),
            reason: err.to_string(),
        })?;
    if parser.next().is_some() {
        return Err(IcsError::Malformed {
            location: "VCALENDAR".to_owned(),
            reason: "multiple VCALENDAR components are not supported".to_owned(),
        });
    }
    if calendar.events.len() != scanned.events.len()
        || calendar.timezones.len() != scanned.timezones.len()
    {
        return Err(IcsError::Malformed {
            location: "VCALENDAR".to_owned(),
            reason: "component count mismatch in ical parser".to_owned(),
        });
    }
    Ok(())
}

fn validate_vcalendar_properties(properties: &[ParsedProperty]) -> Result<(), IcsError> {
    let mut version: Option<&str> = None;
    let mut prodid: Option<&str> = None;
    let mut calscale: Option<&str> = None;
    let mut method: Option<&str> = None;

    for prop in properties {
        match prop.name.as_str() {
            "VERSION" => {
                prop.check_allowed_params("VCALENDAR", None, &["VALUE"], false)?;
                if version.is_some() {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate VERSION in VCALENDAR".to_owned(),
                    });
                }
                if prop.value != "2.0" {
                    return Err(IcsError::Unsupported {
                        component: "VCALENDAR".to_owned(),
                        uid: None,
                        item: format!("VERSION:{}", prop.value),
                        reason: "only iCalendar VERSION:2.0 is supported".to_owned(),
                    });
                }
                version = Some(prop.value.as_str());
            }
            "PRODID" => {
                prop.check_allowed_params("VCALENDAR", None, &["VALUE", "LANGUAGE"], false)?;
                if prodid.is_some()
                    || prop.value.is_empty()
                    || prop.value.len() > MAX_SHORT_STRING_LEN
                {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "missing, duplicate or oversized PRODID in VCALENDAR".to_owned(),
                    });
                }
                prodid = Some(prop.value.as_str());
            }
            "CALSCALE" => {
                prop.check_allowed_params("VCALENDAR", None, &["VALUE"], false)?;
                if calscale.is_some() {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate CALSCALE in VCALENDAR".to_owned(),
                    });
                }
                if !prop.value.eq_ignore_ascii_case("GREGORIAN") {
                    return Err(IcsError::Unsupported {
                        component: "VCALENDAR".to_owned(),
                        uid: None,
                        item: format!("CALSCALE:{}", prop.value),
                        reason: "only GREGORIAN calendar scale is supported".to_owned(),
                    });
                }
                calscale = Some(prop.value.as_str());
            }
            "METHOD" => {
                prop.check_allowed_params("VCALENDAR", None, &["VALUE"], false)?;
                if method.is_some() {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate METHOD in VCALENDAR".to_owned(),
                    });
                }
                if !prop.value.eq_ignore_ascii_case("PUBLISH") {
                    return Err(IcsError::Unsupported {
                        component: "VCALENDAR".to_owned(),
                        uid: None,
                        item: format!("METHOD:{}", prop.value),
                        reason:
                            "iTIP scheduling methods are not supported; only PUBLISH is allowed"
                                .to_owned(),
                    });
                }
                method = Some(prop.value.as_str());
            }
            name if name.starts_with("X-") => {
                if prop.value.len() > MAX_TEXT_STRING_LEN {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "calendar extension value exceeds maximum length".to_owned(),
                    });
                }
            }
            other => {
                return Err(IcsError::Unsupported {
                    component: "VCALENDAR".to_owned(),
                    uid: None,
                    item: other.to_owned(),
                    reason: format!("unsupported VCALENDAR property `{other}`"),
                });
            }
        }
    }

    if version.is_none() || prodid.is_none() {
        return Err(IcsError::Malformed {
            location: "VCALENDAR".to_owned(),
            reason: "VCALENDAR requires both VERSION:2.0 and PRODID".to_owned(),
        });
    }

    Ok(())
}

fn normalize_timezones(
    timezones: &[ParsedTimezoneComponent],
    budget: &mut BudgetTracker,
) -> Result<Vec<Timezone>, IcsError> {
    let mut normalized = Vec::with_capacity(timezones.len());
    let mut seen: HashMap<String, String> = HashMap::new();

    for tz in timezones {
        budget.step()?;
        if tz.transitions.is_empty() {
            return Err(IcsError::Malformed {
                location: "VTIMEZONE".to_owned(),
                reason: "VTIMEZONE must contain at least one STANDARD or DAYLIGHT subcomponent"
                    .to_owned(),
            });
        }
        let mut tzid: Option<String> = None;
        for prop in &tz.properties {
            match prop.name.as_str() {
                "TZID" => {
                    prop.check_allowed_params("VTIMEZONE", None, &["VALUE"], false)?;
                    if tzid.is_some()
                        || prop.value.trim().is_empty()
                        || prop.value.len() > MAX_SHORT_STRING_LEN
                    {
                        return Err(IcsError::Malformed {
                            location: prop.location(),
                            reason: "invalid or duplicate TZID in VTIMEZONE".to_owned(),
                        });
                    }
                    tzid = Some(prop.value.clone());
                }
                "LAST-MODIFIED" | "TZURL" => {}
                name if name.starts_with("X-") => {}
                other => {
                    return Err(IcsError::Unsupported {
                        component: "VTIMEZONE".to_owned(),
                        uid: tzid.clone(),
                        item: other.to_owned(),
                        reason: format!("unsupported VTIMEZONE property `{other}`"),
                    });
                }
            }
        }
        let Some(tzid) = tzid else {
            return Err(IcsError::Malformed {
                location: "VTIMEZONE".to_owned(),
                reason: "VTIMEZONE is missing TZID".to_owned(),
            });
        };

        for tr in &tz.transitions {
            budget.step()?;
            let mut has_dtstart = false;
            let mut has_offset_from = false;
            let mut has_offset_to = false;
            for prop in &tr.properties {
                match prop.name.as_str() {
                    "DTSTART" => {
                        if has_dtstart || format_local_datetime(&prop.value).is_none() {
                            return Err(IcsError::Malformed {
                                location: prop.location(),
                                reason: format!(
                                    "invalid or duplicate DTSTART in VTIMEZONE {}",
                                    tr.kind
                                ),
                            });
                        }
                        has_dtstart = true;
                    }
                    "TZOFFSETFROM" => {
                        if has_offset_from || !is_valid_utc_offset(&prop.value) {
                            return Err(IcsError::Malformed {
                                location: prop.location(),
                                reason: format!(
                                    "invalid or duplicate TZOFFSETFROM in VTIMEZONE {}",
                                    tr.kind
                                ),
                            });
                        }
                        has_offset_from = true;
                    }
                    "TZOFFSETTO" => {
                        if has_offset_to || !is_valid_utc_offset(&prop.value) {
                            return Err(IcsError::Malformed {
                                location: prop.location(),
                                reason: format!(
                                    "invalid or duplicate TZOFFSETTO in VTIMEZONE {}",
                                    tr.kind
                                ),
                            });
                        }
                        has_offset_to = true;
                    }
                    "TZNAME" | "COMMENT" | "RRULE" | "RDATE" | "EXDATE" => {}
                    name if name.starts_with("X-") => {}
                    other => {
                        return Err(IcsError::Unsupported {
                            component: format!("VTIMEZONE/{}", tr.kind),
                            uid: Some(tzid.clone()),
                            item: other.to_owned(),
                            reason: format!("unsupported VTIMEZONE {} property `{other}`", tr.kind),
                        });
                    }
                }
            }
            if !has_dtstart || !has_offset_from || !has_offset_to {
                return Err(IcsError::Malformed {
                    location: format!("VTIMEZONE/{}", tr.kind),
                    reason: "VTIMEZONE observance requires DTSTART, TZOFFSETFROM and TZOFFSETTO"
                        .to_owned(),
                });
            }
        }

        let raw_vtimezone = tz.raw_lines.join("\r\n");
        if raw_vtimezone.is_empty() || raw_vtimezone.len() > MAX_VTIMEZONE_LEN {
            return Err(IcsError::Malformed {
                location: "VTIMEZONE".to_owned(),
                reason: "VTIMEZONE raw text exceeds contract bounds".to_owned(),
            });
        }

        if let Some(existing) = seen.get(&tzid) {
            if existing != &raw_vtimezone {
                return Err(IcsError::Malformed {
                    location: "VTIMEZONE".to_owned(),
                    reason: format!("conflicting VTIMEZONE definitions for `{tzid}`"),
                });
            }
            continue;
        }
        seen.insert(tzid.clone(), raw_vtimezone.clone());
        normalized.push(Timezone {
            tzid,
            vtimezone: raw_vtimezone,
        });
    }

    Ok(normalized)
}

fn build_retained_diagnostics(retained: &[RetainedBlock]) -> Result<Vec<IcsDiagnostic>, IcsError> {
    let mut diagnostics = Vec::with_capacity(retained.len());
    for block in retained {
        let mut uid: Option<String> = None;
        for prop in &block.properties {
            if prop.name == "UID" {
                if uid.is_some() || prop.value.is_empty() || prop.value.len() > MAX_SHORT_STRING_LEN
                {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "invalid or duplicate UID in retained component".to_owned(),
                    });
                }
                uid = Some(prop.value.clone());
            }
        }
        diagnostics.push(IcsDiagnostic {
            kind: block.kind,
            uid,
            raw: block.raw_lines.join("\r\n"),
        });
    }
    Ok(diagnostics)
}

#[derive(Debug)]
struct ExtractedVEvent {
    uid: String,
    recurrence_id: Option<(Temporal, RecurrenceRange)>,
    dtstart: Temporal,
    dtend: Option<Temporal>,
    duration: Option<String>,
    summary: Option<String>,
    description: Option<String>,
    location: Option<String>,
    url: Option<String>,
    categories: Option<Vec<String>>,
    status: Option<Status>,
    classification: Option<Classification>,
    transparency: Option<Transparency>,
    rrule: Option<String>,
    rdates: Vec<RDate>,
    exdates: Vec<Temporal>,
    organizer: Option<Person>,
    attendees: Option<Vec<Person>>,
    alarms: Option<Vec<Alarm>>,
    attachments: Option<Vec<Attachment>>,
    sequence: Option<u64>,
    last_modified: Option<String>,
    extensions: Option<Vec<Extension>>,
}

fn normalize_events(
    raw_events: &[ParsedEventComponent],
    timezone_defs: &[Timezone],
    budget: &mut BudgetTracker,
) -> Result<Vec<EventV1>, IcsError> {
    let mut master_order: Vec<String> = Vec::new();
    let mut masters: HashMap<String, ExtractedVEvent> = HashMap::new();
    let mut exceptions_by_uid: HashMap<String, Vec<ExtractedVEvent>> = HashMap::new();

    for comp in raw_events {
        budget.step()?;
        let extracted = extract_vevent(comp, budget)?;
        if extracted.recurrence_id.is_some() {
            exceptions_by_uid
                .entry(extracted.uid.clone())
                .or_default()
                .push(extracted);
        } else {
            if masters.contains_key(&extracted.uid) {
                return Err(IcsError::Malformed {
                    location: format!("VEVENT UID:{}", extracted.uid),
                    reason: format!("duplicate master VEVENT for UID `{}`", extracted.uid),
                });
            }
            budget.event()?;
            master_order.push(extracted.uid.clone());
            masters.insert(extracted.uid.clone(), extracted);
        }
    }

    for uid in exceptions_by_uid.keys() {
        if !masters.contains_key(uid) {
            return Err(IcsError::Unsupported {
                component: "VEVENT".to_owned(),
                uid: Some(uid.clone()),
                item: "RECURRENCE-ID".to_owned(),
                reason: "orphan recurrence exception without a master VEVENT in the same file"
                    .to_owned(),
            });
        }
    }

    let mut normalized_events = Vec::with_capacity(master_order.len());
    for uid in master_order {
        budget.step()?;
        let master = masters.remove(&uid).expect("master exists");
        let ex_list = exceptions_by_uid.remove(&uid).unwrap_or_default();

        if !ex_list.is_empty()
            && master.rrule.is_none()
            && master.rdates.is_empty()
            && master.exdates.is_empty()
        {
            return Err(IcsError::Unsupported {
                component: "VEVENT".to_owned(),
                uid: Some(uid),
                item: "RECURRENCE-ID".to_owned(),
                reason: "recurrence exception specified for a non-recurring master VEVENT"
                    .to_owned(),
            });
        }

        let mut max_sequence = master.sequence;
        let mut max_last_modified = master.last_modified.clone();
        let mut exceptions: Vec<RecurrenceException> = Vec::with_capacity(ex_list.len());
        let mut cancelled_probes: Vec<EventV1> = Vec::new();

        for ex in ex_list {
            budget.step()?;
            if let Some(seq) = ex.sequence {
                max_sequence = Some(max_sequence.map_or(seq, |cur| cur.max(seq)));
            }
            if let Some(lm) = &ex.last_modified {
                match &max_last_modified {
                    Some(cur) if cur >= lm => {}
                    _ => max_last_modified = Some(lm.clone()),
                }
            }
            let (recurrence_id, range) = ex.recurrence_id.expect("checked is_some");
            let change = if ex.status == Some(Status::Cancelled) {
                cancelled_probes.push(EventV1 {
                    schema_version: SchemaVersion::V1,
                    external_uid: uid.clone(),
                    title: ex.summary.unwrap_or_default(),
                    description: ex.description,
                    location: ex.location,
                    url: ex.url,
                    categories: ex.categories,
                    status: Status::Cancelled,
                    classification: ex.classification,
                    transparency: ex.transparency,
                    start: ex.dtstart,
                    end: ex.dtend,
                    duration: ex.duration,
                    recurrence: None,
                    organizer: ex.organizer,
                    attendees: ex.attendees,
                    alarms: ex.alarms,
                    attachments: ex.attachments,
                    source_sequence: ex.sequence,
                    source_modified_at: ex.last_modified,
                    timezones: None,
                    extensions: ex.extensions,
                });
                Change::Cancelled
            } else {
                let (end, duration) = match (ex.dtend, ex.duration) {
                    (Some(e), Some(d)) => (Some(Some(e)), Some(Some(d))),
                    (Some(e), None) => (
                        Some(Some(e)),
                        if master.duration.is_some() {
                            Some(None)
                        } else {
                            None
                        },
                    ),
                    (None, Some(d)) => (
                        if master.dtend.is_some() {
                            Some(None)
                        } else {
                            None
                        },
                        Some(Some(d)),
                    ),
                    (None, None) => (None, None),
                };
                Change::Override {
                    patch: Box::new(EventPatch {
                        title: ex.summary,
                        description: ex.description.map(Some),
                        location: ex.location.map(Some),
                        url: ex.url.map(Some),
                        categories: ex.categories,
                        status: ex.status,
                        classification: ex.classification,
                        transparency: ex.transparency,
                        start: Some(ex.dtstart),
                        end,
                        duration,
                        organizer: ex.organizer.map(Some),
                        attendees: ex.attendees,
                        alarms: ex.alarms,
                        attachments: ex.attachments,
                        extensions: ex.extensions,
                    }),
                }
            };
            exceptions.push(RecurrenceException {
                recurrence_id,
                range,
                change,
            });
        }

        let recurrence = if master.rrule.is_some()
            || !master.rdates.is_empty()
            || !master.exdates.is_empty()
            || !exceptions.is_empty()
        {
            Some(Recurrence {
                rrule: master.rrule,
                rdates: (!master.rdates.is_empty()).then_some(master.rdates),
                exdates: (!master.exdates.is_empty()).then_some(master.exdates),
                exceptions: (!exceptions.is_empty()).then_some(exceptions),
            })
        } else {
            None
        };

        let mut event = EventV1 {
            schema_version: SchemaVersion::V1,
            external_uid: master.uid,
            title: master.summary.unwrap_or_default(),
            description: master.description,
            location: master.location,
            url: master.url,
            categories: master.categories,
            status: master.status.unwrap_or(Status::Confirmed),
            classification: master.classification,
            transparency: master.transparency,
            start: master.dtstart,
            end: master.dtend,
            duration: master.duration,
            recurrence,
            organizer: master.organizer,
            attendees: master.attendees,
            alarms: master.alarms,
            attachments: master.attachments,
            source_sequence: max_sequence,
            source_modified_at: max_last_modified,
            timezones: None,
            extensions: master.extensions,
        };

        let referenced_tzids = collect_referenced_tzids(&event);
        if !referenced_tzids.is_empty() && !timezone_defs.is_empty() {
            let matched: Vec<Timezone> = timezone_defs
                .iter()
                .filter(|tz| referenced_tzids.contains(&tz.tzid))
                .cloned()
                .collect();
            if !matched.is_empty() {
                event.timezones = Some(matched);
            }
        }

        if let Err(errors) = validate_event(&event) {
            return Err(IcsError::DomainValidation {
                uid: event.external_uid.clone(),
                event: Box::new(event),
                errors,
            });
        }

        for mut probe in cancelled_probes {
            let probe_tzids = collect_referenced_tzids(&probe);
            if !probe_tzids.is_empty() && !timezone_defs.is_empty() {
                let matched: Vec<Timezone> = timezone_defs
                    .iter()
                    .filter(|tz| probe_tzids.contains(&tz.tzid))
                    .cloned()
                    .collect();
                if !matched.is_empty() {
                    probe.timezones = Some(matched);
                }
            }
            if let Err(errors) = validate_event(&probe) {
                return Err(IcsError::DomainValidation {
                    uid: event.external_uid.clone(),
                    event: Box::new(event),
                    errors,
                });
            }
        }

        normalized_events.push(event);
    }

    Ok(normalized_events)
}

fn collect_referenced_tzids(event: &EventV1) -> HashSet<String> {
    let mut set = HashSet::new();
    let mut add = |t: &Temporal| {
        if let Temporal::Zoned { tzid, .. } = t {
            set.insert(tzid.clone());
        }
    };
    add(&event.start);
    if let Some(end) = &event.end {
        add(end);
    }
    if let Some(rec) = &event.recurrence {
        for rdate in rec.rdates.iter().flatten() {
            match rdate {
                RDate::Temporal(t) => add(t),
                RDate::Period(p) => {
                    add(&p.start);
                    if let Some(end) = &p.end {
                        add(end);
                    }
                }
            }
        }
        for exdate in rec.exdates.iter().flatten() {
            add(exdate);
        }
        for ex in rec.exceptions.iter().flatten() {
            add(&ex.recurrence_id);
            if let Change::Override { patch } = &ex.change {
                if let Some(start) = &patch.start {
                    add(start);
                }
                if let Some(Some(end)) = &patch.end {
                    add(end);
                }
            }
        }
    }
    set
}

fn extract_vevent(
    comp: &ParsedEventComponent,
    budget: &mut BudgetTracker,
) -> Result<ExtractedVEvent, IcsError> {
    let uid_hint = comp
        .properties
        .iter()
        .find(|p| p.name == "UID")
        .map(|p| p.value.clone());

    let mut uid: Option<String> = None;
    let mut dtstamp: Option<String> = None;
    let mut dtstart: Option<Temporal> = None;
    let mut dtend: Option<Temporal> = None;
    let mut duration: Option<String> = None;
    let mut recurrence_id: Option<(Temporal, RecurrenceRange)> = None;
    let mut summary: Option<String> = None;
    let mut description: Option<String> = None;
    let mut location: Option<String> = None;
    let mut url: Option<String> = None;
    let mut categories: Vec<String> = Vec::new();
    let mut status: Option<Status> = None;
    let mut classification: Option<Classification> = None;
    let mut transparency: Option<Transparency> = None;
    let mut rrule: Option<String> = None;
    let mut rdates: Vec<RDate> = Vec::new();
    let mut exdates: Vec<Temporal> = Vec::new();
    let mut organizer: Option<Person> = None;
    let mut attendees: Vec<Person> = Vec::new();
    let mut attachments: Vec<Attachment> = Vec::new();
    let mut sequence: Option<u64> = None;
    let mut last_modified: Option<String> = None;
    let mut extensions: Vec<Extension> = Vec::new();

    for prop in &comp.properties {
        budget.step()?;
        let uid_ref = uid.as_deref().or(uid_hint.as_deref());
        match prop.name.as_str() {
            "UID" => {
                prop.check_allowed_params("VEVENT", uid_ref, &["VALUE"], false)?;
                if let Some(v) = prop.single_param("VALUE")? {
                    if !v.eq_ignore_ascii_case("TEXT") {
                        return Err(IcsError::Malformed {
                            location: prop.location(),
                            reason: "UID VALUE parameter must be TEXT".to_owned(),
                        });
                    }
                }
                if uid.is_some() || prop.value.is_empty() || prop.value.len() > MAX_SHORT_STRING_LEN
                {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "missing, duplicate or oversized UID in VEVENT".to_owned(),
                    });
                }
                uid = Some(prop.value.clone());
            }
            "DTSTAMP" => {
                prop.check_allowed_params("VEVENT", uid_ref, &["VALUE"], false)?;
                if let Some(v) = prop.single_param("VALUE")? {
                    if !v.eq_ignore_ascii_case("DATE-TIME") {
                        return Err(IcsError::Malformed {
                            location: prop.location(),
                            reason: "DTSTAMP VALUE must be DATE-TIME".to_owned(),
                        });
                    }
                }
                let Some(formatted) = format_utc_datetime(&prop.value) else {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "DTSTAMP must be a UTC timestamp (YYYYMMDDTHHMMSSZ)".to_owned(),
                    });
                };
                if dtstamp.is_some()
                    || (Temporal::Utc {
                        value: formatted.clone(),
                    })
                    .to_utc()
                    .is_err()
                {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate or invalid DTSTAMP calendar value".to_owned(),
                    });
                }
                dtstamp = Some(formatted);
            }
            "DTSTART" => {
                if dtstart.is_some() {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate DTSTART in VEVENT".to_owned(),
                    });
                }
                dtstart = Some(parse_temporal_property("VEVENT", uid_ref, prop)?);
            }
            "DTEND" => {
                if dtend.is_some() {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate DTEND in VEVENT".to_owned(),
                    });
                }
                dtend = Some(parse_temporal_property("VEVENT", uid_ref, prop)?);
            }
            "DURATION" => {
                prop.check_allowed_params("VEVENT", uid_ref, &["VALUE"], false)?;
                if let Some(v) = prop.single_param("VALUE")? {
                    if !v.eq_ignore_ascii_case("DURATION") {
                        return Err(IcsError::Malformed {
                            location: prop.location(),
                            reason: "DURATION VALUE parameter must be DURATION".to_owned(),
                        });
                    }
                }
                if duration.is_some()
                    || prop.value.is_empty()
                    || prop.value.len() > MAX_SHORT_STRING_LEN
                {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate or empty DURATION in VEVENT".to_owned(),
                    });
                }
                duration = Some(prop.value.clone());
            }
            "RECURRENCE-ID" => {
                if recurrence_id.is_some() {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate RECURRENCE-ID in VEVENT".to_owned(),
                    });
                }
                prop.check_allowed_params("VEVENT", uid_ref, &["VALUE", "TZID", "RANGE"], false)?;
                let range = match prop.single_param("RANGE")? {
                    None => RecurrenceRange::Single,
                    Some(r) if r.eq_ignore_ascii_case("SINGLE") => RecurrenceRange::Single,
                    Some(r) if r.eq_ignore_ascii_case("THISANDFUTURE") => {
                        RecurrenceRange::ThisAndFuture
                    }
                    Some(other) => {
                        return Err(IcsError::Unsupported {
                            component: "VEVENT".to_owned(),
                            uid: uid_ref.map(str::to_owned),
                            item: format!("RECURRENCE-ID;RANGE={other}"),
                            reason: format!("unsupported RECURRENCE-ID RANGE `{other}`"),
                        });
                    }
                };
                let value_param = prop.single_param("VALUE")?;
                let tzid_param = prop.single_param("TZID")?;
                let temporal = parse_temporal_value(prop, value_param, tzid_param, &prop.value)?;
                recurrence_id = Some((temporal, range));
            }
            "SUMMARY" => {
                if summary.is_some() {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate SUMMARY in VEVENT".to_owned(),
                    });
                }
                summary = Some(parse_text_property("VEVENT", uid_ref, prop)?);
            }
            "DESCRIPTION" => {
                if description.is_some() {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate DESCRIPTION in VEVENT".to_owned(),
                    });
                }
                description = Some(parse_text_property("VEVENT", uid_ref, prop)?);
            }
            "LOCATION" => {
                if location.is_some() {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate LOCATION in VEVENT".to_owned(),
                    });
                }
                location = Some(parse_text_property("VEVENT", uid_ref, prop)?);
            }
            "URL" => {
                prop.check_allowed_params("VEVENT", uid_ref, &["VALUE"], false)?;
                if let Some(v) = prop.single_param("VALUE")? {
                    if !v.eq_ignore_ascii_case("URI") {
                        return Err(IcsError::Malformed {
                            location: prop.location(),
                            reason: "URL VALUE parameter must be URI".to_owned(),
                        });
                    }
                }
                if url.is_some() || !is_valid_uri(&prop.value) {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate or invalid URI in URL property".to_owned(),
                    });
                }
                url = Some(prop.value.clone());
            }
            "CATEGORIES" => {
                prop.check_allowed_params("VEVENT", uid_ref, &["VALUE", "LANGUAGE"], false)?;
                for raw_cat in split_unescaped_commas(&prop.value) {
                    budget.step()?;
                    let decoded = unescape_text(prop, raw_cat)?;
                    if decoded.is_empty() || decoded.len() > MAX_SHORT_STRING_LEN {
                        return Err(IcsError::Malformed {
                            location: prop.location(),
                            reason: "category item must be non-empty and at most 1024 bytes"
                                .to_owned(),
                        });
                    }
                    categories.push(decoded);
                }
            }
            "STATUS" => {
                prop.check_allowed_params("VEVENT", uid_ref, &["VALUE"], false)?;
                if status.is_some() {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate STATUS in VEVENT".to_owned(),
                    });
                }
                status = Some(match prop.value.to_ascii_uppercase().as_str() {
                    "TENTATIVE" => Status::Tentative,
                    "CONFIRMED" => Status::Confirmed,
                    "CANCELLED" => Status::Cancelled,
                    other => {
                        return Err(IcsError::Malformed {
                            location: prop.location(),
                            reason: format!("invalid VEVENT STATUS `{other}`"),
                        });
                    }
                });
            }
            "CLASS" => {
                prop.check_allowed_params("VEVENT", uid_ref, &["VALUE"], false)?;
                if classification.is_some() {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate CLASS in VEVENT".to_owned(),
                    });
                }
                classification = Some(match prop.value.to_ascii_uppercase().as_str() {
                    "PUBLIC" => Classification::Public,
                    "PRIVATE" => Classification::Private,
                    "CONFIDENTIAL" => Classification::Confidential,
                    other => {
                        return Err(IcsError::Unsupported {
                            component: "VEVENT".to_owned(),
                            uid: uid_ref.map(str::to_owned),
                            item: format!("CLASS:{other}"),
                            reason: format!("unsupported VEVENT CLASS `{other}`"),
                        });
                    }
                });
            }
            "TRANSP" => {
                prop.check_allowed_params("VEVENT", uid_ref, &["VALUE"], false)?;
                if transparency.is_some() {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate TRANSP in VEVENT".to_owned(),
                    });
                }
                transparency = Some(match prop.value.to_ascii_uppercase().as_str() {
                    "OPAQUE" => Transparency::Opaque,
                    "TRANSPARENT" => Transparency::Transparent,
                    other => {
                        return Err(IcsError::Malformed {
                            location: prop.location(),
                            reason: format!("invalid VEVENT TRANSP `{other}`"),
                        });
                    }
                });
            }
            "RRULE" => {
                prop.check_allowed_params("VEVENT", uid_ref, &["VALUE"], false)?;
                if rrule.is_some() {
                    return Err(IcsError::Unsupported {
                        component: "VEVENT".to_owned(),
                        uid: uid_ref.map(str::to_owned),
                        item: "RRULE".to_owned(),
                        reason: "multiple RRULE properties on a single VEVENT are not supported"
                            .to_owned(),
                    });
                }
                if prop.value.is_empty() || prop.value.len() > MAX_RRULE_LEN {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "empty or oversized RRULE in VEVENT".to_owned(),
                    });
                }
                rrule = Some(prop.value.clone());
            }
            "EXDATE" => {
                prop.check_allowed_params("VEVENT", uid_ref, &["VALUE", "TZID"], false)?;
                let value_param = prop.single_param("VALUE")?;
                let tzid_param = prop.single_param("TZID")?;
                if prop.value.is_empty() {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "empty EXDATE value".to_owned(),
                    });
                }
                for item in prop.value.split(',') {
                    budget.step()?;
                    exdates.push(parse_temporal_value(prop, value_param, tzid_param, item)?);
                }
            }
            "RDATE" => {
                prop.check_allowed_params("VEVENT", uid_ref, &["VALUE", "TZID"], false)?;
                let value_param = prop.single_param("VALUE")?;
                let tzid_param = prop.single_param("TZID")?;
                if prop.value.is_empty() {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "empty RDATE value".to_owned(),
                    });
                }
                for item in prop.value.split(',') {
                    budget.step()?;
                    if value_param.is_some_and(|v| v.eq_ignore_ascii_case("PERIOD")) {
                        let period = parse_period_value(prop, tzid_param, item)?;
                        rdates.push(RDate::Period(period));
                    } else {
                        let temporal = parse_temporal_value(prop, value_param, tzid_param, item)?;
                        rdates.push(RDate::Temporal(temporal));
                    }
                }
            }
            "ORGANIZER" => {
                if organizer.is_some() {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate ORGANIZER in VEVENT".to_owned(),
                    });
                }
                organizer = Some(parse_person("VEVENT", uid_ref, prop, true)?);
            }
            "ATTENDEE" => {
                attendees.push(parse_person("VEVENT", uid_ref, prop, false)?);
            }
            "ATTACH" => {
                attachments.push(parse_attachment("VEVENT", uid_ref, prop)?);
            }
            "SEQUENCE" => {
                prop.check_allowed_params("VEVENT", uid_ref, &["VALUE"], false)?;
                if sequence.is_some()
                    || prop.value.is_empty()
                    || !prop.value.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate or invalid non-negative integer SEQUENCE".to_owned(),
                    });
                }
                let parsed: u64 = prop.value.parse().map_err(|_| IcsError::Malformed {
                    location: prop.location(),
                    reason: "SEQUENCE integer overflow".to_owned(),
                })?;
                sequence = Some(parsed);
            }
            "LAST-MODIFIED" => {
                prop.check_allowed_params("VEVENT", uid_ref, &["VALUE"], false)?;
                if last_modified.is_some() {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate LAST-MODIFIED in VEVENT".to_owned(),
                    });
                }
                let Some(formatted) = format_utc_datetime(&prop.value) else {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "LAST-MODIFIED must be a UTC timestamp (YYYYMMDDTHHMMSSZ)"
                            .to_owned(),
                    });
                };
                if (Temporal::Utc {
                    value: formatted.clone(),
                })
                .to_utc()
                .is_err()
                {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "invalid calendar date in LAST-MODIFIED".to_owned(),
                    });
                }
                last_modified = Some(formatted);
            }
            name if name.starts_with("X-") => {
                extensions.push(parse_extension(prop)?);
            }
            other => {
                return Err(IcsError::Unsupported {
                    component: "VEVENT".to_owned(),
                    uid: uid_ref.map(str::to_owned),
                    item: other.to_owned(),
                    reason: format!("unsupported VEVENT property `{other}`"),
                });
            }
        }
    }

    let Some(uid) = uid else {
        return Err(IcsError::Malformed {
            location: "VEVENT".to_owned(),
            reason: "VEVENT is missing required UID".to_owned(),
        });
    };
    if dtstamp.is_none() {
        return Err(IcsError::Malformed {
            location: format!("VEVENT UID:{uid}"),
            reason: "VEVENT is missing required DTSTAMP".to_owned(),
        });
    }
    let Some(dtstart) = dtstart else {
        return Err(IcsError::Malformed {
            location: format!("VEVENT UID:{uid}"),
            reason: "VEVENT is missing required DTSTART".to_owned(),
        });
    };

    if recurrence_id.is_some() && (rrule.is_some() || !rdates.is_empty() || !exdates.is_empty()) {
        return Err(IcsError::Unsupported {
            component: "VEVENT".to_owned(),
            uid: Some(uid),
            item: "RRULE/RDATE/EXDATE".to_owned(),
            reason: "recurrence rules or date lists on a RECURRENCE-ID exception VEVENT are not supported"
                .to_owned(),
        });
    }

    let mut alarms: Vec<Alarm> = Vec::with_capacity(comp.alarms.len());
    for (idx, raw_alarm) in comp.alarms.iter().enumerate() {
        budget.step()?;
        alarms.push(parse_valarm(&uid, idx + 1, raw_alarm, budget)?);
    }

    Ok(ExtractedVEvent {
        uid,
        recurrence_id,
        dtstart,
        dtend,
        duration,
        summary,
        description,
        location,
        url,
        categories: (!categories.is_empty()).then_some(categories),
        status,
        classification,
        transparency,
        rrule,
        rdates,
        exdates,
        organizer,
        attendees: (!attendees.is_empty()).then_some(attendees),
        alarms: (!alarms.is_empty()).then_some(alarms),
        attachments: (!attachments.is_empty()).then_some(attachments),
        sequence,
        last_modified,
        extensions: (!extensions.is_empty()).then_some(extensions),
    })
}

fn parse_valarm(
    uid: &str,
    index: usize,
    raw: &ParsedAlarm,
    budget: &mut BudgetTracker,
) -> Result<Alarm, IcsError> {
    let mut action: Option<AlarmAction> = None;
    let mut trigger: Option<AlarmTrigger> = None;
    let mut repeat: Option<u32> = None;
    let mut repeat_interval: Option<String> = None;
    let mut description: Option<String> = None;
    let mut summary: Option<String> = None;
    let mut attendees: Vec<Person> = Vec::new();
    let mut attachments: Vec<Attachment> = Vec::new();
    let mut extensions: Vec<Extension> = Vec::new();

    for prop in &raw.properties {
        budget.step()?;
        match prop.name.as_str() {
            "ACTION" => {
                prop.check_allowed_params("VALARM", Some(uid), &["VALUE"], false)?;
                if action.is_some() {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate ACTION in VALARM".to_owned(),
                    });
                }
                action = Some(match prop.value.to_ascii_uppercase().as_str() {
                    "DISPLAY" => AlarmAction::Display,
                    "AUDIO" => AlarmAction::Audio,
                    "EMAIL" => AlarmAction::Email,
                    other => {
                        return Err(IcsError::Unsupported {
                            component: "VALARM".to_owned(),
                            uid: Some(uid.to_owned()),
                            item: format!("ACTION:{other}"),
                            reason: format!("unsupported VALARM ACTION `{other}`"),
                        });
                    }
                });
            }
            "TRIGGER" => {
                if trigger.is_some() {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate TRIGGER in VALARM".to_owned(),
                    });
                }
                prop.check_allowed_params("VALARM", Some(uid), &["VALUE", "RELATED"], false)?;
                let value_param = prop.single_param("VALUE")?;
                let related_param = prop.single_param("RELATED")?;
                if value_param.is_some_and(|v| v.eq_ignore_ascii_case("DATE-TIME")) {
                    if related_param.is_some() {
                        return Err(IcsError::Malformed {
                            location: prop.location(),
                            reason:
                                "RELATED parameter is not allowed on absolute DATE-TIME TRIGGER"
                                    .to_owned(),
                        });
                    }
                    let Some(formatted) = format_utc_datetime(&prop.value) else {
                        return Err(IcsError::Malformed {
                            location: prop.location(),
                            reason: "absolute TRIGGER must be a UTC timestamp (YYYYMMDDTHHMMSSZ)"
                                .to_owned(),
                        });
                    };
                    trigger = Some(AlarmTrigger::Absolute {
                        at: Temporal::Utc { value: formatted },
                    });
                } else {
                    if let Some(v) = value_param {
                        if !v.eq_ignore_ascii_case("DURATION") {
                            return Err(IcsError::Unsupported {
                                component: "VALARM".to_owned(),
                                uid: Some(uid.to_owned()),
                                item: format!("TRIGGER;VALUE={v}"),
                                reason: format!("unsupported TRIGGER VALUE `{v}`"),
                            });
                        }
                    }
                    let related = match related_param {
                        None => Related::Start,
                        Some(r) if r.eq_ignore_ascii_case("START") => Related::Start,
                        Some(r) if r.eq_ignore_ascii_case("END") => Related::End,
                        Some(other) => {
                            return Err(IcsError::Malformed {
                                location: prop.location(),
                                reason: format!("invalid TRIGGER RELATED `{other}`"),
                            });
                        }
                    };
                    if prop.value.is_empty() || prop.value.len() > MAX_SHORT_STRING_LEN {
                        return Err(IcsError::Malformed {
                            location: prop.location(),
                            reason: "empty or oversized relative TRIGGER offset".to_owned(),
                        });
                    }
                    trigger = Some(AlarmTrigger::Relative {
                        related,
                        offset: prop.value.clone(),
                    });
                }
            }
            "REPEAT" => {
                prop.check_allowed_params("VALARM", Some(uid), &["VALUE"], false)?;
                if repeat.is_some()
                    || prop.value.is_empty()
                    || !prop.value.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate or invalid REPEAT in VALARM".to_owned(),
                    });
                }
                let parsed: u32 = prop.value.parse().map_err(|_| IcsError::Malformed {
                    location: prop.location(),
                    reason: "REPEAT integer overflow".to_owned(),
                })?;
                if parsed == 0 {
                    return Err(IcsError::ContractViolation {
                        uid: uid.to_owned(),
                        reason: "VALARM repeat count must be >= 1 in EventV1".to_owned(),
                    });
                }
                repeat = Some(parsed);
            }
            "DURATION" => {
                prop.check_allowed_params("VALARM", Some(uid), &["VALUE"], false)?;
                if repeat_interval.is_some()
                    || prop.value.is_empty()
                    || prop.value.len() > MAX_SHORT_STRING_LEN
                {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate or empty DURATION in VALARM".to_owned(),
                    });
                }
                repeat_interval = Some(prop.value.clone());
            }
            "DESCRIPTION" => {
                if description.is_some() {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate DESCRIPTION in VALARM".to_owned(),
                    });
                }
                description = Some(parse_text_property("VALARM", Some(uid), prop)?);
            }
            "SUMMARY" => {
                if summary.is_some() {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "duplicate SUMMARY in VALARM".to_owned(),
                    });
                }
                summary = Some(parse_text_property("VALARM", Some(uid), prop)?);
            }
            "ATTENDEE" => {
                attendees.push(parse_person("VALARM", Some(uid), prop, false)?);
            }
            "ATTACH" => {
                attachments.push(parse_attachment("VALARM", Some(uid), prop)?);
            }
            name if name.starts_with("X-") => {
                extensions.push(parse_extension(prop)?);
            }
            other => {
                return Err(IcsError::Unsupported {
                    component: "VALARM".to_owned(),
                    uid: Some(uid.to_owned()),
                    item: other.to_owned(),
                    reason: format!("unsupported VALARM property `{other}`"),
                });
            }
        }
    }

    let Some(action) = action else {
        return Err(IcsError::Malformed {
            location: format!("VALARM in UID:{uid}"),
            reason: "VALARM is missing ACTION".to_owned(),
        });
    };
    let Some(trigger) = trigger else {
        return Err(IcsError::Malformed {
            location: format!("VALARM in UID:{uid}"),
            reason: "VALARM is missing TRIGGER".to_owned(),
        });
    };
    if repeat.is_some() != repeat_interval.is_some() {
        return Err(IcsError::ContractViolation {
            uid: uid.to_owned(),
            reason: "VALARM REPEAT and DURATION must be specified together".to_owned(),
        });
    }

    match action {
        AlarmAction::Display => {
            if description.is_none() {
                return Err(IcsError::ContractViolation {
                    uid: uid.to_owned(),
                    reason: "DISPLAY VALARM requires DESCRIPTION".to_owned(),
                });
            }
            if summary.is_some() || !attendees.is_empty() || !attachments.is_empty() {
                return Err(IcsError::ContractViolation {
                    uid: uid.to_owned(),
                    reason: "DISPLAY VALARM must not include SUMMARY, ATTENDEE or ATTACH"
                        .to_owned(),
                });
            }
        }
        AlarmAction::Email => {
            if description.is_none() || summary.is_none() || attendees.is_empty() {
                return Err(IcsError::ContractViolation {
                    uid: uid.to_owned(),
                    reason: "EMAIL VALARM requires DESCRIPTION, SUMMARY and at least one ATTENDEE"
                        .to_owned(),
                });
            }
        }
        AlarmAction::Audio => {
            if description.is_some()
                || summary.is_some()
                || !attendees.is_empty()
                || attachments.len() > 1
            {
                return Err(IcsError::ContractViolation {
                    uid: uid.to_owned(),
                    reason: "AUDIO VALARM allows at most one ATTACH and no DESCRIPTION, SUMMARY or ATTENDEE"
                        .to_owned(),
                });
            }
        }
    }

    Ok(Alarm {
        id: format!("alarm-{index}"),
        action,
        trigger,
        repeat,
        repeat_interval,
        description,
        summary,
        attendees: (!attendees.is_empty()).then_some(attendees),
        attachments: (!attachments.is_empty()).then_some(attachments),
        extensions: (!extensions.is_empty()).then_some(extensions),
    })
}

fn parse_temporal_property(
    component: &str,
    uid: Option<&str>,
    prop: &ParsedProperty,
) -> Result<Temporal, IcsError> {
    prop.check_allowed_params(component, uid, &["VALUE", "TZID"], false)?;
    let value_param = prop.single_param("VALUE")?;
    let tzid_param = prop.single_param("TZID")?;
    parse_temporal_value(prop, value_param, tzid_param, &prop.value)
}

fn parse_temporal_value(
    prop: &ParsedProperty,
    value_param: Option<&str>,
    tzid_param: Option<&str>,
    raw: &str,
) -> Result<Temporal, IcsError> {
    if let Some(tzid) = tzid_param {
        if tzid.is_empty() || tzid.len() > MAX_SHORT_STRING_LEN {
            return Err(IcsError::Malformed {
                location: prop.location(),
                reason: "empty or oversized TZID parameter".to_owned(),
            });
        }
    }

    match value_param {
        Some(v) if v.eq_ignore_ascii_case("DATE") => {
            if tzid_param.is_some() {
                return Err(IcsError::Malformed {
                    location: prop.location(),
                    reason: "TZID parameter must not be specified on VALUE=DATE".to_owned(),
                });
            }
            let Some(date) = format_date(raw) else {
                return Err(IcsError::Malformed {
                    location: prop.location(),
                    reason: format!("invalid DATE literal `{raw}`"),
                });
            };
            Ok(Temporal::Date { date })
        }
        None => {
            if raw.ends_with('Z') {
                if tzid_param.is_some() {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "TZID parameter must not be specified on UTC time ending with Z"
                            .to_owned(),
                    });
                }
                let Some(value) = format_utc_datetime(raw) else {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: format!("invalid UTC DATE-TIME literal `{raw}`"),
                    });
                };
                Ok(Temporal::Utc { value })
            } else {
                let Some(local) = format_local_datetime(raw) else {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: format!("invalid local DATE-TIME literal `{raw}`"),
                    });
                };
                if let Some(tzid) = tzid_param {
                    Ok(Temporal::Zoned {
                        local,
                        tzid: tzid.to_owned(),
                    })
                } else {
                    Ok(Temporal::Floating { local })
                }
            }
        }
        Some(v) if v.eq_ignore_ascii_case("DATE-TIME") => {
            parse_temporal_value(prop, None, tzid_param, raw)
        }
        Some(other) => Err(IcsError::Unsupported {
            component: "VEVENT".to_owned(),
            uid: None,
            item: format!("{};VALUE={other}", prop.name),
            reason: format!("unsupported VALUE type `{other}` on `{}`", prop.name),
        }),
    }
}

fn parse_period_value(
    prop: &ParsedProperty,
    tzid_param: Option<&str>,
    raw: &str,
) -> Result<Period, IcsError> {
    let Some((start_raw, rest)) = raw.split_once('/') else {
        return Err(IcsError::Malformed {
            location: prop.location(),
            reason: format!("RDATE PERIOD `{raw}` is missing `/` separator"),
        });
    };
    if rest.is_empty() || rest.contains('/') {
        return Err(IcsError::Malformed {
            location: prop.location(),
            reason: format!("invalid RDATE PERIOD `{raw}`"),
        });
    }
    let start = parse_temporal_value(prop, Some("DATE-TIME"), tzid_param, start_raw)?;
    if rest.starts_with('P') || rest.starts_with('+') || rest.starts_with('-') {
        Ok(Period {
            kind: PeriodKind::Period,
            start,
            end: None,
            duration: Some(rest.to_owned()),
        })
    } else {
        let end = parse_temporal_value(prop, Some("DATE-TIME"), tzid_param, rest)?;
        Ok(Period {
            kind: PeriodKind::Period,
            start,
            end: Some(end),
            duration: None,
        })
    }
}

fn parse_text_property(
    component: &str,
    uid: Option<&str>,
    prop: &ParsedProperty,
) -> Result<String, IcsError> {
    prop.check_allowed_params(component, uid, &["VALUE", "ALTREP", "LANGUAGE"], false)?;
    if let Some(v) = prop.single_param("VALUE")? {
        if !v.eq_ignore_ascii_case("TEXT") {
            return Err(IcsError::Unsupported {
                component: component.to_owned(),
                uid: uid.map(str::to_owned),
                item: format!("{};VALUE={v}", prop.name),
                reason: format!("unsupported VALUE `{v}` on TEXT property `{}`", prop.name),
            });
        }
    }
    let decoded = unescape_text(prop, &prop.value)?;
    if decoded.len() > MAX_TEXT_STRING_LEN {
        return Err(IcsError::ContractViolation {
            uid: uid.unwrap_or_default().to_owned(),
            reason: format!("`{}` exceeds 65536 character contract limit", prop.name),
        });
    }
    Ok(decoded)
}

fn unescape_text(prop: &ParsedProperty, raw: &str) -> Result<String, IcsError> {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('n') | Some('N') => out.push('\n'),
                Some(',') => out.push(','),
                Some(';') => out.push(';'),
                Some(other) => {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: format!("invalid RFC 5545 TEXT escape `\\{other}`"),
                    });
                }
                None => {
                    return Err(IcsError::Malformed {
                        location: prop.location(),
                        reason: "dangling trailing backslash in TEXT value".to_owned(),
                    });
                }
            }
        } else {
            out.push(ch);
        }
    }
    Ok(out)
}

fn split_unescaped_commas(raw: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let bytes = raw.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i = (i + 2).min(bytes.len());
        } else if bytes[i] == b',' {
            parts.push(&raw[start..i]);
            i += 1;
            start = i;
        } else {
            i += 1;
        }
    }
    parts.push(&raw[start..]);
    parts
}

fn parse_person(
    component: &str,
    uid: Option<&str>,
    prop: &ParsedProperty,
    is_organizer: bool,
) -> Result<Person, IcsError> {
    if !is_valid_uri(&prop.value) {
        return Err(IcsError::Malformed {
            location: prop.location(),
            reason: format!("invalid URI address in `{}`", prop.name),
        });
    }

    let allowed: &[&str] = if is_organizer {
        &["CN", "LANGUAGE", "VALUE"]
    } else {
        &["CN", "ROLE", "PARTSTAT", "RSVP", "LANGUAGE", "VALUE"]
    };
    prop.check_allowed_params(component, uid, allowed, true)?;
    if let Some(v) = prop.single_param("VALUE")? {
        if !v.eq_ignore_ascii_case("CAL-ADDRESS") {
            return Err(IcsError::Unsupported {
                component: component.to_owned(),
                uid: uid.map(str::to_owned),
                item: format!("{};VALUE={v}", prop.name),
                reason: "participant VALUE must be CAL-ADDRESS".to_owned(),
            });
        }
    }

    let display_name = match prop.single_param("CN")? {
        Some(cn) => {
            if cn.len() > MAX_TEXT_STRING_LEN {
                return Err(IcsError::ContractViolation {
                    uid: uid.unwrap_or_default().to_owned(),
                    reason: "CN parameter exceeds maximum length".to_owned(),
                });
            }
            Some(cn.to_owned())
        }
        None => None,
    };

    let role = match prop.single_param("ROLE")? {
        None => None,
        Some(r) if r.eq_ignore_ascii_case("CHAIR") => Some(Role::Chair),
        Some(r) if r.eq_ignore_ascii_case("REQ-PARTICIPANT") => Some(Role::Required),
        Some(r) if r.eq_ignore_ascii_case("OPT-PARTICIPANT") => Some(Role::Optional),
        Some(r) if r.eq_ignore_ascii_case("NON-PARTICIPANT") => Some(Role::NonParticipant),
        Some(other) => {
            return Err(IcsError::Unsupported {
                component: component.to_owned(),
                uid: uid.map(str::to_owned),
                item: format!("ROLE={other}"),
                reason: format!("unsupported participant ROLE `{other}`"),
            });
        }
    };

    let participation = match prop.single_param("PARTSTAT")? {
        None => None,
        Some(p) if p.eq_ignore_ascii_case("NEEDS-ACTION") => Some(Participation::NeedsAction),
        Some(p) if p.eq_ignore_ascii_case("ACCEPTED") => Some(Participation::Accepted),
        Some(p) if p.eq_ignore_ascii_case("DECLINED") => Some(Participation::Declined),
        Some(p) if p.eq_ignore_ascii_case("TENTATIVE") => Some(Participation::Tentative),
        Some(p) if p.eq_ignore_ascii_case("DELEGATED") => Some(Participation::Delegated),
        Some(other) => {
            return Err(IcsError::Unsupported {
                component: component.to_owned(),
                uid: uid.map(str::to_owned),
                item: format!("PARTSTAT={other}"),
                reason: format!("unsupported VEVENT PARTSTAT `{other}`"),
            });
        }
    };

    let rsvp = match prop.single_param("RSVP")? {
        None => None,
        Some(b) if b.eq_ignore_ascii_case("TRUE") => Some(true),
        Some(b) if b.eq_ignore_ascii_case("FALSE") => Some(false),
        Some(other) => {
            return Err(IcsError::Malformed {
                location: prop.location(),
                reason: format!("invalid RSVP boolean `{other}`"),
            });
        }
    };

    let mut extensions: Vec<Extension> = Vec::new();
    for (k, vals) in &prop.params {
        if k.starts_with("X-") {
            for val in vals {
                extensions.push(Extension {
                    name: k.clone(),
                    parameters: BTreeMap::new(),
                    value: val.clone(),
                });
            }
        }
    }

    Ok(Person {
        address: prop.value.clone(),
        display_name,
        role,
        participation,
        rsvp,
        extensions: (!extensions.is_empty()).then_some(extensions),
    })
}

fn parse_attachment(
    component: &str,
    uid: Option<&str>,
    prop: &ParsedProperty,
) -> Result<Attachment, IcsError> {
    for (k, vals) in &prop.params {
        if k == "ENCODING"
            || (k == "VALUE" && vals.iter().any(|v| v.eq_ignore_ascii_case("BINARY")))
        {
            return Err(IcsError::Unsupported {
                component: component.to_owned(),
                uid: uid.map(str::to_owned),
                item: "ATTACH;VALUE=BINARY".to_owned(),
                reason:
                    "inline binary attachments are not supported; only URI attachments are allowed"
                        .to_owned(),
            });
        }
    }
    prop.check_allowed_params(component, uid, &["FMTTYPE", "VALUE"], false)?;
    if let Some(v) = prop.single_param("VALUE")? {
        if !v.eq_ignore_ascii_case("URI") {
            return Err(IcsError::Unsupported {
                component: component.to_owned(),
                uid: uid.map(str::to_owned),
                item: format!("ATTACH;VALUE={v}"),
                reason: format!("unsupported ATTACH VALUE `{v}`"),
            });
        }
    }
    let media_type = match prop.single_param("FMTTYPE")? {
        Some(mt) => {
            if mt.is_empty() || mt.len() > MAX_SHORT_STRING_LEN {
                return Err(IcsError::Malformed {
                    location: prop.location(),
                    reason: "invalid FMTTYPE parameter on ATTACH".to_owned(),
                });
            }
            Some(mt.to_owned())
        }
        None => None,
    };
    if !is_valid_uri(&prop.value) {
        return Err(IcsError::Malformed {
            location: prop.location(),
            reason: "invalid URI in ATTACH property".to_owned(),
        });
    }
    Ok(Attachment {
        uri: prop.value.clone(),
        media_type,
    })
}

fn parse_extension(prop: &ParsedProperty) -> Result<Extension, IcsError> {
    if prop.name.is_empty()
        || prop.name.len() > MAX_SHORT_STRING_LEN
        || prop.value.len() > MAX_TEXT_STRING_LEN
    {
        return Err(IcsError::Malformed {
            location: prop.location(),
            reason: "extension property name or value exceeds contract bounds".to_owned(),
        });
    }
    let mut parameters: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (k, vals) in &prop.params {
        parameters
            .entry(k.clone())
            .or_default()
            .extend(vals.clone());
    }
    Ok(Extension {
        name: prop.name.clone(),
        parameters,
        value: prop.value.clone(),
    })
}

fn format_date(raw: &str) -> Option<String> {
    let b = raw.as_bytes();
    if b.len() != 8 || !b.iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!("{}-{}-{}", &raw[0..4], &raw[4..6], &raw[6..8]))
}

fn format_local_datetime(raw: &str) -> Option<String> {
    let b = raw.as_bytes();
    if b.len() != 15 || b[8] != b'T' {
        return None;
    }
    if !b[0..8].iter().all(|c| c.is_ascii_digit()) || !b[9..15].iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!(
        "{}-{}-{}T{}:{}:{}",
        &raw[0..4],
        &raw[4..6],
        &raw[6..8],
        &raw[9..11],
        &raw[11..13],
        &raw[13..15]
    ))
}

fn format_utc_datetime(raw: &str) -> Option<String> {
    let without_z = raw.strip_suffix('Z')?;
    let local = format_local_datetime(without_z)?;
    Some(format!("{local}Z"))
}

fn is_valid_utc_offset(raw: &str) -> bool {
    let b = raw.as_bytes();
    if !(b.len() == 5 || b.len() == 7) || (b[0] != b'+' && b[0] != b'-') {
        return false;
    }
    if !b[1..].iter().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let hh: u8 = raw[1..3].parse().unwrap_or(99);
    let mm: u8 = raw[3..5].parse().unwrap_or(99);
    let ss: u8 = if b.len() == 7 {
        raw[5..7].parse().unwrap_or(99)
    } else {
        0
    };
    hh <= 23 && mm <= 59 && ss <= 59
}

fn is_valid_uri(raw: &str) -> bool {
    if raw.is_empty() || raw.len() > MAX_TEXT_STRING_LEN || raw.chars().any(char::is_whitespace) {
        return false;
    }
    let Some((scheme, rest)) = raw.split_once(':') else {
        return false;
    };
    if scheme.is_empty() || rest.is_empty() {
        return false;
    }
    let mut chars = scheme.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}
