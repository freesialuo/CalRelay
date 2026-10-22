//! Golden evaluation tests for bounded recurrence expansion (Issue #20).

use calendar_domain::{
    expand_occurrences, validate_event, Code, EventV1, ExpandedOccurrence, ExpansionError,
    ExpansionOptions,
};
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{json, Value};

fn utc_dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn event_with(extra: Value) -> EventV1 {
    let mut base = json!({
        "schema_version": "1.0",
        "external_uid": "rec-golden-1",
        "title": "Recurring review",
        "status": "confirmed",
        "start": {"kind": "utc", "value": "2026-01-31T09:00:00Z"},
        "duration": "PT1H"
    });
    for (k, v) in extra.as_object().unwrap() {
        base[k] = v.clone();
    }
    serde_json::from_value(base).expect("event fixture must deserialize")
}

fn default_options(window_start: &str, window_end: &str) -> ExpansionOptions<'static> {
    ExpansionOptions {
        window_start: utc_dt(window_start),
        window_end: utc_dt(window_end),
        calendar_tzid: None,
        max_occurrences: 64,
        max_iterations: 512,
    }
}

#[test]
fn horizon_overflow_returns_invalid_window() {
    let event = event_with(json!({
        "recurrence": {"rrule": "FREQ=DAILY;COUNT=1"}
    }));
    let mut options = default_options("2026-01-01T00:00:00Z", "2026-02-01T00:00:00Z");
    options.window_end = DateTime::<Utc>::MAX_UTC;
    assert_eq!(
        expand_occurrences(&event, &options),
        Err(ExpansionError::InvalidWindow)
    );
    options.window_end = DateTime::<Utc>::MAX_UTC - chrono::Duration::days(1);
    assert_eq!(
        expand_occurrences(&event, &options),
        Err(ExpansionError::InvalidWindow)
    );
    options.window_end = DateTime::<Utc>::MAX_UTC - chrono::Duration::days(2);
    assert_eq!(expand_occurrences(&event, &options).unwrap().len(), 1);
}

#[test]
fn unrepresentable_candidates_fail_before_invalid_date_skipping() {
    for rule in [
        "FREQ=DAILY;INTERVAL=4294967295;COUNT=2",
        "FREQ=WEEKLY;INTERVAL=4294967295;COUNT=2",
        "FREQ=MONTHLY;INTERVAL=4294967295;COUNT=2",
        "FREQ=YEARLY;INTERVAL=1000000;COUNT=2",
    ] {
        let event = event_with(json!({"recurrence": {"rrule": rule}}));
        validate_event(&event).unwrap();
        assert_eq!(
            expand_occurrences(
                &event,
                &default_options("2026-01-01T00:00:00Z", "2026-02-01T00:00:00Z")
            ),
            Err(ExpansionError::InvalidWindow),
            "{rule}"
        );
    }
}

#[test]
fn rdates_share_the_rrule_iteration_budget_even_when_duplicated_or_excluded() {
    for rule in [None, Some("FREQ=DAILY;COUNT=1")] {
        let start = json!({"kind": "utc", "value": "2026-01-31T09:00:00Z"});
        let event = event_with(json!({
            "recurrence": {
                "rrule": rule,
                "rdates": [start.clone(), {"kind": "period", "start": start, "duration": "PT2H"}],
                "exdates": [start]
            }
        }));
        validate_event(&event).unwrap();
        let mut options = default_options("2026-01-01T00:00:00Z", "2026-02-01T00:00:00Z");
        let required_steps = if rule.is_some() { 3 } else { 2 };
        options.max_iterations = required_steps;
        assert!(expand_occurrences(&event, &options).unwrap().is_empty());
        options.max_iterations = required_steps - 1;
        assert_eq!(
            expand_occurrences(&event, &options),
            Err(ExpansionError::IterationLimitExceeded {
                max_iterations: required_steps - 1
            })
        );
    }
}

fn summary(items: &[ExpandedOccurrence]) -> Vec<(String, String, String, bool)> {
    items
        .iter()
        .map(|o| {
            (
                o.original_key.clone(),
                o.start_utc.to_rfc3339(),
                o.end_utc.to_rfc3339(),
                o.cancelled,
            )
        })
        .collect()
}

#[test]
fn merged_all_day_period_ends_are_exclusive_while_timed_ends_may_equal_start() {
    for (start, end, moved, valid) in [
        (
            json!({"kind": "date", "date": "2026-02-01"}),
            json!({"kind": "date", "date": "2026-02-02"}),
            json!({"kind": "date", "date": "2026-02-02"}),
            false,
        ),
        (
            json!({"kind": "date", "date": "2026-02-01"}),
            json!({"kind": "date", "date": "2026-02-02"}),
            json!({"kind": "date", "date": "2026-02-03"}),
            false,
        ),
        (
            json!({"kind": "date", "date": "2026-02-01"}),
            json!({"kind": "date", "date": "2026-02-02"}),
            json!({"kind": "date", "date": "2026-01-31"}),
            true,
        ),
        (
            json!({"kind": "utc", "value": "2026-02-01T09:00:00Z"}),
            json!({"kind": "utc", "value": "2026-02-01T11:00:00Z"}),
            json!({"kind": "utc", "value": "2026-02-01T11:00:00Z"}),
            true,
        ),
        (
            json!({"kind": "floating", "local": "2026-02-01T09:00:00"}),
            json!({"kind": "floating", "local": "2026-02-01T11:00:00"}),
            json!({"kind": "floating", "local": "2026-02-01T11:00:00"}),
            true,
        ),
        (
            json!({"kind": "zoned", "local": "2026-02-01T09:00:00", "tzid": "UTC"}),
            json!({"kind": "zoned", "local": "2026-02-01T11:00:00", "tzid": "UTC"}),
            json!({"kind": "zoned", "local": "2026-02-01T11:00:00", "tzid": "UTC"}),
            true,
        ),
    ] {
        let event = event_with(json!({
            "start": start, "duration": null,
            "recurrence": {
                "rdates": [{"kind": "period", "start": start, "end": end}],
                "exceptions": [{
                    "recurrence_id": start, "range": "single",
                    "change": {"kind": "override", "patch": {"start": moved}}
                }]
            }
        }));
        validate_event(&event).unwrap();
        let mut options = default_options("2026-01-30T00:00:00Z", "2026-02-04T00:00:00Z");
        options.calendar_tzid = Some("UTC");
        let result = expand_occurrences(&event, &options);
        if valid {
            let out = result.unwrap();
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].start, serde_json::from_value(moved).unwrap());
            assert_eq!(out[0].original_key, event.start.recurrence_key());
            if matches!(out[0].start, calendar_domain::Temporal::Date { .. }) {
                assert_eq!(out[0].end_utc, utc_dt("2026-02-02T00:00:00Z"));
            } else {
                assert_eq!(out[0].start_utc, out[0].end_utc);
            }
        } else {
            assert_eq!(result, Err(ExpansionError::InvalidDuration), "{moved}");
        }
    }
}

#[test]
fn combined_period_and_exception_span_types_are_revalidated() {
    let utc_start = json!({"kind": "utc", "value": "2026-02-01T09:00:00Z"});
    let utc_end = json!({"kind": "utc", "value": "2026-02-01T11:00:00Z"});
    let floating_start = json!({"kind": "floating", "local": "2026-02-01T09:00:00"});
    let floating_end = json!({"kind": "floating", "local": "2026-02-01T11:00:00"});
    let date = json!({"kind": "date", "date": "2026-02-01"});
    for (start, period_span, moved, expected) in [
        (
            utc_start.clone(),
            json!({"end": utc_end}),
            date.clone(),
            Code::EndTypeMismatch,
        ),
        (
            utc_start.clone(),
            json!({"duration": "PT2H"}),
            date.clone(),
            Code::DateDurationHasTime,
        ),
        (
            utc_start.clone(),
            json!({"duration": "P1DT0H"}),
            date.clone(),
            Code::DateDurationHasTime,
        ),
        (
            utc_start.clone(),
            json!({"end": utc_end}),
            floating_start.clone(),
            Code::EndTypeMismatch,
        ),
        (
            floating_start.clone(),
            json!({"end": floating_end}),
            utc_start.clone(),
            Code::EndTypeMismatch,
        ),
        (
            date.clone(),
            json!({"end": {"kind": "date", "date": "2026-02-02"}}),
            utc_start.clone(),
            Code::EndTypeMismatch,
        ),
    ] {
        let mut period = period_span;
        period["kind"] = json!("period");
        period["start"] = start.clone();
        let event = event_with(json!({
            "start": start,
            "duration": null,
            "recurrence": {
                "rdates": [period],
                "exceptions": [{
                    "recurrence_id": start, "range": "single",
                    "change": {"kind": "override", "patch": {"start": moved}}
                }]
            }
        }));
        validate_event(&event).unwrap();
        let mut options = default_options("2026-02-01T00:00:00Z", "2026-02-03T00:00:00Z");
        options.calendar_tzid = Some("UTC");
        match expand_occurrences(&event, &options) {
            Err(ExpansionError::Validation(errors)) => {
                assert!(errors.iter().any(|e| e.code == expected), "{errors:?}")
            }
            result => panic!("expected {expected:?} for {moved}, got {result:?}"),
        }
    }
}

#[test]
fn compatible_period_span_type_changes_and_explicit_clears_remain_valid() {
    let start = json!({"kind": "utc", "value": "2026-02-01T09:00:00Z"});
    let end = json!({"kind": "utc", "value": "2026-02-01T11:00:00Z"});
    let date = json!({"kind": "date", "date": "2026-02-01"});
    for (span, patch, expected_end) in [
        (
            json!({"duration": "P1D"}),
            json!({"start": date}),
            "2026-02-02T00:00:00Z",
        ),
        (
            json!({"duration": "PT2H"}),
            json!({"start": date, "duration": null}),
            "2026-02-02T00:00:00Z",
        ),
        (
            json!({"end": end}),
            json!({"start": date, "end": null}),
            "2026-02-02T00:00:00Z",
        ),
        (
            json!({"end": end}),
            json!({"start": date, "end": {"kind": "date", "date": "2026-02-02"}}),
            "2026-02-02T00:00:00Z",
        ),
        (
            json!({"end": end}),
            json!({"start": {"kind": "zoned", "local": "2026-02-01T09:00:00", "tzid": "UTC"}}),
            "2026-02-01T11:00:00Z",
        ),
    ] {
        let mut period = span;
        period["kind"] = json!("period");
        period["start"] = start.clone();
        let event = event_with(json!({
            "start": start, "duration": null,
            "recurrence": {
                "rdates": [period],
                "exceptions": [{
                    "recurrence_id": start, "range": "single",
                    "change": {"kind": "override", "patch": patch}
                }]
            }
        }));
        validate_event(&event).unwrap();
        let mut options = default_options("2026-02-01T00:00:00Z", "2026-02-03T00:00:00Z");
        options.calendar_tzid = Some("UTC");
        let out = expand_occurrences(&event, &options).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].end_utc, utc_dt(expected_end), "{patch}");
        assert_eq!(out[0].original_key, "utc:2026-02-01T09:00:00Z");
    }
}

#[test]
fn ac1_month_end_and_leap_day_skip_invalid_dates_without_drift_or_count_loss() {
    // RFC 5545 Section 3.3.10: invalid calendar dates (Feb 31, Apr 31) are ignored and not counted toward COUNT.
    let monthly_end = event_with(json!({
        "start": {"kind": "utc", "value": "2026-01-31T09:00:00Z"},
        "duration": "PT1H",
        "recurrence": {"rrule": "FREQ=MONTHLY;BYMONTHDAY=31;COUNT=4"}
    }));
    let out = expand_occurrences(
        &monthly_end,
        &default_options("2026-01-01T00:00:00Z", "2026-12-31T23:59:59Z"),
    )
    .unwrap();
    assert_eq!(
        summary(&out),
        vec![
            (
                "utc:2026-01-31T09:00:00Z".to_owned(),
                "2026-01-31T09:00:00+00:00".to_owned(),
                "2026-01-31T10:00:00+00:00".to_owned(),
                false,
            ),
            (
                "utc:2026-03-31T09:00:00Z".to_owned(),
                "2026-03-31T09:00:00+00:00".to_owned(),
                "2026-03-31T10:00:00+00:00".to_owned(),
                false,
            ),
            (
                "utc:2026-05-31T09:00:00Z".to_owned(),
                "2026-05-31T09:00:00+00:00".to_owned(),
                "2026-05-31T10:00:00+00:00".to_owned(),
                false,
            ),
            (
                "utc:2026-07-31T09:00:00Z".to_owned(),
                "2026-07-31T09:00:00+00:00".to_owned(),
                "2026-07-31T10:00:00+00:00".to_owned(),
                false,
            ),
        ]
    );

    // Yearly recurrence starting on a leap day (2024-02-29): skips 2025/2026/2027 without shifting
    // to March 1 (unlike rrule 0.14.0's counter_date::fix_day) and resumes on 2028-02-29.
    let mut leap_json = json!({
        "schema_version": "1.0",
        "external_uid": "rec-golden-leap",
        "title": "Leap anniversary",
        "status": "confirmed",
        "start": {"kind": "date", "date": "2024-02-29"},
        "recurrence": {"rrule": "FREQ=YEARLY;COUNT=2"}
    });
    let leap_yearly: EventV1 = serde_json::from_value(leap_json.take()).unwrap();
    let mut opts = default_options("2024-01-01T00:00:00Z", "2029-01-01T00:00:00Z");
    opts.calendar_tzid = Some("UTC");
    let leap_out = expand_occurrences(&leap_yearly, &opts).unwrap();
    assert_eq!(
        leap_out
            .iter()
            .map(|o| (
                o.original_key.as_str(),
                o.local_date,
                o.end_utc.to_rfc3339()
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                "date:2024-02-29",
                NaiveDate::from_ymd_opt(2024, 2, 29).unwrap(),
                "2024-03-01T00:00:00+00:00".to_owned(),
            ),
            (
                "date:2028-02-29",
                NaiveDate::from_ymd_opt(2028, 2, 29).unwrap(),
                "2028-03-01T00:00:00+00:00".to_owned(),
            ),
        ]
    );
}

#[test]
fn ac1_count_and_until_boundaries_with_exdate_and_rdate_period() {
    // Daily recurrence bounded by UNTIL, with one EXDATE, one Temporal RDATE, and one RDATE PERIOD
    // carrying its own 2h30m duration.
    let ev = event_with(json!({
        "start": {"kind": "utc", "value": "2026-11-02T17:00:00Z"},
        "duration": "PT1H",
        "recurrence": {
            "rrule": "FREQ=DAILY;UNTIL=20261104T170000Z",
            "exdates": [
                {"kind": "utc", "value": "2026-11-03T17:00:00Z"}
            ],
            "rdates": [
                {"kind": "utc", "value": "2026-11-05T17:00:00Z"},
                {
                    "kind": "period",
                    "start": {"kind": "utc", "value": "2026-11-06T17:00:00Z"},
                    "duration": "PT2H30M"
                }
            ]
        }
    }));
    let out = expand_occurrences(
        &ev,
        &default_options("2026-11-01T00:00:00Z", "2026-11-10T00:00:00Z"),
    )
    .unwrap();
    assert_eq!(
        summary(&out),
        vec![
            (
                "utc:2026-11-02T17:00:00Z".to_owned(),
                "2026-11-02T17:00:00+00:00".to_owned(),
                "2026-11-02T18:00:00+00:00".to_owned(),
                false,
            ),
            (
                "utc:2026-11-04T17:00:00Z".to_owned(),
                "2026-11-04T17:00:00+00:00".to_owned(),
                "2026-11-04T18:00:00+00:00".to_owned(),
                false,
            ),
            (
                "utc:2026-11-05T17:00:00Z".to_owned(),
                "2026-11-05T17:00:00+00:00".to_owned(),
                "2026-11-05T18:00:00+00:00".to_owned(),
                false,
            ),
            (
                "utc:2026-11-06T17:00:00Z".to_owned(),
                "2026-11-06T17:00:00+00:00".to_owned(),
                "2026-11-06T19:30:00+00:00".to_owned(),
                false,
            ),
        ]
    );
}

const SHIPPED_EXAMPLE: &str = include_str!("../../../examples/event-v1.json");

#[test]
fn ac1_exceptions_cancel_or_reschedule_by_original_recurrence_key() {
    // Shipped repository example: FREQ=WEEKLY;BYDAY=MO;COUNT=8 with 1 EXDATE (2026-11-16)
    // and 1 rescheduled override (2026-11-09 -> 2026-11-10T10:00:00).
    let example: EventV1 = serde_json::from_str(SHIPPED_EXAMPLE).unwrap();
    let example_out = expand_occurrences(
        &example,
        &default_options("2026-11-01T00:00:00Z", "2027-01-01T00:00:00Z"),
    )
    .unwrap();
    assert_eq!(example_out.len(), 7);
    assert_eq!(
        example_out[1].original_key,
        "zoned:America/Vancouver:2026-11-09T09:00:00"
    );
    assert_eq!(
        example_out[1].start_utc.to_rfc3339(),
        "2026-11-10T18:00:00+00:00"
    );
    assert_eq!(
        example_out[1].end_utc.to_rfc3339(),
        "2026-11-10T18:30:00+00:00"
    );
    assert_eq!(example_out[1].title, "本周复盘（改期）");
    assert!(!example_out
        .iter()
        .any(|o| o.original_key == "zoned:America/Vancouver:2026-11-16T09:00:00"));
    // 3 daily occurrences:
    // - 2026-11-02: unchanged
    // - 2026-11-03: cancelled exception (retains original_key with cancelled = true)
    // - 2026-11-04: overridden to start 2h later with 90m duration and new title, retaining original_key!
    let ev = event_with(json!({
        "start": {"kind": "zoned", "local": "2026-11-02T09:00:00", "tzid": "America/Vancouver"},
        "duration": "PT1H",
        "recurrence": {
            "rrule": "FREQ=DAILY;COUNT=3",
            "exceptions": [
                {
                    "recurrence_id": {"kind": "zoned", "local": "2026-11-03T09:00:00", "tzid": "America/Vancouver"},
                    "range": "single",
                    "change": {"kind": "cancelled"}
                },
                {
                    "recurrence_id": {"kind": "zoned", "local": "2026-11-04T09:00:00", "tzid": "America/Vancouver"},
                    "range": "single",
                    "change": {
                        "kind": "override",
                        "patch": {
                            "title": "Moved standup",
                            "start": {"kind": "zoned", "local": "2026-11-04T11:00:00", "tzid": "America/Vancouver"},
                            "duration": "PT1H30M"
                        }
                    }
                }
            ]
        }
    }));
    let out = expand_occurrences(
        &ev,
        &default_options("2026-11-01T00:00:00Z", "2026-11-10T00:00:00Z"),
    )
    .unwrap();
    assert_eq!(out.len(), 3);
    assert_eq!(
        summary(&out),
        vec![
            (
                "zoned:America/Vancouver:2026-11-02T09:00:00".to_owned(),
                "2026-11-02T17:00:00+00:00".to_owned(),
                "2026-11-02T18:00:00+00:00".to_owned(),
                false,
            ),
            (
                "zoned:America/Vancouver:2026-11-03T09:00:00".to_owned(),
                "2026-11-03T17:00:00+00:00".to_owned(),
                "2026-11-03T18:00:00+00:00".to_owned(),
                true,
            ),
            (
                "zoned:America/Vancouver:2026-11-04T09:00:00".to_owned(),
                "2026-11-04T19:00:00+00:00".to_owned(),
                "2026-11-04T20:30:00+00:00".to_owned(),
                false,
            ),
        ]
    );
    assert_eq!(out[2].title, "Moved standup");
}

#[test]
fn ac2_dst_transitions_and_timezone_bindings_preserve_local_wall_clock() {
    // 1. America/Vancouver spring-forward gap on 2026-03-08 (02:00 -> 03:00, UTC-8 -> UTC-7):
    // Daily at 02:30 keeps local wall-clock 02:30; on 2026-03-08 02:30 is nonexistent and resolves
    // using pre-transition offset UTC-8 -> 10:30Z, while 2026-03-09 02:30 uses PDT UTC-7 -> 09:30Z.
    let gap_daily = event_with(json!({
        "start": {"kind": "zoned", "local": "2026-03-07T02:30:00", "tzid": "America/Vancouver"},
        "duration": "PT30M",
        "recurrence": {"rrule": "FREQ=DAILY;COUNT=3"}
    }));
    let gap_out = expand_occurrences(
        &gap_daily,
        &default_options("2026-03-07T00:00:00Z", "2026-03-11T00:00:00Z"),
    )
    .unwrap();
    assert_eq!(
        summary(&gap_out),
        vec![
            (
                "zoned:America/Vancouver:2026-03-07T02:30:00".to_owned(),
                "2026-03-07T10:30:00+00:00".to_owned(),
                "2026-03-07T11:00:00+00:00".to_owned(),
                false,
            ),
            (
                "zoned:America/Vancouver:2026-03-08T02:30:00".to_owned(),
                "2026-03-08T10:30:00+00:00".to_owned(),
                "2026-03-08T11:00:00+00:00".to_owned(),
                false,
            ),
            (
                "zoned:America/Vancouver:2026-03-09T02:30:00".to_owned(),
                "2026-03-09T09:30:00+00:00".to_owned(),
                "2026-03-09T10:00:00+00:00".to_owned(),
                false,
            ),
        ]
    );

    // 2. America/Havana midnight DST gap on 2026-03-08 (00:00 -> 01:00, UTC-5 -> UTC-4):
    // Unlike rrule 0.14.0 (which drops midnight gap occurrences), 2026-03-08T00:30:00 is preserved.
    let havana_daily = event_with(json!({
        "start": {"kind": "zoned", "local": "2026-03-07T00:30:00", "tzid": "America/Havana"},
        "duration": "PT30M",
        "recurrence": {"rrule": "FREQ=DAILY;COUNT=3"}
    }));
    let havana_out = expand_occurrences(
        &havana_daily,
        &default_options("2026-03-07T00:00:00Z", "2026-03-11T00:00:00Z"),
    )
    .unwrap();
    assert_eq!(
        summary(&havana_out),
        vec![
            (
                "zoned:America/Vancouver:2026-03-07T00:30:00"
                    .replace("America/Vancouver", "America/Havana"),
                "2026-03-07T05:30:00+00:00".to_owned(),
                "2026-03-07T06:00:00+00:00".to_owned(),
                false,
            ),
            (
                "zoned:America/Havana:2026-03-08T00:30:00".to_owned(),
                "2026-03-08T05:30:00+00:00".to_owned(),
                "2026-03-08T06:00:00+00:00".to_owned(),
                false,
            ),
            (
                "zoned:America/Havana:2026-03-09T00:30:00".to_owned(),
                "2026-03-09T04:30:00+00:00".to_owned(),
                "2026-03-09T05:00:00+00:00".to_owned(),
                false,
            ),
        ]
    );

    // 3. Floating events require an explicit calendar_tzid and resolve in that zone across DST;
    // missing calendar_tzid or source VTIMEZONE conflict is explicitly rejected.
    let floating_ev = event_with(json!({
        "start": {"kind": "floating", "local": "2026-10-31T09:00:00"},
        "duration": "PT1H",
        "recurrence": {"rrule": "FREQ=DAILY;COUNT=2"}
    }));
    assert_eq!(
        expand_occurrences(
            &floating_ev,
            &default_options("2026-10-31T00:00:00Z", "2026-11-05T00:00:00Z"),
        ),
        Err(ExpansionError::MissingCalendarTimezone)
    );
    let mut floating_opts = default_options("2026-10-31T00:00:00Z", "2026-11-05T00:00:00Z");
    floating_opts.calendar_tzid = Some("America/Vancouver");
    let floating_out = expand_occurrences(&floating_ev, &floating_opts).unwrap();
    assert_eq!(
        summary(&floating_out),
        vec![
            (
                "floating:2026-10-31T09:00:00".to_owned(),
                "2026-10-31T16:00:00+00:00".to_owned(),
                "2026-10-31T17:00:00+00:00".to_owned(),
                false,
            ),
            (
                "floating:2026-11-01T09:00:00".to_owned(),
                "2026-11-01T17:00:00+00:00".to_owned(),
                "2026-11-01T18:00:00+00:00".to_owned(),
                false,
            ),
        ]
    );

    // Source VTIMEZONE (including same-name conflict) is rejected with UnsupportedTimezone.
    let custom_zone_ev = event_with(json!({
        "start": {"kind": "zoned", "local": "2026-11-02T09:00:00", "tzid": "America/Vancouver"},
        "duration": "PT1H",
        "recurrence": {"rrule": "FREQ=DAILY;COUNT=2"},
        "timezones": [{"tzid": "America/Vancouver", "vtimezone": "BEGIN:VTIMEZONE\nEND:VTIMEZONE"}]
    }));
    assert_eq!(
        expand_occurrences(
            &custom_zone_ev,
            &default_options("2026-11-01T00:00:00Z", "2026-11-10T00:00:00Z"),
        ),
        Err(ExpansionError::UnsupportedTimezone)
    );
}

#[test]
fn ac2_finite_window_and_quantity_or_iteration_budgets_fail_explicitly() {
    let infinite_daily = event_with(json!({
        "start": {"kind": "utc", "value": "2026-11-01T09:00:00Z"},
        "duration": "PT1H",
        "recurrence": {"rrule": "FREQ=DAILY"}
    }));

    // Bounded window [Nov 3, Nov 6) returns only the 3 occurrences inside the window.
    let windowed = expand_occurrences(
        &infinite_daily,
        &default_options("2026-11-03T00:00:00Z", "2026-11-06T00:00:00Z"),
    )
    .unwrap();
    assert_eq!(
        windowed
            .iter()
            .map(|o| o.original_key.as_str())
            .collect::<Vec<_>>(),
        vec![
            "utc:2026-11-03T09:00:00Z",
            "utc:2026-11-04T09:00:00Z",
            "utc:2026-11-05T09:00:00Z",
        ]
    );

    // Exceeding max_occurrences within the window fails explicitly instead of truncating silently.
    let mut small_occ = default_options("2026-11-01T00:00:00Z", "2026-11-10T00:00:00Z");
    small_occ.max_occurrences = 3;
    assert_eq!(
        expand_occurrences(&infinite_daily, &small_occ),
        Err(ExpansionError::OccurrenceLimitExceeded { max_occurrences: 3 })
    );

    // Exceeding max_iterations while stepping toward a far-future window fails explicitly.
    let mut small_iter = default_options("2030-01-01T00:00:00Z", "2030-01-05T00:00:00Z");
    small_iter.max_iterations = 50;
    assert_eq!(
        expand_occurrences(&infinite_daily, &small_iter),
        Err(ExpansionError::IterationLimitExceeded { max_iterations: 50 })
    );
}

#[test]
fn ac3_unverified_rrule_clauses_and_ranges_are_explicitly_rejected_without_widening_validator() {
    // Clauses that pass the minimal schema-boundary `validate_event` syntax check (FREQ present,
    // COUNT and UNTIL not combined) but are outside the verified expansion tier must still pass
    // `validate_event` (do not widen validator claims) while being rejected by `expand_occurrences`.
    for unverified in [
        "FREQ=HOURLY;COUNT=3",
        "FREQ=MINUTELY;COUNT=3",
        "FREQ=SECONDLY;COUNT=3",
        "FREQ=MONTHLY;BYDAY=1MO;COUNT=3",
        "FREQ=MONTHLY;BYMONTHDAY=-1;COUNT=3",
        "FREQ=DAILY;BYSETPOS=1;COUNT=3",
        "FREQ=YEARLY;BYYEARDAY=100;COUNT=3",
        "FREQ=DAILY;INTERVAL=0;COUNT=3",
        "FREQ=DAILY;COUNT=0",
        "FREQ=DAILY;UNTIL=20261105",
        "FREQ=WEEKLY;BYDAY=TU;COUNT=3",
        "FREQ=MONTHLY;BYMONTHDAY=15;COUNT=3",
    ] {
        let ev = event_with(json!({
            "start": {"kind": "utc", "value": "2026-11-02T09:00:00Z"},
            "duration": "PT1H",
            "recurrence": {"rrule": unverified}
        }));
        assert_eq!(
            validate_event(&ev),
            Ok(()),
            "validator boundary must stay unchanged for {unverified}"
        );
        assert!(
            matches!(
                expand_occurrences(
                    &ev,
                    &default_options("2026-11-01T00:00:00Z", "2026-12-01T00:00:00Z"),
                ),
                Err(ExpansionError::UnsupportedRrule(_))
            ),
            "expander must explicitly reject {unverified}"
        );
    }

    // THISANDFUTURE exception range remains rejected at validation and expansion time.
    let this_and_future = event_with(json!({
        "start": {"kind": "utc", "value": "2026-11-02T09:00:00Z"},
        "recurrence": {
            "rrule": "FREQ=DAILY;COUNT=3",
            "exceptions": [{
                "recurrence_id": {"kind": "utc", "value": "2026-11-03T09:00:00Z"},
                "range": "this_and_future",
                "change": {"kind": "cancelled"}
            }]
        }
    }));
    assert!(matches!(
        expand_occurrences(
            &this_and_future,
            &default_options("2026-11-01T00:00:00Z", "2026-11-10T00:00:00Z"),
        ),
        Err(ExpansionError::Validation(errs)) if errs.iter().any(|e| e.code == Code::UnsupportedRange)
    ));
}

#[test]
fn inherited_explicit_ends_preserve_every_instance_span_and_overlap() {
    let event = event_with(json!({
        "start": {"kind": "date", "date": "2026-01-01"},
        "end": {"kind": "date", "date": "2026-01-02"},
        "duration": null,
        "recurrence": {"rrule": "FREQ=DAILY;COUNT=3"}
    }));
    let mut options = default_options("2026-01-01T00:00:00Z", "2026-01-05T00:00:00Z");
    options.calendar_tzid = Some("UTC");
    let out = expand_occurrences(&event, &options).unwrap();
    assert_eq!(out.len(), 3);
    for occurrence in out {
        assert_eq!(
            occurrence.end_utc - occurrence.start_utc,
            chrono::Duration::days(1)
        );
    }

    let event = event_with(json!({
        "start": {"kind": "utc", "value": "2026-01-01T09:00:00Z"},
        "end": {"kind": "utc", "value": "2026-01-03T09:00:00Z"},
        "duration": null,
        "recurrence": {"rrule": "FREQ=DAILY;COUNT=3"}
    }));
    let out = expand_occurrences(
        &event,
        &default_options("2026-01-03T12:00:00Z", "2026-01-03T13:00:00Z"),
    )
    .unwrap();
    assert_eq!(out.len(), 2);
    for occurrence in out {
        assert_eq!(
            occurrence.end_utc - occurrence.start_utc,
            chrono::Duration::days(2)
        );
    }
}

#[test]
fn explicit_occurrence_ends_remain_absolute_and_cleared_ends_use_defaults() {
    let event = event_with(json!({
        "start": {"kind": "utc", "value": "2026-01-01T09:00:00Z"},
        "end": {"kind": "utc", "value": "2026-01-05T09:00:00Z"},
        "duration": null,
        "recurrence": {
            "rrule": "FREQ=DAILY;COUNT=3",
            "rdates": [{"kind": "period", "start": {"kind": "utc", "value": "2026-01-04T09:00:00Z"}, "end": {"kind": "utc", "value": "2026-01-04T11:00:00Z"}}],
            "exceptions": [
                {"recurrence_id": {"kind": "utc", "value": "2026-01-02T09:00:00Z"}, "range": "single", "change": {"kind": "override", "patch": {"end": {"kind": "utc", "value": "2026-01-02T10:00:00Z"}}}},
                {"recurrence_id": {"kind": "utc", "value": "2026-01-03T09:00:00Z"}, "range": "single", "change": {"kind": "override", "patch": {"end": null}}}
            ]
        }
    }));
    let out = expand_occurrences(
        &event,
        &default_options("2026-01-01T00:00:00Z", "2026-01-10T00:00:00Z"),
    )
    .unwrap();
    assert_eq!(out.len(), 4);
    assert_eq!(out[1].end_utc, utc_dt("2026-01-02T10:00:00Z"));
    assert_eq!(out[2].end_utc, out[2].start_utc);
    assert_eq!(out[3].end_utc, utc_dt("2026-01-04T11:00:00Z"));
}

#[test]
fn yearly_month_days_expand_all_months_unless_restricted() {
    for (rule, expected) in [
        (
            "FREQ=YEARLY;BYMONTHDAY=15;COUNT=3",
            vec![
                "2026-01-15T09:00:00Z",
                "2026-02-15T09:00:00Z",
                "2026-03-15T09:00:00Z",
            ],
        ),
        (
            "FREQ=YEARLY;BYMONTHDAY=15;BYMONTH=1,3;COUNT=3",
            vec![
                "2026-01-15T09:00:00Z",
                "2026-03-15T09:00:00Z",
                "2027-01-15T09:00:00Z",
            ],
        ),
        (
            "FREQ=YEARLY;COUNT=3",
            vec![
                "2026-01-15T09:00:00Z",
                "2027-01-15T09:00:00Z",
                "2028-01-15T09:00:00Z",
            ],
        ),
    ] {
        let event = event_with(json!({
            "start": {"kind": "utc", "value": "2026-01-15T09:00:00Z"},
            "recurrence": {"rrule": rule}
        }));
        let out = expand_occurrences(
            &event,
            &default_options("2026-01-01T00:00:00Z", "2029-01-01T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(
            out.iter().map(|o| o.start_utc).collect::<Vec<_>>(),
            expected.into_iter().map(utc_dt).collect::<Vec<_>>(),
            "{rule}"
        );
    }
}

#[test]
fn unrepresentable_durations_return_errors_for_all_span_sources() {
    for duration in [
        "PT2562047788015215H31M",         // numeric accumulation
        "P3000000D",                      // exceeds the Temporal four-digit year range
        "PT9223372036854775807H",         // numeric multiplication
        "PT9223372036854775807S",         // Chrono duration construction
        "PT9223372036854775S",            // datetime addition
        "P18446744073709551615W",         // week multiplication
        "P18446744073709551615D",         // calendar addition
        "PT999999999999999999999999999S", // numeric parsing
    ] {
        for extra in [
            json!({"duration": duration}),
            json!({"duration": duration, "recurrence": {"rrule": "FREQ=DAILY;COUNT=2"}}),
            json!({"recurrence": {"rdates": [{"kind": "period", "start": {"kind": "utc", "value": "2026-02-01T09:00:00Z"}, "duration": duration}]}}),
            json!({"recurrence": {"rrule": "FREQ=DAILY;COUNT=2", "exceptions": [{"recurrence_id": {"kind": "utc", "value": "2026-02-01T09:00:00Z"}, "range": "single", "change": {"kind": "override", "patch": {"duration": duration}}}]}}),
        ] {
            let event = event_with(extra);
            validate_event(&event).unwrap();
            assert_eq!(
                expand_occurrences(
                    &event,
                    &default_options("2026-01-01T00:00:00Z", "2026-03-01T00:00:00Z")
                ),
                Err(ExpansionError::InvalidDuration),
                "{duration}"
            );
        }
    }
}

#[test]
fn inherited_timed_ends_keep_exact_duration_across_dst() {
    let event = event_with(json!({
        "start": {"kind": "zoned", "local": "2026-03-07T09:00:00", "tzid": "America/Vancouver"},
        "end": {"kind": "zoned", "local": "2026-03-09T09:00:00", "tzid": "America/Vancouver"},
        "duration": null,
        "recurrence": {"rrule": "FREQ=DAILY;COUNT=3"}
    }));
    let out = expand_occurrences(
        &event,
        &default_options("2026-03-07T00:00:00Z", "2026-03-12T00:00:00Z"),
    )
    .unwrap();
    assert_eq!(out.len(), 3);
    for occurrence in out {
        assert_eq!(
            occurrence.end_utc - occurrence.start_utc,
            chrono::Duration::hours(47)
        );
    }
}

#[test]
fn generated_years_outside_temporal_range_return_typed_errors() {
    for start in [
        json!({"kind": "utc", "value": "2026-01-31T09:00:00Z"}),
        json!({"kind": "zoned", "local": "2026-01-31T09:00:00", "tzid": "America/Vancouver"}),
        json!({"kind": "floating", "local": "2026-01-31T09:00:00"}),
        json!({"kind": "date", "date": "2026-01-31"}),
    ] {
        for rule in [
            "FREQ=YEARLY;INTERVAL=10000;COUNT=2",
            "FREQ=MONTHLY;INTERVAL=120000;COUNT=2",
            "FREQ=WEEKLY;INTERVAL=600000;COUNT=2",
            "FREQ=DAILY;INTERVAL=4000000;COUNT=2",
        ] {
            let event = event_with(json!({
                "start": start,
                "duration": null,
                "recurrence": {"rrule": rule}
            }));
            validate_event(&event).unwrap();
            let mut options = default_options("2026-01-01T00:00:00Z", "2027-01-01T00:00:00Z");
            options.calendar_tzid = Some("UTC");
            assert_eq!(
                expand_occurrences(&event, &options),
                Err(ExpansionError::InvalidWindow),
                "{start}: {rule}"
            );
        }
    }
}

#[test]
fn recurrence_year_limit_keeps_last_supported_year() {
    let mut event = event_with(json!({
        "start": {"kind": "utc", "value": "9998-01-31T09:00:00Z"},
        "recurrence": {"rrule": "FREQ=YEARLY;COUNT=2"}
    }));
    let options = default_options("9998-01-01T00:00:00Z", "9999-12-01T00:00:00Z");
    let out = expand_occurrences(&event, &options).unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(out[1].start_utc, utc_dt("9999-01-31T09:00:00Z"));
    event.recurrence.as_mut().unwrap().rrule = Some("FREQ=YEARLY;COUNT=3".to_owned());
    assert_eq!(
        expand_occurrences(&event, &options),
        Err(ExpansionError::InvalidWindow)
    );
}

#[test]
fn exception_spans_override_period_spans_without_shadowing() {
    let end = json!({"kind": "utc", "value": "2026-02-01T12:00:00Z"});
    for (period_span, patch, expected_end) in [
        (
            json!({"duration": "PT2H"}),
            json!({"end": end}),
            "2026-02-01T12:00:00Z",
        ),
        (
            json!({"end": end}),
            json!({"duration": "PT2H"}),
            "2026-02-01T11:00:00Z",
        ),
        (
            json!({"duration": "PT2H"}),
            json!({"end": null}),
            "2026-02-01T11:00:00Z",
        ),
        (
            json!({"end": end}),
            json!({"duration": null}),
            "2026-02-01T12:00:00Z",
        ),
        (
            json!({"duration": "PT2H"}),
            json!({"duration": null}),
            "2026-02-01T09:00:00Z",
        ),
        (
            json!({"end": end}),
            json!({"end": null}),
            "2026-02-01T09:00:00Z",
        ),
        (
            json!({"duration": "PT2H"}),
            json!({"title": "Renamed"}),
            "2026-02-01T11:00:00Z",
        ),
    ] {
        let start = json!({"kind": "utc", "value": "2026-02-01T09:00:00Z"});
        let mut period = period_span;
        period["kind"] = json!("period");
        period["start"] = start.clone();
        let event = event_with(json!({
            "duration": null,
            "recurrence": {
                "rdates": [period],
                "exceptions": [{"recurrence_id": start, "range": "single", "change": {"kind": "override", "patch": patch}}]
            }
        }));
        validate_event(&event).unwrap();
        let out = expand_occurrences(
            &event,
            &default_options("2026-02-01T00:00:00Z", "2026-02-02T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].end_utc, utc_dt(expected_end), "{patch}");
    }
}

#[test]
fn calendar_timezone_is_validated_only_for_temporals_that_need_it() {
    for start in [
        json!({"kind": "utc", "value": "2026-01-31T09:00:00Z"}),
        json!({"kind": "zoned", "local": "2026-01-31T09:00:00", "tzid": "America/Vancouver"}),
        json!({"kind": "date", "date": "2026-01-31"}),
        json!({"kind": "floating", "local": "2026-01-31T09:00:00"}),
    ] {
        let event = event_with(json!({
            "start": start,
            "duration": null,
            "timezones": [{"tzid": "Custom/Calendar", "vtimezone": "BEGIN:VTIMEZONE\nEND:VTIMEZONE"}]
        }));
        let needs_calendar = matches!(start["kind"].as_str(), Some("date" | "floating"));
        let mut options = default_options("2026-01-01T00:00:00Z", "2026-02-02T00:00:00Z");
        options.calendar_tzid = Some("UTC");
        let expected = expand_occurrences(&event, &options).unwrap();
        for (tzid, error) in [
            (None, ExpansionError::MissingCalendarTimezone),
            (
                Some("Unknown/Zone"),
                ExpansionError::MissingCalendarTimezone,
            ),
            (Some("Custom/Calendar"), ExpansionError::UnsupportedTimezone),
        ] {
            options.calendar_tzid = tzid;
            let out = expand_occurrences(&event, &options);
            if needs_calendar {
                assert_eq!(out, Err(error), "{start}: {tzid:?}");
            } else {
                assert_eq!(out, Ok(expected.clone()), "{start}: {tzid:?}");
            }
        }
    }
}

#[test]
fn rescheduled_period_spans_are_checked_after_combining_overrides() {
    for (start, end, moved, valid, expected) in [
        (
            json!({"kind": "utc", "value": "2026-02-01T09:00:00Z"}),
            json!({"kind": "utc", "value": "2026-02-01T11:00:00Z"}),
            json!({"kind": "utc", "value": "2026-02-01T12:00:00Z"}),
            false,
            "2026-02-01T11:00:00Z",
        ),
        (
            json!({"kind": "floating", "local": "2026-02-01T09:00:00"}),
            json!({"kind": "floating", "local": "2026-02-01T11:00:00"}),
            json!({"kind": "floating", "local": "2026-02-01T12:00:00"}),
            false,
            "2026-02-01T11:00:00Z",
        ),
        (
            json!({"kind": "zoned", "local": "2026-02-01T09:00:00", "tzid": "America/Vancouver"}),
            json!({"kind": "zoned", "local": "2026-02-01T11:00:00", "tzid": "America/Vancouver"}),
            json!({"kind": "zoned", "local": "2026-02-01T12:00:00", "tzid": "America/Vancouver"}),
            false,
            "2026-02-01T19:00:00Z",
        ),
        (
            json!({"kind": "utc", "value": "2026-02-01T09:00:00Z"}),
            json!({"kind": "utc", "value": "2026-02-01T11:00:00Z"}),
            json!({"kind": "utc", "value": "2026-02-01T10:00:00Z"}),
            true,
            "2026-02-01T11:00:00Z",
        ),
        (
            json!({"kind": "utc", "value": "2026-02-01T09:00:00Z"}),
            json!({"kind": "utc", "value": "2026-02-01T11:00:00Z"}),
            json!({"kind": "utc", "value": "2026-02-01T11:00:00Z"}),
            true,
            "2026-02-01T11:00:00Z",
        ),
    ] {
        let event = event_with(json!({
            "start": start,
            "duration": null,
            "recurrence": {
                "rdates": [{"kind": "period", "start": start, "end": end}],
                "exceptions": [{"recurrence_id": start, "range": "single", "change": {"kind": "override", "patch": {"start": moved}}}]
            }
        }));
        validate_event(&event).unwrap();
        let mut options = default_options("2026-02-01T00:00:00Z", "2026-02-02T00:00:00Z");
        options.calendar_tzid = Some("UTC");
        let result = expand_occurrences(&event, &options);
        if valid {
            let out = result.unwrap();
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].end_utc, utc_dt(expected));
        } else {
            assert_eq!(result, Err(ExpansionError::InvalidDuration), "{moved}");
        }
    }
}
