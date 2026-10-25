//! #31 acceptance tests: protocol goldens, rejection boundaries and semantic round trips.
use calendar_adapters::{
    normalize_ics,
    renderer::{render_ics, FeedEvent, RenderError},
    IcsLimits,
};
use calendar_domain::{EventV1, Status, Temporal};
use serde_json::json;

fn event() -> EventV1 {
    serde_json::from_value(json!({"schema_version":"1.0", "external_uid":"upstream", "title":"Meeting", "status":"confirmed", "start":{"kind":"utc", "value":"2026-11-02T09:00:00Z"}, "duration":"PT1H"})).unwrap()
}
fn entry(event: &EventV1) -> FeedEvent<'_> {
    FeedEvent {
        event_id: "internal-1",
        revision: 3,
        changed_at: "2026-10-25T12:00:00Z",
        deleted: false,
        document: event,
    }
}
fn parse(ics: &str) -> Vec<EventV1> {
    normalize_ics(ics, &IcsLimits::default()).unwrap().events
}

#[test]
fn ac1_revision_identity_and_tombstone_golden() {
    let event = event();
    let rendered = render_ics(&[entry(&event)]).unwrap();
    assert_eq!(rendered, include_str!("fixtures/render/utc.ics"));
    assert_eq!(rendered, render_ics(&[entry(&event)]).unwrap());
    let mut changed = event.clone();
    changed.external_uid = "renamed upstream".into();
    changed.source_sequence = Some(999);
    changed.source_modified_at = Some("2020-01-01T00:00:00Z".into());
    let mut tombstone = entry(&changed);
    tombstone.revision = 4;
    tombstone.changed_at = "2026-10-26T13:00:00Z";
    tombstone.deleted = true;
    let output = render_ics(&[tombstone]).unwrap();
    assert_eq!(output, include_str!("fixtures/render/deleted.ics"));
    let before = parse(&rendered).remove(0);
    let after = parse(&output).remove(0);
    assert_eq!(before.external_uid, after.external_uid);
    assert_eq!(after.status, Status::Cancelled);
    assert_eq!(after.source_sequence, Some(3));
    let mut other = entry(&event);
    other.event_id = "internal-2";
    assert_ne!(
        before.external_uid,
        parse(&render_ics(&[other]).unwrap())[0].external_uid
    );
}

#[test]
fn ac1_four_temporal_families_golden_and_round_trip() {
    for (kind, start, end) in [
        (
            "date",
            json!({"kind":"date","date":"2026-11-02"}),
            json!({"kind":"date","date":"2026-11-03"}),
        ),
        (
            "utc",
            json!({"kind":"utc","value":"2026-11-02T09:00:00Z"}),
            json!({"kind":"utc","value":"2026-11-02T10:00:00Z"}),
        ),
        (
            "floating",
            json!({"kind":"floating","local":"2026-11-02T09:00:00"}),
            json!({"kind":"floating","local":"2026-11-02T10:00:00"}),
        ),
        (
            "zoned",
            json!({"kind":"zoned","local":"2026-11-02T09:00:00","tzid":"Custom/Source"}),
            json!({"kind":"zoned","local":"2026-11-02T10:00:00","tzid":"Custom/Source"}),
        ),
    ] {
        let mut value = serde_json::to_value(event()).unwrap();
        value["start"] = start;
        value["end"] = end;
        value.as_object_mut().unwrap().remove("duration");
        if kind == "zoned" {
            value["timezones"] = json!([{"tzid":"Custom/Source", "vtimezone":include_str!("fixtures/render/source.vtimezone")}]);
        }
        let original: EventV1 = serde_json::from_value(value).unwrap();
        let output = render_ics(&[entry(&original)]).unwrap();
        let expected = match kind {
            "date" => include_str!("fixtures/render/date.ics"),
            "utc" => include_str!("fixtures/render/utc-end.ics"),
            "floating" => include_str!("fixtures/render/floating.ics"),
            _ => include_str!("fixtures/render/zoned.ics"),
        };
        assert_eq!(output, expected, "{kind}");
        let roundtrip = parse(&output).remove(0);
        assert_eq!(roundtrip.start, original.start);
        assert_eq!(roundtrip.end, original.end);
        assert_eq!(roundtrip.duration, original.duration);
        if kind == "zoned" {
            assert_eq!(
                roundtrip.timezones.as_ref().unwrap()[0].tzid,
                "Custom/Source"
            );
        }
    }
}

#[test]
fn ac2_recurrence_cancellation_and_period_semantics() {
    let mut value = serde_json::to_value(event()).unwrap();
    value["recurrence"] = json!({"rrule":"FREQ=DAILY;COUNT=5", "exdates":[{"kind":"utc","value":"2026-11-03T09:00:00Z"}], "rdates":[{"kind":"period","start":{"kind":"utc","value":"2026-11-09T09:00:00Z"},"duration":"PT2H"}], "exceptions":[{"recurrence_id":{"kind":"utc","value":"2026-11-04T09:00:00Z"},"range":"single","change":{"kind":"cancelled"}}]});
    let original: EventV1 = serde_json::from_value(value).unwrap();
    let output = render_ics(&[entry(&original)]).unwrap();
    assert_eq!(output, include_str!("fixtures/render/recurrence.ics"));
    assert_eq!(parse(&output)[0].recurrence, original.recurrence);
    let mut deleted = entry(&original);
    deleted.deleted = true;
    let output = render_ics(&[deleted]).unwrap();
    assert_eq!(output.matches("BEGIN:VEVENT").count(), 1);
    assert_eq!(parse(&output)[0].status, Status::Cancelled);
}

#[test]
fn ac3_unicode_escape_folding_and_semantic_round_trip() {
    let mut original = event();
    original.title = "🌏東京é".repeat(32);
    original.description = Some("One, two; three\\four\n第二行\tend".into());
    original.location = Some("A;B,C\\D".into());
    original.categories = Some(vec!["a,b".into(), "c;d".into()]);
    let output = render_ics(&[entry(&original)]).unwrap();
    assert!(output.contains("\r\n "));
    assert!(output.split("\r\n").all(|line| line.len() <= 75));
    assert!(!output.replace("\r\n", "").contains(['\r', '\n']));
    let mut decoded = parse(&output).remove(0);
    decoded.external_uid = original.external_uid.clone();
    decoded.source_sequence = original.source_sequence;
    assert_eq!(decoded, original);
    let rerendered = render_ics(&[entry(&decoded)]).unwrap();
    assert_eq!(output, rerendered);
}

#[test]
fn unsupported_or_invalid_input_is_an_atomic_error() {
    let mut bad = event();
    bad.start = Temporal::Zoned {
        local: "2026-11-02T09:00:00".into(),
        tzid: "America/Vancouver".into(),
    };
    assert!(matches!(
        render_ics(&[entry(&bad)]),
        Err(RenderError::Unsupported(_))
    ));
    for range in ["single", "this_and_future"] {
        let mut value = serde_json::to_value(event()).unwrap();
        value["recurrence"] = json!({"rrule":"FREQ=DAILY;COUNT=5","exceptions":[{"recurrence_id":{"kind":"utc","value":"2026-11-03T09:00:00Z"},"range":range,"change":{"kind":"override","patch":{"title":"Moved"}}}]});
        let bad = serde_json::from_value(value).unwrap();
        assert!(render_ics(&[entry(&bad)]).is_err());
    }
    let original = event();
    let mut invalid = entry(&original);
    invalid.revision = 0;
    assert!(matches!(
        render_ics(&[invalid]),
        Err(RenderError::Metadata(_))
    ));
    invalid.revision = i32::MAX as u64 + 2;
    assert!(render_ics(&[invalid]).is_err());
    invalid.revision = 1;
    invalid.changed_at = "2026-02-30T12:00:00Z";
    assert!(render_ics(&[invalid]).is_err());
    assert!(render_ics(&[entry(&original), entry(&original)]).is_err());
    for field in ["title", "url"] {
        let mut value = serde_json::to_value(event()).unwrap();
        value[field] = json!("injected\r\nBEGIN:VEVENT");
        let bad = serde_json::from_value(value).unwrap();
        let mut other = entry(&bad);
        other.event_id = "other";
        assert!(render_ics(&[entry(&original), other]).is_err());
    }
    let mut value = serde_json::to_value(event()).unwrap();
    value["extensions"] = json!([{"name":"X-UNKNOWN", "parameters":{}, "value":"data"}]);
    let bad = serde_json::from_value(value).unwrap();
    assert!(matches!(
        render_ics(&[entry(&bad)]),
        Err(RenderError::Unsupported(_))
    ));
}

#[test]
fn ac2_timezone_conflicts_and_component_injection_are_rejected() {
    let mut value = serde_json::to_value(event()).unwrap();
    value["start"] = json!({"kind":"zoned", "local":"2026-11-02T09:00:00", "tzid":"Custom/Source"});
    value["timezones"] = json!([{"tzid":"Custom/Source","vtimezone":include_str!("fixtures/render/source.vtimezone")}]);
    let first: EventV1 = serde_json::from_value(value.clone()).unwrap();
    value["timezones"][0]["vtimezone"] =
        json!(include_str!("fixtures/render/source.vtimezone").replace("+0100", "+0200"));
    let second: EventV1 = serde_json::from_value(value.clone()).unwrap();
    let mut other = entry(&second);
    other.event_id = "other";
    assert!(matches!(
        render_ics(&[entry(&first), other]),
        Err(RenderError::Timezone(_))
    ));
    for raw in [
        "BEGIN:VTIMEZONE\nTZID:Other\nEND:VTIMEZONE",
        "BEGIN:VEVENT\nUID:injected\nEND:VEVENT",
        "",
    ] {
        value["timezones"][0]["vtimezone"] = json!(raw);
        let bad: EventV1 = serde_json::from_value(value.clone()).unwrap();
        assert!(render_ics(&[entry(&bad)]).is_err());
    }
    let mut shared = entry(&first);
    shared.event_id = "other";
    let output = render_ics(&[entry(&first), shared]).unwrap();
    assert_eq!(output.matches("BEGIN:VTIMEZONE").count(), 1);
}

#[test]
fn ac2_timezone_binding_cannot_be_borrowed_from_another_event() {
    let mut value = serde_json::to_value(event()).unwrap();
    value["start"] =
        json!({"kind":"zoned","local":"2026-11-02T09:00:00","tzid":"America/Vancouver"});
    let missing: EventV1 = serde_json::from_value(value.clone()).unwrap();
    value["timezones"] = json!([{"tzid":"America/Vancouver","vtimezone":include_str!("fixtures/render/source.vtimezone").replace("Custom/Source", "America/Vancouver")}]);
    let custom: EventV1 = serde_json::from_value(value).unwrap();
    let mut other = entry(&custom);
    other.event_id = "other";
    assert!(
        render_ics(&[entry(&missing), other]).is_err(),
        "a same-name custom definition must not reinterpret another event's IANA binding"
    );
}

#[test]
fn ac2_date_periods_are_explicitly_rejected() {
    let mut value = serde_json::to_value(event()).unwrap();
    value["start"] = json!({"kind":"date","date":"2026-11-02"});
    value["duration"] = json!("P1D");
    value["recurrence"] = json!({"rdates":[{"kind":"period","start":{"kind":"date","date":"2026-11-09"},"duration":"P1D"}]});
    let original: EventV1 = serde_json::from_value(value).unwrap();
    calendar_domain::validate_event(&original).unwrap();
    assert!(matches!(
        render_ics(&[entry(&original)]),
        Err(RenderError::Unsupported(_))
    ));
}

#[test]
fn ac2_period_endpoints_require_one_timezone_binding() {
    for (start, end) in [
        (
            json!({"kind":"zoned","local":"2026-11-09T09:00:00","tzid":"Custom/Source"}),
            json!({"kind":"zoned","local":"2026-11-09T11:00:00","tzid":"Custom/Other"}),
        ),
        (
            json!({"kind":"zoned","local":"2026-11-09T09:00:00","tzid":"Custom/Source"}),
            json!({"kind":"utc","value":"2026-11-09T11:00:00Z"}),
        ),
        (
            json!({"kind":"utc","value":"2026-11-09T09:00:00Z"}),
            json!({"kind":"zoned","local":"2026-11-09T11:00:00","tzid":"Custom/Source"}),
        ),
    ] {
        let mut value = serde_json::to_value(event()).unwrap();
        value["timezones"] = json!([
            {"tzid":"Custom/Source","vtimezone":include_str!("fixtures/render/source.vtimezone")},
            {"tzid":"Custom/Other","vtimezone":include_str!("fixtures/render/source.vtimezone").replace("Custom/Source","Custom/Other")}
        ]);
        value["start"] = start.clone();
        value["recurrence"] = json!({"rdates":[{"kind":"period","start":start,"end":end}]});
        let original: EventV1 = serde_json::from_value(value).unwrap();
        calendar_domain::validate_event(&original).unwrap();
        assert!(matches!(
            render_ics(&[entry(&original)]),
            Err(RenderError::Unsupported(_))
        ));
    }
}

#[test]
fn ac2_period_endpoints_with_same_binding_round_trip() {
    for (start, end) in [
        (
            json!({"kind":"utc","value":"2026-11-09T09:00:00Z"}),
            json!({"kind":"utc","value":"2026-11-09T11:00:00Z"}),
        ),
        (
            json!({"kind":"floating","local":"2026-11-09T09:00:00"}),
            json!({"kind":"floating","local":"2026-11-09T11:00:00"}),
        ),
        (
            json!({"kind":"zoned","local":"2026-11-09T09:00:00","tzid":"Custom/Source"}),
            json!({"kind":"zoned","local":"2026-11-09T11:00:00","tzid":"Custom/Source"}),
        ),
    ] {
        let mut value = serde_json::to_value(event()).unwrap();
        value["start"] = start.clone();
        if start["kind"] == "zoned" {
            value["timezones"] = json!([{"tzid":"Custom/Source","vtimezone":include_str!("fixtures/render/source.vtimezone")}]);
        }
        value["recurrence"] = json!({"rdates":[{"kind":"period","start":start,"end":end}]});
        let original: EventV1 = serde_json::from_value(value).unwrap();
        let output = render_ics(&[entry(&original)]).unwrap();
        assert_eq!(parse(&output)[0].recurrence, original.recurrence);
    }
}

#[test]
fn ac2_zero_duration_periods_are_rejected_without_restricting_event_duration() {
    for duration in ["PT0S", "+PT00H0M00S", "P0D", "P000W", "P0DT0H0M0S"] {
        let mut value = serde_json::to_value(event()).unwrap();
        value["recurrence"] = json!({"rdates":[{"kind":"period","start":{"kind":"utc","value":"2026-11-09T09:00:00Z"},"duration":duration}]});
        let original: EventV1 = serde_json::from_value(value).unwrap();
        calendar_domain::validate_event(&original).unwrap();
        assert!(
            render_ics(&[entry(&original)]).is_err(),
            "zero PERIOD {duration} must reject"
        );
    }
    let mut original = event();
    original.duration = Some("PT0S".into());
    assert!(render_ics(&[entry(&original)]).is_ok());
    for duration in ["PT0H0M1S", "P0001D"] {
        let mut value = serde_json::to_value(event()).unwrap();
        value["recurrence"] = json!({"rdates":[{"kind":"period","start":{"kind":"utc","value":"2026-11-09T09:00:00Z"},"duration":duration}]});
        let original: EventV1 = serde_json::from_value(value).unwrap();
        let output = render_ics(&[entry(&original)]).unwrap();
        assert_eq!(parse(&output)[0].recurrence, original.recurrence);
    }
}

#[test]
fn ac2_equal_period_endpoints_are_rejected_in_each_supported_time_family() {
    for start in [
        json!({"kind":"utc","value":"2026-11-09T09:00:00Z"}),
        json!({"kind":"floating","local":"2026-11-09T09:00:00"}),
        json!({"kind":"zoned","local":"2026-11-09T09:00:00","tzid":"Custom/Source"}),
    ] {
        let mut value = serde_json::to_value(event()).unwrap();
        value["start"] = start.clone();
        if start["kind"] == "zoned" {
            value["timezones"] = json!([{"tzid":"Custom/Source","vtimezone":include_str!("fixtures/render/source.vtimezone")}]);
        }
        value["recurrence"] = json!({"rdates":[{"kind":"period","start":start,"end":start}]});
        let original: EventV1 = serde_json::from_value(value).unwrap();
        calendar_domain::validate_event(&original).unwrap();
        assert!(
            render_ics(&[entry(&original)]).is_err(),
            "equal PERIOD endpoints must reject"
        );
    }
}
