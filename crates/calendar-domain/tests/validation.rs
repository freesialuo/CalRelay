//! Behavioural tests for EventV1 domain types and semantic validation (Issue #2).

use calendar_domain::{validate_event, Code, EventV1, Temporal, ValidationError};
use serde_json::{json, Value};

const EXAMPLE: &str = include_str!("../../../examples/event-v1.json");

fn event(extra: Value) -> Value {
    let mut base = json!({
        "schema_version": "1.0",
        "external_uid": "uid-1",
        "title": "t",
        "status": "confirmed",
        "start": {"kind": "utc", "value": "2026-11-02T17:00:00Z"}
    });
    for (k, v) in extra.as_object().unwrap() {
        base[k] = v.clone();
    }
    base
}

fn check(value: Value) -> Result<(), Vec<ValidationError>> {
    let ev: EventV1 = serde_json::from_value(value).expect("shape must deserialize");
    validate_event(&ev)
}

fn codes(value: Value) -> Vec<Code> {
    match check(value) {
        Ok(()) => vec![],
        Err(errs) => errs.into_iter().map(|e| e.code).collect(),
    }
}

fn zoned(local: &str, tzid: &str) -> Value {
    json!({"kind": "zoned", "local": local, "tzid": tzid})
}

fn utc(value: &str) -> Value {
    json!({"kind": "utc", "value": value})
}

fn date(d: &str) -> Value {
    json!({"kind": "date", "date": d})
}

fn floating(l: &str) -> Value {
    json!({"kind": "floating", "local": l})
}

#[test]
fn shipped_example_is_valid_and_round_trips() {
    let raw: Value = serde_json::from_str(EXAMPLE).unwrap();
    let ev: EventV1 = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(validate_event(&ev), Ok(()));
    assert_eq!(serde_json::to_value(&ev).unwrap(), raw);
}

#[test]
fn unknown_fields_and_versions_are_rejected_at_the_boundary() {
    let mut bad = event(json!({}));
    bad["surprise"] = json!(1);
    assert!(serde_json::from_value::<EventV1>(bad).is_err());
    let mut bad = event(json!({}));
    bad["schema_version"] = json!("2.0");
    assert!(serde_json::from_value::<EventV1>(bad).is_err());
}

#[test]
fn temporal_kinds_keep_distinct_semantics() {
    let f: Temporal = serde_json::from_value(floating("2026-11-02T09:00:00")).unwrap();
    assert!(matches!(f, Temporal::Floating { .. }));
    assert_eq!(serde_json::to_value(&f).unwrap()["kind"], "floating");
    let d: Temporal = serde_json::from_value(date("2026-11-02")).unwrap();
    assert!(matches!(d, Temporal::Date { .. }));
}

#[test]
fn end_and_duration_are_mutually_exclusive() {
    let c = codes(event(json!({
        "end": utc("2026-11-02T18:00:00Z"),
        "duration": "PT1H"
    })));
    assert!(c.contains(&Code::EndAndDurationExclusive));
}

#[test]
fn all_day_event_uses_exclusive_end_date() {
    let ok = event(json!({"start": date("2026-11-02"), "end": date("2026-11-03")}));
    assert_eq!(codes(ok), vec![]);
    let same = event(json!({"start": date("2026-11-02"), "end": date("2026-11-02")}));
    assert!(codes(same).contains(&Code::EndBeforeStart));
}

#[test]
fn all_day_duration_may_not_carry_a_time_part() {
    let ok = event(json!({"start": date("2026-11-02"), "duration": "P2D"}));
    assert_eq!(codes(ok), vec![]);
    let ok = event(json!({"start": date("2026-11-02"), "duration": "P1W"}));
    assert_eq!(codes(ok), vec![]);
    let bad = event(json!({"start": date("2026-11-02"), "duration": "PT1H"}));
    assert!(codes(bad).contains(&Code::DateDurationHasTime));
}

#[test]
fn end_type_must_match_start_family() {
    let c = codes(event(
        json!({"start": date("2026-11-02"), "end": utc("2026-11-03T00:00:00Z")}),
    ));
    assert!(c.contains(&Code::EndTypeMismatch));
    let c = codes(event(json!({"end": floating("2026-11-02T18:00:00")})));
    assert!(c.contains(&Code::EndTypeMismatch));
    let c = codes(event(
        json!({"start": floating("2026-11-02T09:00:00"), "end": utc("2026-11-02T18:00:00Z")}),
    ));
    assert!(c.contains(&Code::EndTypeMismatch));
}

#[test]
fn floating_start_and_end_are_compared_locally() {
    let ok = event(
        json!({"start": floating("2026-11-02T09:00:00"), "end": floating("2026-11-02T10:00:00")}),
    );
    assert_eq!(codes(ok), vec![]);
    let bad = event(
        json!({"start": floating("2026-11-02T09:00:00"), "end": floating("2026-11-02T08:00:00")}),
    );
    assert!(codes(bad).contains(&Code::EndBeforeStart));
}

#[test]
fn end_is_compared_in_absolute_time_across_zones() {
    // 09:00 Vancouver (PST, UTC-8) is 17:00Z.
    let start = zoned("2026-11-02T09:00:00", "America/Vancouver");
    let ok = event(json!({"start": start, "end": utc("2026-11-02T17:30:00Z")}));
    assert_eq!(codes(ok), vec![]);
    let bad = event(json!({"start": start, "end": utc("2026-11-02T16:00:00Z")}));
    assert!(codes(bad).contains(&Code::EndBeforeStart));
}

#[test]
fn durations_are_validated_but_not_flattened() {
    for bad in ["P1X", "PT", "1H", "P", ""] {
        assert!(
            codes(event(json!({"duration": bad}))).contains(&Code::InvalidDuration),
            "{bad:?}"
        );
    }
    assert!(codes(event(json!({"duration": "-PT1H"}))).contains(&Code::NegativeDuration));
    assert_eq!(codes(event(json!({"duration": "P1DT2H30M"}))), vec![]);
}

#[test]
fn invalid_calendar_values_are_rejected() {
    assert!(codes(event(json!({"start": date("2026-02-30")}))).contains(&Code::InvalidTemporal));
    assert!(codes(event(json!({"start": utc("2026-13-01T00:00:00Z")})))
        .contains(&Code::InvalidTemporal));
    assert!(
        codes(event(json!({"start": floating("2026-11-02T25:00:00")})))
            .contains(&Code::InvalidTemporal)
    );
    // Leap day is valid only in leap years.
    assert_eq!(codes(event(json!({"start": date("2028-02-29")}))), vec![]);
    assert!(codes(event(json!({"start": date("2027-02-29")}))).contains(&Code::InvalidTemporal));
}

#[test]
fn unknown_timezone_is_rejected_unless_defined_by_the_source() {
    let unknown = event(json!({"start": zoned("2026-11-02T09:00:00", "Mars/Olympus")}));
    assert!(codes(unknown).contains(&Code::UnknownTimezone));

    let defined = event(json!({
        "start": zoned("2026-11-02T09:00:00", "Custom/Zone"),
        "timezones": [{"tzid": "Custom/Zone", "vtimezone": "BEGIN:VTIMEZONE\nEND:VTIMEZONE"}]
    }));
    assert_eq!(codes(defined), vec![]);
}

#[test]
fn dst_gap_and_overlap_follow_calendar_rules() {
    let gap: Temporal =
        serde_json::from_value(zoned("2026-03-08T02:30:00", "America/Vancouver")).unwrap();
    // Nonexistent local time: use the offset in force before the transition (UTC-8).
    assert_eq!(
        gap.to_utc().unwrap().unwrap().to_rfc3339(),
        "2026-03-08T10:30:00+00:00"
    );
    let overlap: Temporal =
        serde_json::from_value(zoned("2026-11-01T01:30:00", "America/Vancouver")).unwrap();
    // Ambiguous local time: first occurrence (still PDT, UTC-7).
    assert_eq!(
        overlap.to_utc().unwrap().unwrap().to_rfc3339(),
        "2026-11-01T08:30:00+00:00"
    );
}

#[test]
fn recurrence_values_must_share_the_start_kind_and_zone() {
    let start = zoned("2026-11-02T09:00:00", "America/Vancouver");
    let mk = |exdate: Value| {
        event(json!({"start": start, "recurrence": {"rrule": "FREQ=DAILY", "exdates": [exdate]}}))
    };
    assert_eq!(
        codes(mk(zoned("2026-11-03T09:00:00", "America/Vancouver"))),
        vec![]
    );
    assert!(codes(mk(date("2026-11-03"))).contains(&Code::RecurrenceTypeMismatch));
    assert!(codes(mk(utc("2026-11-03T17:00:00Z"))).contains(&Code::RecurrenceTypeMismatch));
    assert!(codes(mk(zoned("2026-11-03T09:00:00", "Europe/Paris")))
        .contains(&Code::RecurrenceTypeMismatch));

    let all_day = event(json!({
        "start": date("2026-11-02"),
        "recurrence": {"rrule": "FREQ=DAILY", "rdates": [date("2026-11-09"), zoned("2026-11-10T09:00:00", "America/Vancouver")]}
    }));
    assert!(codes(all_day).contains(&Code::RecurrenceTypeMismatch));
}

#[test]
fn rdate_period_keeps_its_own_length_and_is_checked() {
    let period = |end: Value| {
        event(json!({
            "start": utc("2026-11-02T17:00:00Z"),
            "recurrence": {"rdates": [{"kind": "period", "start": utc("2026-11-05T17:00:00Z"), "end": end}]}
        }))
    };
    assert_eq!(codes(period(utc("2026-11-05T18:00:00Z"))), vec![]);
    assert!(codes(period(utc("2026-11-05T16:00:00Z"))).contains(&Code::EndBeforeStart));
}

#[test]
fn exceptions_are_located_by_original_recurrence_id() {
    let start = zoned("2026-11-02T09:00:00", "America/Vancouver");
    let exc = |rid: &str, change: Value| json!({"recurrence_id": zoned(rid, "America/Vancouver"), "range": "single", "change": change});
    let cancelled = json!({"kind": "cancelled"});
    let dup = event(json!({
        "start": start,
        "recurrence": {"rrule": "FREQ=DAILY", "exceptions": [
            exc("2026-11-03T09:00:00", cancelled.clone()),
            exc("2026-11-03T09:00:00", cancelled.clone())
        ]}
    }));
    assert!(codes(dup).contains(&Code::DuplicateException));

    let distinct = event(json!({
        "start": start,
        "recurrence": {"rrule": "FREQ=DAILY", "exceptions": [
            exc("2026-11-03T09:00:00", cancelled.clone()),
            exc("2026-11-04T09:00:00", cancelled)
        ]}
    }));
    assert_eq!(codes(distinct), vec![]);
}

#[test]
fn rescheduled_exception_is_located_by_original_time_not_new_start() {
    let start = zoned("2026-11-02T09:00:00", "America/Vancouver");
    let ev = event(json!({
        "start": start,
        "recurrence": {"rrule": "FREQ=DAILY", "exceptions": [
            {"recurrence_id": start, "range": "single",
             "change": {"kind": "override", "patch": {"start": zoned("2026-11-02T11:00:00", "America/Vancouver")}}}
        ]}
    }));
    assert_eq!(codes(ev), vec![]);
}

#[test]
fn override_patch_is_validated_like_an_event() {
    let start = utc("2026-11-02T17:00:00Z");
    let exc = |patch: Value| {
        event(json!({
            "start": start,
            "recurrence": {"rrule": "FREQ=DAILY", "exceptions": [
                {"recurrence_id": start, "range": "single", "change": {"kind": "override", "patch": patch}}
            ]}
        }))
    };
    let both = exc(json!({"end": utc("2026-11-02T18:00:00Z"), "duration": "PT1H"}));
    assert!(codes(both).contains(&Code::EndAndDurationExclusive));
    // Explicit null clears an optional field and is allowed.
    assert_eq!(
        codes(exc(json!({"description": null, "end": null}))),
        vec![]
    );
    assert!(codes(exc(json!({"duration": "-PT1H"}))).contains(&Code::NegativeDuration));
}

#[test]
fn this_and_future_is_explicitly_unsupported() {
    let start = utc("2026-11-02T17:00:00Z");
    let ev = event(json!({
        "start": start,
        "recurrence": {"rrule": "FREQ=DAILY", "exceptions": [
            {"recurrence_id": start, "range": "this_and_future", "change": {"kind": "cancelled"}}
        ]}
    }));
    assert!(codes(ev).contains(&Code::UnsupportedRange));
}

#[test]
fn malformed_or_unsupported_rrule_is_rejected() {
    let rr = |s: &str| event(json!({"recurrence": {"rrule": s}}));
    assert!(codes(rr("COUNT=3")).contains(&Code::UnsupportedRrule));
    assert!(codes(rr("FREQ=FORTNIGHTLY")).contains(&Code::UnsupportedRrule));
    assert!(
        codes(rr("FREQ=DAILY;COUNT=3;UNTIL=20261231T000000Z")).contains(&Code::UnsupportedRrule)
    );
    assert_eq!(codes(rr("FREQ=MONTHLY;BYMONTHDAY=31;COUNT=12")), vec![]);
}

#[test]
fn original_recurrence_key_is_canonical() {
    let ev: EventV1 = serde_json::from_value(event(json!({}))).unwrap();
    assert_eq!(ev.original_recurrence_key(None), "single");
    let t: Temporal =
        serde_json::from_value(zoned("2026-11-09T09:00:00", "America/Vancouver")).unwrap();
    assert_eq!(
        t.recurrence_key(),
        "zoned:America/Vancouver:2026-11-09T09:00:00"
    );
    let t: Temporal = serde_json::from_value(date("2026-11-09")).unwrap();
    assert_eq!(t.recurrence_key(), "date:2026-11-09");
    let t: Temporal = serde_json::from_value(utc("2026-11-09T17:00:00Z")).unwrap();
    assert_eq!(t.recurrence_key(), "utc:2026-11-09T17:00:00Z");
    let t: Temporal = serde_json::from_value(floating("2026-11-09T09:00:00")).unwrap();
    assert_eq!(t.recurrence_key(), "floating:2026-11-09T09:00:00");
}

#[test]
fn alarm_actions_other_than_display_are_data_only_but_still_shape_checked() {
    let ev = event(json!({"alarms": [
        {"id": "a", "action": "display",
         "trigger": {"kind": "relative", "related": "start", "offset": "-PT10M"}, "description": "d"}
    ]}));
    assert_eq!(codes(ev), vec![]);
    let bad = event(json!({"alarms": [
        {"id": "a", "action": "display",
         "trigger": {"kind": "relative", "related": "start", "offset": "-PTXM"}, "description": "d"}
    ]}));
    assert!(codes(bad).contains(&Code::InvalidDuration));
}
