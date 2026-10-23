//! Issue #25: V-Model acceptance and integration tests for bounded ICS file normalization.

use std::collections::BTreeMap;

use calendar_adapters::{
    normalize_ics, normalize_ics_to_snapshot_batch, IcsBudgetKind, IcsDiagnostic, IcsError,
    IcsLimits, IcsSnapshotInput, RetainedComponentKind,
};
use calendar_application::{DeletionPolicy, SyncMode, SyncRecord};
use calendar_domain::{
    expand_occurrences, validate_event, Alarm, AlarmAction, AlarmTrigger, Attachment, Change,
    Classification, Code, EventPatch, EventV1, ExpansionOptions, Extension, Participation, Period,
    PeriodKind, Person, RDate, Recurrence, RecurrenceException, RecurrenceRange, Related, Role,
    SchemaVersion, Status, Temporal, Timezone, Transparency,
};

const TIME_TEXT: &str = include_str!("../../calendar-domain/tests/fixtures/ics/time-and-text.ics");
const RECURRENCE: &str =
    include_str!("../../calendar-domain/tests/fixtures/ics/recurrence-exceptions.ics");
const TIMEZONE_RANGE: &str =
    include_str!("../../calendar-domain/tests/fixtures/ics/timezone-and-range.ics");

#[test]
fn ac1_golden_time_text_and_supported_properties_normalize_to_event_v1() {
    let result = normalize_ics(TIME_TEXT, &IcsLimits::default()).expect("time-and-text normalizes");
    assert!(result.diagnostics.is_empty());
    assert_eq!(result.events.len(), 4);

    for event in &result.events {
        validate_event(event).expect("every normalized event passes domain validation");
        let value = serde_json::to_value(event).expect("serializes to JSON");
        let round_trip: EventV1 =
            serde_json::from_value(value).expect("deserializes with strict schema shape");
        assert_eq!(&round_trip, event);
    }

    assert_eq!(
        result.events[0],
        EventV1 {
            schema_version: SchemaVersion::V1,
            external_uid: "date@example.test".to_owned(),
            title: "全天会议 🌏".to_owned(),
            description: None,
            location: None,
            url: None,
            categories: None,
            status: Status::Confirmed,
            classification: None,
            transparency: None,
            start: Temporal::Date {
                date: "2026-11-02".to_owned(),
            },
            end: Some(Temporal::Date {
                date: "2026-11-03".to_owned(),
            }),
            duration: None,
            recurrence: None,
            organizer: None,
            attendees: None,
            alarms: None,
            attachments: None,
            source_sequence: None,
            source_modified_at: None,
            timezones: None,
            extensions: None,
        }
    );

    let mut custom_first_params = BTreeMap::new();
    custom_first_params.insert(
        "X-LABEL".to_owned(),
        vec!["alpha,beta".to_owned(), "gamma".to_owned()],
    );
    let mut custom_second_params = BTreeMap::new();
    custom_second_params.insert("X-LABEL".to_owned(), vec!["delta".to_owned()]);

    assert_eq!(
        result.events[1],
        EventV1 {
            schema_version: SchemaVersion::V1,
            external_uid: "utc@example.test".to_owned(),
            title: "Unicode café 东京".to_owned(),
            description: Some("One, two; three\\four\n第二行".to_owned()),
            location: None,
            url: None,
            categories: Some(vec!["work".to_owned(), "家庭".to_owned()]),
            status: Status::Confirmed,
            classification: None,
            transparency: None,
            start: Temporal::Utc {
                value: "2026-11-02T17:00:00Z".to_owned(),
            },
            end: Some(Temporal::Utc {
                value: "2026-11-02T18:00:00Z".to_owned(),
            }),
            duration: None,
            recurrence: None,
            organizer: None,
            attendees: Some(vec![
                Person {
                    address: "mailto:jane@example.test".to_owned(),
                    display_name: Some("Doe, Jane".to_owned()),
                    role: Some(Role::Required),
                    participation: None,
                    rsvp: None,
                    extensions: None,
                },
                Person {
                    address: "mailto:li@example.test".to_owned(),
                    display_name: Some("李雷".to_owned()),
                    role: Some(Role::Optional),
                    participation: None,
                    rsvp: None,
                    extensions: None,
                },
            ]),
            alarms: None,
            attachments: None,
            source_sequence: None,
            source_modified_at: None,
            timezones: None,
            extensions: Some(vec![
                Extension {
                    name: "X-LONG".to_owned(),
                    parameters: BTreeMap::new(),
                    value: concat!(
                        "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefgh",
                        "ijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmno",
                        "pqrstuvwxyz-END"
                    )
                    .to_owned(),
                },
                Extension {
                    name: "X-CUSTOM".to_owned(),
                    parameters: custom_first_params,
                    value: "first".to_owned(),
                },
                Extension {
                    name: "X-CUSTOM".to_owned(),
                    parameters: custom_second_params,
                    value: "second".to_owned(),
                },
            ]),
        }
    );

    assert_eq!(
        result.events[2].description.as_deref(),
        Some(r"Literal \n token"),
        "escaped backslash followed by n must decode to literal backslash+n, not newline"
    );
    assert_eq!(
        result.events[2].start,
        Temporal::Floating {
            local: "2026-11-02T09:00:00".to_owned(),
        }
    );
    assert_eq!(result.events[2].duration.as_deref(), Some("PT1H"));

    assert_eq!(
        result.events[3].start,
        Temporal::Zoned {
            local: "2026-11-02T09:00:00".to_owned(),
            tzid: "America/Vancouver".to_owned(),
        }
    );
    assert_eq!(result.events[3].duration.as_deref(), Some("PT1H"));

    // Verify rich supported properties: STATUS, CLASS, TRANSP, LOCATION, URL, ORGANIZER,
    // ATTENDEE (PARTSTAT/RSVP/X-*), ATTACH, VALARM (DISPLAY/AUDIO/EMAIL + absolute/relative),
    // SEQUENCE, LAST-MODIFIED, and comma-separated CATEGORIES with escaped commas.
    let rich_ics = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//CalRelay//Issue 25 Rich//EN\r\n\
CALSCALE:GREGORIAN\r\n\
METHOD:PUBLISH\r\n\
BEGIN:VEVENT\r\n\
UID:rich@example.test\r\n\
DTSTAMP:20261022T120000Z\r\n\
LAST-MODIFIED:20261022T153000Z\r\n\
SEQUENCE:7\r\n\
SUMMARY:架构评审\\, 阶段一\r\n\
DESCRIPTION:详情\\N第二行\r\n\
LOCATION:Room 42\\; West\r\n\
URL:https://calrelay.example.test/events/42\r\n\
STATUS:TENTATIVE\r\n\
CLASS:CONFIDENTIAL\r\n\
TRANSP:TRANSPARENT\r\n\
CATEGORIES:ops\\,sec,backend\r\n\
DTSTART:20261102T170000Z\r\n\
DURATION:PT45M\r\n\
ORGANIZER;CN=\"Alice, Lead\";X-ORG-UNIT=core:mailto:alice@example.test\r\n\
ATTENDEE;CN=Bob;ROLE=CHAIR;PARTSTAT=ACCEPTED;RSVP=TRUE;X-TEAM=infra:mailto:bob@example.test\r\n\
ATTACH;FMTTYPE=application/pdf:https://calrelay.example.test/spec.pdf\r\n\
BEGIN:VALARM\r\n\
ACTION:DISPLAY\r\n\
TRIGGER;RELATED=END:-PT10M\r\n\
REPEAT:2\r\n\
DURATION:PT5M\r\n\
DESCRIPTION:即将结束\r\n\
X-ALARM-TAG:primary\r\n\
END:VALARM\r\n\
BEGIN:VALARM\r\n\
ACTION:EMAIL\r\n\
TRIGGER;VALUE=DATE-TIME:20261102T165000Z\r\n\
SUMMARY:邮件提醒\r\n\
DESCRIPTION:请准时参会\r\n\
ATTENDEE:mailto:bob@example.test\r\n\
ATTACH:https://calrelay.example.test/agenda.txt\r\n\
END:VALARM\r\n\
BEGIN:VALARM\r\n\
ACTION:AUDIO\r\n\
TRIGGER:-PT5M\r\n\
ATTACH;FMTTYPE=audio/wav:https://calrelay.example.test/bell.wav\r\n\
END:VALARM\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

    let rich = normalize_ics(rich_ics, &IcsLimits::default()).expect("rich event normalizes");
    assert_eq!(rich.events.len(), 1);
    let ev = &rich.events[0];
    validate_event(ev).expect("rich event validates");
    assert_eq!(ev.title, "架构评审, 阶段一");
    assert_eq!(ev.description.as_deref(), Some("详情\n第二行"));
    assert_eq!(ev.location.as_deref(), Some("Room 42; West"));
    assert_eq!(
        ev.url.as_deref(),
        Some("https://calrelay.example.test/events/42")
    );
    assert_eq!(ev.status, Status::Tentative);
    assert_eq!(ev.classification, Some(Classification::Confidential));
    assert_eq!(ev.transparency, Some(Transparency::Transparent));
    assert_eq!(
        ev.categories,
        Some(vec!["ops,sec".to_owned(), "backend".to_owned()])
    );
    assert_eq!(ev.source_sequence, Some(7));
    assert_eq!(
        ev.source_modified_at.as_deref(),
        Some("2026-10-22T15:30:00Z")
    );
    assert_eq!(
        ev.organizer,
        Some(Person {
            address: "mailto:alice@example.test".to_owned(),
            display_name: Some("Alice, Lead".to_owned()),
            role: None,
            participation: None,
            rsvp: None,
            extensions: Some(vec![Extension {
                name: "X-ORG-UNIT".to_owned(),
                parameters: BTreeMap::new(),
                value: "core".to_owned(),
            }]),
        })
    );
    assert_eq!(
        ev.attendees,
        Some(vec![Person {
            address: "mailto:bob@example.test".to_owned(),
            display_name: Some("Bob".to_owned()),
            role: Some(Role::Chair),
            participation: Some(Participation::Accepted),
            rsvp: Some(true),
            extensions: Some(vec![Extension {
                name: "X-TEAM".to_owned(),
                parameters: BTreeMap::new(),
                value: "infra".to_owned(),
            }]),
        }])
    );
    assert_eq!(
        ev.attachments,
        Some(vec![Attachment {
            uri: "https://calrelay.example.test/spec.pdf".to_owned(),
            media_type: Some("application/pdf".to_owned()),
        }])
    );
    assert_eq!(
        ev.alarms,
        Some(vec![
            Alarm {
                id: "alarm-1".to_owned(),
                action: AlarmAction::Display,
                trigger: AlarmTrigger::Relative {
                    related: Related::End,
                    offset: "-PT10M".to_owned(),
                },
                repeat: Some(2),
                repeat_interval: Some("PT5M".to_owned()),
                description: Some("即将结束".to_owned()),
                summary: None,
                attendees: None,
                attachments: None,
                extensions: Some(vec![Extension {
                    name: "X-ALARM-TAG".to_owned(),
                    parameters: BTreeMap::new(),
                    value: "primary".to_owned(),
                }]),
            },
            Alarm {
                id: "alarm-2".to_owned(),
                action: AlarmAction::Email,
                trigger: AlarmTrigger::Absolute {
                    at: Temporal::Utc {
                        value: "2026-11-02T16:50:00Z".to_owned(),
                    },
                },
                repeat: None,
                repeat_interval: None,
                description: Some("请准时参会".to_owned()),
                summary: Some("邮件提醒".to_owned()),
                attendees: Some(vec![Person {
                    address: "mailto:bob@example.test".to_owned(),
                    display_name: None,
                    role: None,
                    participation: None,
                    rsvp: None,
                    extensions: None,
                }]),
                attachments: Some(vec![Attachment {
                    uri: "https://calrelay.example.test/agenda.txt".to_owned(),
                    media_type: None,
                }]),
                extensions: None,
            },
            Alarm {
                id: "alarm-3".to_owned(),
                action: AlarmAction::Audio,
                trigger: AlarmTrigger::Relative {
                    related: Related::Start,
                    offset: "-PT5M".to_owned(),
                },
                repeat: None,
                repeat_interval: None,
                description: None,
                summary: None,
                attendees: None,
                attachments: Some(vec![Attachment {
                    uri: "https://calrelay.example.test/bell.wav".to_owned(),
                    media_type: Some("audio/wav".to_owned()),
                }]),
                extensions: None,
            },
        ])
    );
}

#[test]
fn ac1_golden_recurrence_exceptions_and_cancellations_preserve_recurrence_id_and_spans() {
    let result =
        normalize_ics(RECURRENCE, &IcsLimits::default()).expect("recurrence-exceptions normalizes");
    assert!(result.diagnostics.is_empty());
    assert_eq!(result.events.len(), 1);

    let event = &result.events[0];
    validate_event(event).expect("recurring event with exceptions validates");
    assert_eq!(
        event,
        &EventV1 {
            schema_version: SchemaVersion::V1,
            external_uid: "series@example.test".to_owned(),
            title: "Series".to_owned(),
            description: None,
            location: None,
            url: None,
            categories: None,
            status: Status::Confirmed,
            classification: None,
            transparency: None,
            start: Temporal::Utc {
                value: "2026-11-02T17:00:00Z".to_owned(),
            },
            end: None,
            duration: Some("PT1H".to_owned()),
            recurrence: Some(Recurrence {
                rrule: Some("FREQ=DAILY;COUNT=7".to_owned()),
                rdates: Some(vec![
                    RDate::Period(Period {
                        kind: PeriodKind::Period,
                        start: Temporal::Utc {
                            value: "2026-11-10T17:00:00Z".to_owned(),
                        },
                        end: Some(Temporal::Utc {
                            value: "2026-11-10T19:00:00Z".to_owned(),
                        }),
                        duration: None,
                    }),
                    RDate::Period(Period {
                        kind: PeriodKind::Period,
                        start: Temporal::Utc {
                            value: "2026-11-11T17:00:00Z".to_owned(),
                        },
                        end: None,
                        duration: Some("PT3H".to_owned()),
                    }),
                ]),
                exdates: Some(vec![
                    Temporal::Utc {
                        value: "2026-11-03T17:00:00Z".to_owned(),
                    },
                    Temporal::Utc {
                        value: "2026-11-04T17:00:00Z".to_owned(),
                    },
                    Temporal::Utc {
                        value: "2026-11-05T17:00:00Z".to_owned(),
                    },
                ]),
                exceptions: Some(vec![
                    RecurrenceException {
                        recurrence_id: Temporal::Utc {
                            value: "2026-11-06T17:00:00Z".to_owned(),
                        },
                        range: RecurrenceRange::Single,
                        change: Change::Override {
                            patch: Box::new(EventPatch {
                                title: Some("Moved".to_owned()),
                                start: Some(Temporal::Utc {
                                    value: "2026-11-06T20:00:00Z".to_owned(),
                                }),
                                duration: Some(Some("PT2H".to_owned())),
                                ..EventPatch::default()
                            }),
                        },
                    },
                    RecurrenceException {
                        recurrence_id: Temporal::Utc {
                            value: "2026-11-07T17:00:00Z".to_owned(),
                        },
                        range: RecurrenceRange::Single,
                        change: Change::Cancelled,
                    },
                ]),
            }),
            organizer: None,
            attendees: None,
            alarms: None,
            attachments: None,
            source_sequence: None,
            source_modified_at: None,
            timezones: None,
            extensions: None,
        }
    );

    // Also verify master DTEND overridden by exception DURATION clears inherited `end`
    // so both `validate_event` and `expand_occurrences` succeed.
    let end_to_duration_ics = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//CalRelay//Issue 25 Span Override//EN\r\n\
BEGIN:VEVENT\r\n\
UID:span-switch@example.test\r\n\
DTSTAMP:20261022T120000Z\r\n\
DTSTART:20261102T170000Z\r\n\
DTEND:20261102T180000Z\r\n\
RRULE:FREQ=DAILY;COUNT=2\r\n\
SUMMARY:Master with DTEND\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:span-switch@example.test\r\n\
DTSTAMP:20261022T120000Z\r\n\
RECURRENCE-ID:20261103T170000Z\r\n\
DTSTART:20261103T190000Z\r\n\
DURATION:PT2H\r\n\
SUMMARY:Override with DURATION\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

    let switched = normalize_ics(end_to_duration_ics, &IcsLimits::default())
        .expect("DTEND master with DURATION exception normalizes");
    let switched_event = &switched.events[0];
    validate_event(switched_event)
        .expect("passes domain validation without EndAndDurationExclusive");
    let window_start = Temporal::Utc {
        value: "2026-11-01T00:00:00Z".to_owned(),
    }
    .to_utc()
    .unwrap()
    .unwrap();
    let window_end = Temporal::Utc {
        value: "2026-11-10T00:00:00Z".to_owned(),
    }
    .to_utc()
    .unwrap()
    .unwrap();
    let occs = expand_occurrences(
        switched_event,
        &ExpansionOptions {
            window_start,
            window_end,
            calendar_tzid: None,
            max_occurrences: 10,
            max_iterations: 10,
        },
    )
    .expect("expands occurrences cleanly");
    assert_eq!(occs.len(), 2);
    assert_eq!(occs[1].original_key, "utc:2026-11-03T17:00:00Z");
    assert_eq!(
        (occs[1].end_utc - occs[1].start_utc).num_minutes(),
        120,
        "exception DURATION:PT2H replaces master 1h DTEND"
    );

    // Verify case-insensitive component framing and empty SUMMARY: value normalize cleanly,
    // and malformed fields on a STATUS:CANCELLED exception VEVENT are rejected rather than ignored.
    let lowercase_empty_summary = "\
begin:vcalendar\n\
version:2.0\n\
prodid:-//CalRelay//Issue 25 Case//EN\n\
begin:vevent\n\
uid:case@example.test\n\
dtstamp:20261022T120000Z\n\
dtstart:20261102T170000Z\n\
summary:\n\
end:vevent\n\
end:vcalendar\n";
    let case_norm = normalize_ics(lowercase_empty_summary, &IcsLimits::default())
        .expect("case-insensitive framing and empty SUMMARY normalize");
    assert_eq!(case_norm.events.len(), 1);
    assert_eq!(case_norm.events[0].title, "");

    let invalid_cancelled_exception = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//CalRelay//Issue 25 Invalid Cancelled//EN\r\n\
BEGIN:VEVENT\r\n\
UID:bad-cancel@example.test\r\n\
DTSTAMP:20261022T120000Z\r\n\
DTSTART:20261102T170000Z\r\n\
DURATION:PT1H\r\n\
RRULE:FREQ=DAILY;COUNT=3\r\n\
SUMMARY:Master\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:bad-cancel@example.test\r\n\
DTSTAMP:20261022T120000Z\r\n\
RECURRENCE-ID:20261103T170000Z\r\n\
DTSTART:20261103T170000Z\r\n\
DTEND:20261103T160000Z\r\n\
STATUS:CANCELLED\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";
    let cancel_err = normalize_ics(invalid_cancelled_exception, &IcsLimits::default())
        .expect_err("invalid DTEND on a cancelled exception VEVENT must be rejected");
    assert!(matches!(
        cancel_err,
        IcsError::DomainValidation { ref errors, .. }
            if errors.iter().any(|e| e.code == Code::EndBeforeStart)
    ));

    // PR #54 review regression 1: Master with DTEND and an override exception on a later date
    // that omits both DTEND and DURATION must inherit the master's effective span rather than
    // failing domain validation with EndBeforeStart against the master's absolute DTEND.
    let inherited_dtend_span_ics = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//CalRelay//Issue 25 Inherited DTEND Span//EN\r\n\
BEGIN:VEVENT\r\n\
UID:meeting\\,team\\;ops@example.test\r\n\
DTSTAMP:20261022T120000Z\r\n\
DTSTART:20261102T170000Z\r\n\
DTEND:20261102T180000Z\r\n\
RRULE:FREQ=DAILY;COUNT=2\r\n\
SUMMARY:Daily with DTEND\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:meeting\\,team\\;ops@example.test\r\n\
DTSTAMP:20261022T120000Z\r\n\
RECURRENCE-ID:20261103T170000Z\r\n\
DTSTART:20261103T190000Z\r\n\
SUMMARY:Rescheduled without explicit DTEND or DURATION\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:allday-dtend@example.test\r\n\
DTSTAMP:20261022T120000Z\r\n\
DTSTART;VALUE=DATE:20261102\r\n\
DTEND;VALUE=DATE:20261104\r\n\
RRULE:FREQ=DAILY;COUNT=2\r\n\
SUMMARY:All-day two-day master\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:allday-dtend@example.test\r\n\
DTSTAMP:20261022T120000Z\r\n\
RECURRENCE-ID;VALUE=DATE:20261103\r\n\
DTSTART;VALUE=DATE:20261105\r\n\
SUMMARY:Rescheduled all-day without DTEND\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

    let inherited_norm = normalize_ics(inherited_dtend_span_ics, &IcsLimits::default())
        .expect("override omitting DTEND/DURATION on a DTEND master normalizes");
    assert_eq!(inherited_norm.events.len(), 2);
    // PR #54 review regression 2: UID TEXT escapes must be decoded into external_uid.
    assert_eq!(
        inherited_norm.events[0].external_uid,
        "meeting,team;ops@example.test"
    );
    let inherited_occs = expand_occurrences(
        &inherited_norm.events[0],
        &ExpansionOptions {
            window_start,
            window_end,
            calendar_tzid: None,
            max_occurrences: 10,
            max_iterations: 10,
        },
    )
    .expect("timed inherited DTEND override expands cleanly");
    assert_eq!(inherited_occs.len(), 2);
    assert_eq!(
        (inherited_occs[1].end_utc - inherited_occs[1].start_utc).num_minutes(),
        60,
        "rescheduled exception without DTEND/DURATION preserves master 1-hour span"
    );

    let allday_occs = expand_occurrences(
        &inherited_norm.events[1],
        &ExpansionOptions {
            window_start,
            window_end,
            calendar_tzid: Some("UTC"),
            max_occurrences: 10,
            max_iterations: 10,
        },
    )
    .expect("all-day inherited DTEND override expands cleanly");
    assert_eq!(allday_occs.len(), 2);
    assert_eq!(
        (allday_occs[1].end_utc - allday_occs[1].start_utc).num_days(),
        2,
        "rescheduled all-day exception without DTEND/DURATION preserves master 2-day span"
    );

    let bad_uid_escape = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//CalRelay//Issue 25 Bad UID Escape//EN\r\n\
BEGIN:VEVENT\r\n\
UID:bad\\qescape@example.test\r\n\
DTSTAMP:20261022T120000Z\r\n\
DTSTART:20261102T170000Z\r\n\
SUMMARY:Bad UID escape\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";
    assert!(matches!(
        normalize_ics(bad_uid_escape, &IcsLimits::default()),
        Err(IcsError::Malformed { .. })
    ));

    // PR #54 review round 2 regression: removing a detached exception with a higher SEQUENCE
    // must not decrease the normalized event's `source_sequence` (which tracks the master VEVENT).
    let snap_with_high_ex_seq = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//CalRelay//Issue 25 Master Sequence//EN\r\n\
BEGIN:VEVENT\r\n\
UID:seq-series@example.test\r\n\
DTSTAMP:20261022T120000Z\r\n\
SEQUENCE:1\r\n\
DTSTART:20261102T170000Z\r\n\
DURATION:PT1H\r\n\
RRULE:FREQ=DAILY;COUNT=3\r\n\
SUMMARY:Master at seq 1\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:seq-series@example.test\r\n\
DTSTAMP:20261022T120000Z\r\n\
RECURRENCE-ID:20261103T170000Z\r\n\
SEQUENCE:5\r\n\
DTSTART:20261103T180000Z\r\n\
DURATION:PT1H\r\n\
SUMMARY:Override at seq 5\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

    let snap_after_ex_removed = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//CalRelay//Issue 25 Master Sequence//EN\r\n\
BEGIN:VEVENT\r\n\
UID:seq-series@example.test\r\n\
DTSTAMP:20261022T120000Z\r\n\
SEQUENCE:1\r\n\
DTSTART:20261102T170000Z\r\n\
DURATION:PT1H\r\n\
RRULE:FREQ=DAILY;COUNT=3\r\n\
SUMMARY:Master at seq 1\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

    let before_removal =
        normalize_ics(snap_with_high_ex_seq, &IcsLimits::default()).expect("snap 1 normalizes");
    let after_removal =
        normalize_ics(snap_after_ex_removed, &IcsLimits::default()).expect("snap 2 normalizes");
    assert_eq!(before_removal.events[0].source_sequence, Some(1));
    assert_eq!(after_removal.events[0].source_sequence, Some(1));
}

#[test]
fn ac2_byte_property_event_and_parse_step_budgets_reject_whole_batch() {
    // 1. Invalid zero limits are rejected up front.
    assert!(matches!(
        normalize_ics(
            TIME_TEXT,
            &IcsLimits {
                max_bytes: 0,
                ..IcsLimits::default()
            }
        ),
        Err(IcsError::BudgetExceeded {
            kind: IcsBudgetKind::InvalidLimits,
            ..
        })
    ));

    // 2. Byte size limit.
    let byte_err = normalize_ics(
        TIME_TEXT,
        &IcsLimits {
            max_bytes: 64,
            ..IcsLimits::default()
        },
    )
    .expect_err("input larger than max_bytes must fail");
    assert!(matches!(
        byte_err,
        IcsError::BudgetExceeded {
            kind: IcsBudgetKind::Bytes,
            limit: 64,
            ..
        }
    ));

    // 3. Property count limit.
    let prop_err = normalize_ics(
        TIME_TEXT,
        &IcsLimits {
            max_properties: 5,
            ..IcsLimits::default()
        },
    )
    .expect_err("input with more than 5 properties must fail");
    assert!(matches!(
        prop_err,
        IcsError::BudgetExceeded {
            kind: IcsBudgetKind::Properties,
            limit: 5,
            ..
        }
    ));

    // 4. Event count limit.
    let event_err = normalize_ics(
        TIME_TEXT,
        &IcsLimits {
            max_events: 3,
            ..IcsLimits::default()
        },
    )
    .expect_err("4 events must exceed max_events = 3");
    assert!(matches!(
        event_err,
        IcsError::BudgetExceeded {
            kind: IcsBudgetKind::Events,
            limit: 3,
            observed: 4,
        }
    ));

    // 5. Parse step budget.
    let step_err = normalize_ics(
        TIME_TEXT,
        &IcsLimits {
            max_parse_steps: 4,
            ..IcsLimits::default()
        },
    )
    .expect_err("input exceeding max_parse_steps must fail");
    assert!(matches!(
        step_err,
        IcsError::BudgetExceeded {
            kind: IcsBudgetKind::ParseSteps,
            limit: 4,
            ..
        }
    ));
}

#[test]
fn ac2_whole_batch_failure_prevents_partial_snapshot_deletion() {
    // Valid snapshot produces a complete FullSnapshot SyncBatch with 4 Upsert records and 0 Delete records.
    let (batch, diagnostics) = normalize_ics_to_snapshot_batch(
        TIME_TEXT,
        &IcsLimits::default(),
        IcsSnapshotInput {
            source_id: "src-ics".to_owned(),
            calendar_id: "cal-main".to_owned(),
            base_revision: 1,
            deletion_policy: DeletionPolicy::Limit(5),
            proposed_checkpoint: Some("etag-1".to_owned()),
            verified_at: 1_700_000_000_000,
        },
    )
    .expect("valid ICS builds full snapshot batch");
    assert!(diagnostics.is_empty());
    assert!(matches!(
        batch.mode,
        SyncMode::FullSnapshot { complete: true, .. }
    ));
    assert_eq!(batch.records.len(), 4);
    assert!(batch
        .records
        .iter()
        .all(|r| matches!(r, SyncRecord::Upsert { .. })));

    // If a file contains one valid event and one malformed or unsupported event, the entire
    // normalization fails and produces no SyncBatch, preventing partial-snapshot deletions.
    let partial_corruption = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//CalRelay//Issue 25 Atomic Batch//EN\r\n\
BEGIN:VEVENT\r\n\
UID:keep-me@example.test\r\n\
DTSTAMP:20261022T120000Z\r\n\
DTSTART:20261102T170000Z\r\n\
DURATION:PT1H\r\n\
SUMMARY:Valid first event\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:corrupted@example.test\r\n\
DTSTAMP:20261022T120000Z\r\n\
DTSTART:20261102T170000Z\r\n\
DTEND:20261102T160000Z\r\n\
SUMMARY:End before start\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

    let err = normalize_ics_to_snapshot_batch(
        partial_corruption,
        &IcsLimits::default(),
        IcsSnapshotInput {
            source_id: "src-ics".to_owned(),
            calendar_id: "cal-main".to_owned(),
            base_revision: 1,
            deletion_policy: DeletionPolicy::Limit(5),
            proposed_checkpoint: Some("etag-2".to_owned()),
            verified_at: 1_700_000_005_000,
        },
    )
    .expect_err("one invalid VEVENT must reject the entire snapshot batch");

    match err {
        IcsError::DomainValidation { uid, errors, .. } => {
            assert_eq!(uid, "corrupted@example.test");
            assert!(errors.iter().any(|e| e.code == Code::EndBeforeStart));
        }
        other => panic!("expected DomainValidation for corrupted@example.test, got {other:?}"),
    }
}

#[test]
fn ac3_timezone_and_thisandfuture_fixture_maps_vtimezone_and_fails_with_unsupported_range() {
    // 1. Feeding #18's `timezone-and-range.ics` maps VTIMEZONE (`Custom/Source`) and
    //    `RANGE=THISANDFUTURE` into `EventV1`, then rejects the batch during domain validation
    //    with `Code::UnsupportedRange` (never silently treating THISANDFUTURE as single).
    let err = normalize_ics(TIMEZONE_RANGE, &IcsLimits::default())
        .expect_err("THISANDFUTURE must be rejected by domain validation");
    match err {
        IcsError::DomainValidation { uid, event, errors } => {
            assert_eq!(uid, "custom-series@example.test");
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0].code, Code::UnsupportedRange);
            assert_eq!(errors[0].path, "/recurrence/exceptions/0/range");
            let tzs = event.timezones.as_ref().expect("VTIMEZONE preserved");
            assert_eq!(tzs.len(), 1);
            assert_eq!(tzs[0].tzid, "Custom/Source");
            assert!(tzs[0].vtimezone.contains("BEGIN:STANDARD"));
            assert!(tzs[0].vtimezone.contains("BEGIN:DAYLIGHT"));
            let ex = &event
                .recurrence
                .as_ref()
                .unwrap()
                .exceptions
                .as_ref()
                .unwrap()[0];
            assert_eq!(ex.range, RecurrenceRange::ThisAndFuture);
        }
        other => panic!("expected DomainValidation with UnsupportedRange, got {other:?}"),
    }

    // 2. Without the THISANDFUTURE exception, the custom VTIMEZONE series normalizes cleanly
    //    (storing `Timezone { tzid: "Custom/Source", vtimezone }`), while UTC resolution / planning
    //    explicitly rejects the uninterpreted source VTIMEZONE with `Code::UnsupportedTimezone`.
    let custom_tz_only = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//CalRelay//Issue 25 Custom TZ//EN\r\n\
BEGIN:VTIMEZONE\r\n\
TZID:Custom/Source\r\n\
BEGIN:STANDARD\r\n\
DTSTART:19701101T020000\r\n\
TZOFFSETFROM:-0700\r\n\
TZOFFSETTO:-0800\r\n\
END:STANDARD\r\n\
END:VTIMEZONE\r\n\
BEGIN:VEVENT\r\n\
UID:custom-only@example.test\r\n\
DTSTAMP:20261022T120000Z\r\n\
DTSTART;TZID=Custom/Source:20261102T090000\r\n\
DURATION:PT1H\r\n\
SUMMARY:Custom zone event\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

    let normalized = normalize_ics(custom_tz_only, &IcsLimits::default())
        .expect("custom VTIMEZONE without THISANDFUTURE passes storage normalization");
    assert_eq!(normalized.events.len(), 1);
    let ev = &normalized.events[0];
    assert_eq!(
        ev.timezones,
        Some(vec![Timezone {
            tzid: "Custom/Source".to_owned(),
            vtimezone: "BEGIN:VTIMEZONE\r\nTZID:Custom/Source\r\nBEGIN:STANDARD\r\nDTSTART:19701101T020000\r\nTZOFFSETFROM:-0700\r\nTZOFFSETTO:-0800\r\nEND:STANDARD\r\nEND:VTIMEZONE".to_owned(),
        }])
    );
    assert_eq!(ev.resolve_utc(&ev.start), Err(Code::UnsupportedTimezone));
}

#[test]
fn ac3_non_vevent_components_are_retained_in_diagnostics_while_unsupported_or_malformed_vevents_fail(
) {
    // 1. VTODO, VJOURNAL, VFREEBUSY are retained as raw diagnostics and never mixed into VEVENTs.
    let mixed_components = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//CalRelay//Issue 25 Mixed//EN\r\n\
BEGIN:VEVENT\r\n\
UID:event-1@example.test\r\n\
DTSTAMP:20261022T120000Z\r\n\
DTSTART:20261102T170000Z\r\n\
DURATION:PT30M\r\n\
SUMMARY:Real Event\r\n\
END:VEVENT\r\n\
BEGIN:VTODO\r\n\
UID:todo-1@example.test\r\n\
DTSTAMP:20261022T120000Z\r\n\
SUMMARY:Task item\r\n\
END:VTODO\r\n\
BEGIN:VJOURNAL\r\n\
UID:journal-1@example.test\r\n\
DTSTAMP:20261022T120000Z\r\n\
SUMMARY:Journal note\r\n\
END:VJOURNAL\r\n\
END:VCALENDAR\r\n";

    let mixed = normalize_ics(mixed_components, &IcsLimits::default())
        .expect("VTODO and VJOURNAL are reported in diagnostics without failing VEVENT import");
    assert_eq!(mixed.events.len(), 1);
    assert_eq!(mixed.events[0].external_uid, "event-1@example.test");
    assert_eq!(
        mixed.diagnostics,
        vec![
            IcsDiagnostic {
                kind: RetainedComponentKind::VTodo,
                uid: Some("todo-1@example.test".to_owned()),
                raw: "BEGIN:VTODO\r\nUID:todo-1@example.test\r\nDTSTAMP:20261022T120000Z\r\nSUMMARY:Task item\r\nEND:VTODO".to_owned(),
            },
            IcsDiagnostic {
                kind: RetainedComponentKind::VJournal,
                uid: Some("journal-1@example.test".to_owned()),
                raw: "BEGIN:VJOURNAL\r\nUID:journal-1@example.test\r\nDTSTAMP:20261022T120000Z\r\nSUMMARY:Journal note\r\nEND:VJOURNAL".to_owned(),
            },
        ]
    );

    // 2. Malformed and unsupported inputs are explicitly rejected with clear diagnostics.
    let malformed_cases = [
        (
            "missing VCALENDAR wrapper",
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTAMP:20261022T120000Z\r\nDTSTART:20261102T170000Z\r\nEND:VEVENT\r\n",
        ),
        (
            "mismatched BEGIN/END tags",
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTAMP:20261022T120000Z\r\nDTSTART:20261102T170000Z\r\nEND:VCALENDAR\r\n",
        ),
        (
            "missing VERSION",
            "BEGIN:VCALENDAR\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTAMP:20261022T120000Z\r\nDTSTART:20261102T170000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        ),
        (
            "invalid TEXT escape sequence",
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTAMP:20261022T120000Z\r\nDTSTART:20261102T170000Z\r\nSUMMARY:Bad \\q escape\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        ),
        (
            "TZID on UTC timestamp",
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTAMP:20261022T120000Z\r\nDTSTART;TZID=America/Vancouver:20261102T170000Z\r\nSUMMARY:Bad UTC TZID\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        ),
        (
            "TZID on DATE value",
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTAMP:20261022T120000Z\r\nDTSTART;VALUE=DATE;TZID=America/Vancouver:20261102\r\nSUMMARY:Bad DATE TZID\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        ),
        (
            "duplicate master UID",
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:dup@example.test\r\nDTSTAMP:20261022T120000Z\r\nDTSTART:20261102T170000Z\r\nSUMMARY:One\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:dup@example.test\r\nDTSTAMP:20261022T120000Z\r\nDTSTART:20261103T170000Z\r\nSUMMARY:Two\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        ),
    ];

    for (label, input) in malformed_cases {
        let res = normalize_ics(input, &IcsLimits::default());
        assert!(res.is_err(), "{label} should be rejected, got {res:?}");
    }

    let unsupported_cases = [
        (
            "non-2.0 VERSION",
            "BEGIN:VCALENDAR\r\nVERSION:1.0\r\nPRODID:-//Test//EN\r\nEND:VCALENDAR\r\n",
        ),
        (
            "iTIP METHOD:REQUEST",
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nMETHOD:REQUEST\r\nEND:VCALENDAR\r\n",
        ),
        (
            "non-GREGORIAN CALSCALE",
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nCALSCALE:ISLAMIC\r\nEND:VCALENDAR\r\n",
        ),
        (
            "orphan RECURRENCE-ID without master VEVENT",
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:orphan@example.test\r\nDTSTAMP:20261022T120000Z\r\nRECURRENCE-ID:20261102T170000Z\r\nDTSTART:20261102T180000Z\r\nSUMMARY:Orphan\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        ),
        (
            "multiple RRULE properties on one VEVENT",
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:multi-rrule@example.test\r\nDTSTAMP:20261022T120000Z\r\nDTSTART:20261102T170000Z\r\nRRULE:FREQ=DAILY;COUNT=2\r\nRRULE:FREQ=WEEKLY;COUNT=2\r\nSUMMARY:Multi RRULE\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        ),
        (
            "RRULE on exception VEVENT",
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:ex-rrule@example.test\r\nDTSTAMP:20261022T120000Z\r\nDTSTART:20261102T170000Z\r\nRRULE:FREQ=DAILY;COUNT=3\r\nSUMMARY:Master\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:ex-rrule@example.test\r\nDTSTAMP:20261022T120000Z\r\nRECURRENCE-ID:20261103T170000Z\r\nDTSTART:20261103T180000Z\r\nRRULE:FREQ=DAILY;COUNT=2\r\nSUMMARY:Ex with RRULE\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        ),
        (
            "inline binary ATTACH",
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:bin@example.test\r\nDTSTAMP:20261022T120000Z\r\nDTSTART:20261102T170000Z\r\nATTACH;ENCODING=BASE64;VALUE=BINARY:SGVsbG8=\r\nSUMMARY:Binary attach\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        ),
        (
            "unsupported VEVENT property PRIORITY",
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:prio@example.test\r\nDTSTAMP:20261022T120000Z\r\nDTSTART:20261102T170000Z\r\nPRIORITY:1\r\nSUMMARY:Priority\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        ),
        (
            "unsupported VALARM ACTION:PROCEDURE",
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:proc@example.test\r\nDTSTAMP:20261022T120000Z\r\nDTSTART:20261102T170000Z\r\nSUMMARY:Alarm\r\nBEGIN:VALARM\r\nACTION:PROCEDURE\r\nTRIGGER:-PT5M\r\nATTACH:https://example.test/run.bin\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        ),
    ];

    for (label, input) in unsupported_cases {
        let err = normalize_ics(input, &IcsLimits::default())
            .expect_err(&format!("{label} must be rejected"));
        assert!(
            matches!(err, IcsError::Unsupported { .. }),
            "{label} should return IcsError::Unsupported, got {err:?}"
        );
    }
}
