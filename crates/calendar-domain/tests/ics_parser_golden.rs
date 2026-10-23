//! Issue #18: fixed, independent RFC 5545 witnesses for two parser candidates.
//! This compares syntax preservation; it does not claim recurrence execution or import support.

#[path = "support/ics_evaluation.rs"]
mod ics_evaluation;

use ics_evaluation::{parse, Candidate, Component, Property, CANDIDATES};

const TIME_TEXT: &str = include_str!("fixtures/ics/time-and-text.ics");
const RECURRENCE: &str = include_str!("fixtures/ics/recurrence-exceptions.ics");
const TIMEZONE_RANGE: &str = include_str!("fixtures/ics/timezone-and-range.ics");

fn calendar(candidate: Candidate, input: &str) -> Component {
    // Repository checkout uses LF; feed each candidate RFC content-line CRLF.
    let input = input.replace("\r\n", "\n").replace('\n', "\r\n");
    let parsed = parse(candidate, &input).unwrap_or_else(|e| panic!("{candidate:?}: {e}"));
    assert_eq!(parsed.name, "VCALENDAR", "{candidate:?}");
    parsed
}

fn property<'a>(component: &'a Component, name: &str) -> &'a Property {
    component
        .property(name)
        .unwrap_or_else(|| panic!("{} lacks {name}: {component:?}", component.name))
}

fn values<'a>(component: &'a Component, name: &str) -> Vec<&'a str> {
    component
        .properties
        .iter()
        .filter(|p| p.name == name)
        .map(|p| p.value.as_str())
        .collect()
}

fn events(calendar: &Component) -> Vec<&Component> {
    calendar
        .components
        .iter()
        .filter(|c| c.name == "VEVENT")
        .collect()
}

fn parameter(property: &Property, name: &str, expected: &[&str]) {
    let expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        property.parameter(name),
        Some(expected.as_slice()),
        "{} parameter {name}",
        property.name
    );
}

#[test]
fn ac1_four_temporal_forms_preserve_literal_values_and_parameters() {
    for candidate in CANDIDATES {
        let parsed = calendar(candidate, TIME_TEXT);
        let events = events(&parsed);
        assert_eq!(events.len(), 4, "{candidate:?}");
        let expected = [
            ("date@example.test", "20261102"),
            ("utc@example.test", "20261102T170000Z"),
            ("floating@example.test", "20261102T090000"),
            ("zoned@example.test", "20261102T090000"),
        ];
        for (event, (uid, start)) in events.iter().zip(expected) {
            assert_eq!(property(event, "UID").value, uid, "{candidate:?}");
            assert_eq!(property(event, "DTSTART").value, start, "{candidate:?}");
        }
        parameter(property(events[0], "DTSTART"), "VALUE", &["DATE"]);
        assert_eq!(property(events[0], "DTEND").value, "20261103");
        assert_eq!(property(events[1], "DTSTART").parameter("TZID"), None);
        assert_eq!(property(events[2], "DTSTART").parameter("TZID"), None);
        parameter(
            property(events[3], "DTSTART"),
            "TZID",
            &["America/Vancouver"],
        );
    }
}

#[test]
fn ac1_unicode_text_escaping_and_space_or_tab_folding_have_fixed_oracles() {
    for candidate in CANDIDATES {
        let parsed = calendar(candidate, TIME_TEXT);
        let events = events(&parsed);
        assert_eq!(events.len(), 4, "{candidate:?}");
        assert_eq!(property(events[0], "SUMMARY").value, "全天会议 🌏");
        assert_eq!(property(events[1], "SUMMARY").value, "Unicode café 东京");
        let description = match candidate {
            Candidate::Ical => r"One\, two\; three\\four\n第二行",
            Candidate::Icalendar => "One, two; three\\four\n第二行",
        };
        assert_eq!(
            property(events[1], "DESCRIPTION").value,
            description,
            "{candidate:?}"
        );
        assert_eq!(
            property(events[1], "X-LONG").value,
            concat!(
                "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefgh",
                "ijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmno",
                "pqrstuvwxyz-END"
            ),
            "{candidate:?}"
        );
    }
}

#[test]
fn ac1_repeated_properties_quoted_values_and_repeated_extension_parameters_survive() {
    for candidate in CANDIDATES {
        let parsed = calendar(candidate, TIME_TEXT);
        let events = events(&parsed);
        assert_eq!(events.len(), 4, "{candidate:?}");
        let event = events[1];
        assert_eq!(values(event, "CATEGORIES"), ["work", "家庭"]);
        assert_eq!(
            values(event, "ATTENDEE"),
            ["mailto:jane@example.test", "mailto:li@example.test"]
        );
        let attendees: Vec<_> = event
            .properties
            .iter()
            .filter(|p| p.name == "ATTENDEE")
            .collect();
        parameter(attendees[0], "CN", &["Doe, Jane"]);
        parameter(attendees[0], "ROLE", &["REQ-PARTICIPANT"]);
        parameter(attendees[1], "CN", &["李雷"]);
        parameter(attendees[1], "ROLE", &["OPT-PARTICIPANT"]);
        assert_eq!(values(event, "X-CUSTOM"), ["first", "second"]);
        let extension = property(event, "X-CUSTOM");
        assert_eq!(
            extension.params,
            vec![
                ("X-LABEL".to_string(), vec!["alpha,beta".to_string()]),
                ("X-LABEL".to_string(), vec!["gamma".to_string()]),
            ],
            "{candidate:?}: extension parameter multiplicity"
        );
    }
}

#[test]
fn ac2_exdate_lists_and_both_period_encodings_are_preserved_without_flattening() {
    for candidate in CANDIDATES {
        let parsed = calendar(candidate, RECURRENCE);
        let events = events(&parsed);
        assert_eq!(events.len(), 3, "{candidate:?}");
        assert_eq!(values(events[0], "RRULE"), ["FREQ=DAILY;COUNT=7"]);
        assert_eq!(
            values(events[0], "EXDATE"),
            ["20261103T170000Z,20261104T170000Z", "20261105T170000Z"]
        );
        assert_eq!(
            values(events[0], "RDATE"),
            ["20261110T170000Z/20261110T190000Z", "20261111T170000Z/PT3H"]
        );
        for period in events[0].properties.iter().filter(|p| p.name == "RDATE") {
            parameter(period, "VALUE", &["PERIOD"]);
        }
    }
}

#[test]
fn ac2_reschedule_and_cancel_keep_original_recurrence_id_and_revision() {
    for candidate in CANDIDATES {
        let parsed = calendar(candidate, RECURRENCE);
        let events = events(&parsed);
        assert_eq!(events.len(), 3, "{candidate:?}");
        assert!(events
            .iter()
            .all(|e| property(e, "UID").value == "series@example.test"));
        assert_eq!(
            property(events[1], "RECURRENCE-ID").value,
            "20261106T170000Z"
        );
        assert_eq!(property(events[1], "DTSTART").value, "20261106T200000Z");
        assert_eq!(property(events[1], "SEQUENCE").value, "2");
        assert_eq!(property(events[1], "DURATION").value, "PT2H");
        assert_eq!(
            property(events[2], "RECURRENCE-ID").value,
            "20261107T170000Z"
        );
        assert_eq!(property(events[2], "STATUS").value, "CANCELLED");
        assert_eq!(property(events[2], "SEQUENCE").value, "3");
    }
}

#[test]
fn ac2_timezone_subcomponents_and_thisandfuture_remain_explicit_source_data() {
    for candidate in CANDIDATES {
        let parsed = calendar(candidate, TIMEZONE_RANGE);
        assert_eq!(parsed.components.len(), 3, "{candidate:?}");
        let timezone = &parsed.components[0];
        assert_eq!(timezone.name, "VTIMEZONE");
        assert_eq!(property(timezone, "TZID").value, "Custom/Source");
        assert_eq!(property(timezone, "X-LIC-LOCATION").value, "Source City");
        assert_eq!(timezone.components.len(), 2);
        let standard = &timezone.components[0];
        let daylight = &timezone.components[1];
        assert_eq!(standard.name, "STANDARD");
        assert_eq!(daylight.name, "DAYLIGHT");
        assert_eq!(property(standard, "DTSTART").value, "19701101T020000");
        assert_eq!(property(standard, "TZOFFSETFROM").value, "-0700");
        assert_eq!(property(standard, "TZOFFSETTO").value, "-0800");
        assert_eq!(
            property(standard, "RRULE").value,
            "FREQ=YEARLY;BYMONTH=11;BYDAY=1SU"
        );
        assert_eq!(property(daylight, "DTSTART").value, "19700308T020000");
        assert_eq!(property(daylight, "TZOFFSETFROM").value, "-0800");
        assert_eq!(property(daylight, "TZOFFSETTO").value, "-0700");
        assert_eq!(
            property(daylight, "RRULE").value,
            "FREQ=YEARLY;BYMONTH=3;BYDAY=2SU"
        );
        let events = events(&parsed);
        parameter(property(events[0], "DTSTART"), "TZID", &["Custom/Source"]);
        let id = property(events[1], "RECURRENCE-ID");
        assert_eq!(id.value, "20261104T090000");
        parameter(id, "TZID", &["Custom/Source"]);
        parameter(id, "RANGE", &["THISANDFUTURE"]);
        assert_eq!(property(events[1], "DTSTART").value, "20261104T100000");
    }
}

#[test]
fn ac3_literal_backslash_n_exposes_icalendar_text_decoding_loss() {
    // RFC 5545 section 3.3.11: the escaped backslash is literal, so the
    // semantic text is backslash + 'n', not a line break. Record the candidate
    // defect rather than silently correcting its parsed output in the helper.
    let ical = calendar(Candidate::Ical, TIME_TEXT);
    let icalendar = calendar(Candidate::Icalendar, TIME_TEXT);
    let raw = property(events(&ical)[2], "DESCRIPTION");
    let lossy = property(events(&icalendar)[2], "DESCRIPTION");
    assert_eq!(raw.value, r"Literal \\n token");
    assert_eq!(lossy.value, "Literal \n token");
    assert_ne!(
        lossy.value, r"Literal \n token",
        "candidate unexpectedly preserves RFC text; revisit selection evidence"
    );
}
