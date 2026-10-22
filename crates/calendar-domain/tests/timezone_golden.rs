//! Golden compatibility tests for IANA time zones and source VTIMEZONE boundaries (Issue #19).

use calendar_domain::{validate_event, Code, EventV1, Temporal, Timezone, TZDB_VERSION};
use serde_json::{json, Value};

fn zoned(local: &str, tzid: &str) -> Temporal {
    serde_json::from_value(json!({"kind": "zoned", "local": local, "tzid": tzid})).unwrap()
}

fn utc(value: &str) -> Temporal {
    serde_json::from_value(json!({"kind": "utc", "value": value})).unwrap()
}

fn date(d: &str) -> Temporal {
    serde_json::from_value(json!({"kind": "date", "date": d})).unwrap()
}

fn floating(local: &str) -> Temporal {
    serde_json::from_value(json!({"kind": "floating", "local": local})).unwrap()
}

fn event_with(extra: Value) -> EventV1 {
    let mut base = json!({
        "schema_version": "1.0",
        "external_uid": "tz-golden-1",
        "title": "Timezone check",
        "status": "confirmed",
        "start": {"kind": "utc", "value": "2026-11-02T17:00:00Z"}
    });
    for (k, v) in extra.as_object().unwrap() {
        base[k] = v.clone();
    }
    serde_json::from_value(base).expect("event fixture must deserialize")
}

fn assert_zoned_utc(local: &str, tzid: &str, expected_utc: &str) {
    let temporal = zoned(local, tzid);
    let resolved = temporal
        .to_utc()
        .unwrap_or_else(|e| panic!("{tzid} {local} failed with {e:?}"))
        .unwrap_or_else(|| panic!("{tzid} {local} returned None"));
    assert_eq!(
        resolved.to_rfc3339(),
        expected_utc,
        "unexpected UTC instant for {tzid} {local}"
    );
}

#[test]
fn ac1_dst_gaps_and_overlaps_across_hemispheres_and_midnight_transitions() {
    // Northern Hemisphere (America/Vancouver): spring-forward gap 2026-03-08 02:00 -> 03:00 (UTC-8 -> UTC-7).
    assert_zoned_utc(
        "2026-03-08T01:59:59",
        "America/Vancouver",
        "2026-03-08T09:59:59+00:00",
    );
    assert_zoned_utc(
        "2026-03-08T02:00:00",
        "America/Vancouver",
        "2026-03-08T10:00:00+00:00",
    );
    assert_zoned_utc(
        "2026-03-08T02:30:00",
        "America/Vancouver",
        "2026-03-08T10:30:00+00:00",
    );
    assert_zoned_utc(
        "2026-03-08T03:00:00",
        "America/Vancouver",
        "2026-03-08T10:00:00+00:00",
    );

    // Span ordering across a spring-forward gap: 02:30 (10:30Z) to 03:15 (10:15Z) is inverted in UTC.
    let inverted_gap_span = event_with(json!({
        "start": {"kind": "zoned", "local": "2026-03-08T02:30:00", "tzid": "America/Vancouver"},
        "end": {"kind": "zoned", "local": "2026-03-08T03:15:00", "tzid": "America/Vancouver"}
    }));
    let errs = validate_event(&inverted_gap_span).unwrap_err();
    assert!(errs
        .iter()
        .any(|e| e.code == Code::EndBeforeStart && e.path == "/end"));

    let valid_gap_span = event_with(json!({
        "start": {"kind": "zoned", "local": "2026-03-08T02:30:00", "tzid": "America/Vancouver"},
        "end": {"kind": "zoned", "local": "2026-03-08T03:30:00", "tzid": "America/Vancouver"}
    }));
    assert_eq!(validate_event(&valid_gap_span), Ok(()));

    // Northern Hemisphere (America/Vancouver): fall-back overlap 2026-11-01 01:00..02:00 (UTC-7 -> UTC-8).
    assert_zoned_utc(
        "2026-11-01T00:59:59",
        "America/Vancouver",
        "2026-11-01T07:59:59+00:00",
    );
    assert_zoned_utc(
        "2026-11-01T01:00:00",
        "America/Vancouver",
        "2026-11-01T08:00:00+00:00",
    );
    assert_zoned_utc(
        "2026-11-01T01:30:00",
        "America/Vancouver",
        "2026-11-01T08:30:00+00:00",
    );
    assert_zoned_utc(
        "2026-11-01T02:00:00",
        "America/Vancouver",
        "2026-11-01T10:00:00+00:00",
    );

    // Southern Hemisphere (Australia/Sydney): spring-forward 2026-10-04 02:00 -> 03:00 (UTC+10 -> UTC+11),
    // fall-back 2026-04-05 03:00 -> 02:00 (UTC+11 -> UTC+10).
    assert_zoned_utc(
        "2026-10-04T01:59:59",
        "Australia/Sydney",
        "2026-10-03T15:59:59+00:00",
    );
    assert_zoned_utc(
        "2026-10-04T02:30:00",
        "Australia/Sydney",
        "2026-10-03T16:30:00+00:00",
    );
    assert_zoned_utc(
        "2026-10-04T03:00:00",
        "Australia/Sydney",
        "2026-10-03T16:00:00+00:00",
    );
    assert_zoned_utc(
        "2026-04-05T02:30:00",
        "Australia/Sydney",
        "2026-04-04T15:30:00+00:00",
    );
    assert_zoned_utc(
        "2026-04-05T03:00:00",
        "Australia/Sydney",
        "2026-04-04T17:00:00+00:00",
    );

    // Midnight DST transitions (America/Havana): spring-forward 2026-03-08 00:00 -> 01:00 (UTC-5 -> UTC-4),
    // fall-back 2026-11-01 01:00 -> 00:00 (UTC-4 -> UTC-5).
    assert_zoned_utc(
        "2026-03-07T23:59:59",
        "America/Havana",
        "2026-03-08T04:59:59+00:00",
    );
    assert_zoned_utc(
        "2026-03-08T00:30:00",
        "America/Havana",
        "2026-03-08T05:30:00+00:00",
    );
    assert_zoned_utc(
        "2026-03-08T01:00:00",
        "America/Havana",
        "2026-03-08T05:00:00+00:00",
    );
    assert_zoned_utc(
        "2026-11-01T00:30:00",
        "America/Havana",
        "2026-11-01T04:30:00+00:00",
    );
    assert_zoned_utc(
        "2026-11-01T01:00:00",
        "America/Havana",
        "2026-11-01T06:00:00+00:00",
    );
}

#[test]
fn ac1_non_whole_hour_offsets_and_half_hour_dst_transitions() {
    // 45-minute fixed offsets: Asia/Kathmandu (UTC+05:45), Australia/Eucla (UTC+08:45).
    assert_zoned_utc(
        "2026-11-02T09:15:00",
        "Asia/Kathmandu",
        "2026-11-02T03:30:00+00:00",
    );
    assert_zoned_utc(
        "2026-11-02T08:15:00",
        "Australia/Eucla",
        "2026-11-01T23:30:00+00:00",
    );

    // Cross-zone span comparison between two 45-minute offset zones:
    // 2026-11-02T09:15:00+05:45 (03:30Z) vs 2026-11-02T12:00:00+08:45 (03:15Z, before start)
    // and 2026-11-02T12:30:00+08:45 (03:45Z, after start).
    let bad_cross_zone = event_with(json!({
        "start": {"kind": "zoned", "local": "2026-11-02T09:15:00", "tzid": "Asia/Kathmandu"},
        "end": {"kind": "zoned", "local": "2026-11-02T12:00:00", "tzid": "Australia/Eucla"}
    }));
    let errs = validate_event(&bad_cross_zone).unwrap_err();
    assert!(errs
        .iter()
        .any(|e| e.code == Code::EndBeforeStart && e.path == "/end"));

    let ok_cross_zone = event_with(json!({
        "start": {"kind": "zoned", "local": "2026-11-02T09:15:00", "tzid": "Asia/Kathmandu"},
        "end": {"kind": "zoned", "local": "2026-11-02T12:30:00", "tzid": "Australia/Eucla"}
    }));
    assert_eq!(validate_event(&ok_cross_zone), Ok(()));

    // Australia/Lord_Howe: 30-minute DST shift between UTC+10:30 and UTC+11:00.
    // Spring-forward 2026-10-04 02:00 -> 02:30; fall-back 2026-04-05 02:00 -> 01:30.
    assert_zoned_utc(
        "2026-10-04T01:45:00",
        "Australia/Lord_Howe",
        "2026-10-03T15:15:00+00:00",
    );
    assert_zoned_utc(
        "2026-10-04T02:15:00",
        "Australia/Lord_Howe",
        "2026-10-03T15:45:00+00:00",
    );
    assert_zoned_utc(
        "2026-10-04T02:30:00",
        "Australia/Lord_Howe",
        "2026-10-03T15:30:00+00:00",
    );
    assert_zoned_utc(
        "2026-04-05T01:45:00",
        "Australia/Lord_Howe",
        "2026-04-04T14:45:00+00:00",
    );
    assert_zoned_utc(
        "2026-04-05T02:00:00",
        "Australia/Lord_Howe",
        "2026-04-04T15:30:00+00:00",
    );

    // Pacific/Chatham: 45-minute offset with 1-hour DST shift (UTC+12:45 <-> UTC+13:45) at 02:45.
    // Spring-forward 2026-09-27 02:45 -> 03:45; fall-back 2026-04-05 03:45 -> 02:45.
    assert_zoned_utc(
        "2026-09-27T02:30:00",
        "Pacific/Chatham",
        "2026-09-26T13:45:00+00:00",
    );
    assert_zoned_utc(
        "2026-09-27T03:15:00",
        "Pacific/Chatham",
        "2026-09-26T14:30:00+00:00",
    );
    assert_zoned_utc(
        "2026-09-27T03:45:00",
        "Pacific/Chatham",
        "2026-09-26T14:00:00+00:00",
    );
    assert_zoned_utc(
        "2026-04-05T03:15:00",
        "Pacific/Chatham",
        "2026-04-04T13:30:00+00:00",
    );
    assert_zoned_utc(
        "2026-04-05T03:45:00",
        "Pacific/Chatham",
        "2026-04-04T15:00:00+00:00",
    );
}

#[test]
fn ac1_cross_day_boundaries_and_dateline_full_day_skips() {
    // Cross-year forward (America/Vancouver, UTC-8) and cross-month backward across Feb end (Asia/Tokyo, UTC+9).
    assert_zoned_utc(
        "2026-12-31T20:30:00",
        "America/Vancouver",
        "2027-01-01T04:30:00+00:00",
    );
    assert_zoned_utc(
        "2026-03-01T00:30:00",
        "Asia/Tokyo",
        "2026-02-28T15:30:00+00:00",
    );
    assert_zoned_utc(
        "2028-03-01T00:30:00",
        "Asia/Tokyo",
        "2028-02-29T15:30:00+00:00",
    );

    // International Date Line full-day skip: Pacific/Apia skipped 2011-12-30 entirely
    // (2011-12-29T23:59:59-10:00 -> 2011-12-31T00:00:00+14:00).
    assert_zoned_utc(
        "2011-12-29T23:30:00",
        "Pacific/Apia",
        "2011-12-30T09:30:00+00:00",
    );
    assert_zoned_utc(
        "2011-12-30T09:00:00",
        "Pacific/Apia",
        "2011-12-30T19:00:00+00:00",
    );
    assert_zoned_utc(
        "2011-12-30T23:30:00",
        "Pacific/Apia",
        "2011-12-31T09:30:00+00:00",
    );
    assert_zoned_utc(
        "2011-12-31T00:30:00",
        "Pacific/Apia",
        "2011-12-30T10:30:00+00:00",
    );

    // Pacific/Kwajalein skipped 1993-08-20 entirely (UTC-12 -> UTC+12).
    assert_zoned_utc(
        "1993-08-20T12:00:00",
        "Pacific/Kwajalein",
        "1993-08-21T00:00:00+00:00",
    );
}

#[test]
fn ac1_and_ac2_same_name_source_vtimezone_conflicts_never_fall_back_to_iana() {
    let custom_vancouver = Timezone {
        tzid: "America/Vancouver".to_owned(),
        vtimezone: "BEGIN:VTIMEZONE\nTZID:America/Vancouver\nBEGIN:STANDARD\nTZOFFSETFROM:+0200\nTZOFFSETTO:+0200\nDTSTART:19700101T000000\nEND:STANDARD\nEND:VTIMEZONE".to_owned(),
    };

    // Without source VTIMEZONE override, America/Vancouver resolves via IANA (09:00 PST = 17:00Z).
    let iana_event = event_with(json!({
        "start": {"kind": "zoned", "local": "2026-11-02T09:00:00", "tzid": "America/Vancouver"},
        "end": {"kind": "utc", "value": "2026-11-02T16:00:00Z"}
    }));
    assert!(validate_event(&iana_event).is_err());
    assert_eq!(
        iana_event
            .resolve_utc(&iana_event.start)
            .unwrap()
            .unwrap()
            .to_rfc3339(),
        "2026-11-02T17:00:00+00:00"
    );

    // With same-name source VTIMEZONE, storage validation accepts the document without applying IANA offsets,
    // including on RDATE periods and exception overrides.
    let same_name_event = event_with(json!({
        "start": {"kind": "zoned", "local": "2026-11-02T09:00:00", "tzid": "America/Vancouver"},
        "end": {"kind": "utc", "value": "2026-11-02T16:00:00Z"},
        "recurrence": {
            "rrule": "FREQ=DAILY",
            "rdates": [{
                "kind": "period",
                "start": {"kind": "zoned", "local": "2026-11-03T09:00:00", "tzid": "America/Vancouver"},
                "end": {"kind": "utc", "value": "2026-11-03T16:00:00Z"}
            }],
            "exceptions": [{
                "recurrence_id": {"kind": "zoned", "local": "2026-11-04T09:00:00", "tzid": "America/Vancouver"},
                "range": "single",
                "change": {
                    "kind": "override",
                    "patch": {
                        "end": {"kind": "utc", "value": "2026-11-04T16:00:00Z"}
                    }
                }
            }]
        },
        "timezones": [custom_vancouver.clone()]
    }));
    assert_eq!(validate_event(&same_name_event), Ok(()));

    // At planning/resolution time, same-name source VTIMEZONE MUST be explicitly rejected as
    // UnsupportedTimezone; it must never silently fall back to IANA America/Vancouver.
    assert_eq!(
        same_name_event.resolve_utc(&same_name_event.start),
        Err(Code::UnsupportedTimezone)
    );
    assert_eq!(
        same_name_event
            .start
            .to_utc_with_timezones(&[custom_vancouver]),
        Err(Code::UnsupportedTimezone)
    );

    // If a source declares a same-name timezone entry with a blank/whitespace vtimezone body,
    // neither validation nor resolution may fall back to the IANA definition.
    let blank_same_name = event_with(json!({
        "start": {"kind": "zoned", "local": "2026-11-02T09:00:00", "tzid": "America/Vancouver"},
        "timezones": [{"tzid": "America/Vancouver", "vtimezone": "   "}]
    }));
    let errs = validate_event(&blank_same_name).unwrap_err();
    assert!(errs
        .iter()
        .any(|e| e.code == Code::UnknownTimezone && e.path == "/start"));
    assert_eq!(
        blank_same_name.resolve_utc(&blank_same_name.start),
        Err(Code::UnknownTimezone)
    );
}

#[test]
fn ac2_uninterpreted_custom_vtimezone_and_unknown_zones_are_rejected_without_host_fallback() {
    let custom_tz = Timezone {
        tzid: "Custom/Proprietary".to_owned(),
        vtimezone: "BEGIN:VTIMEZONE\nTZID:Custom/Proprietary\nEND:VTIMEZONE".to_owned(),
    };
    let custom_event = event_with(json!({
        "start": {"kind": "zoned", "local": "2026-11-02T09:00:00", "tzid": "Custom/Proprietary"},
        "timezones": [custom_tz.clone()]
    }));
    // Valid to store, but explicitly rejected for planning/UTC resolution.
    assert_eq!(validate_event(&custom_event), Ok(()));
    assert_eq!(
        custom_event.resolve_utc(&custom_event.start),
        Err(Code::UnsupportedTimezone)
    );
    assert_eq!(
        custom_event.start.to_utc_with_timezones(&[custom_tz]),
        Err(Code::UnsupportedTimezone)
    );
    // Without the source timezone table, Custom/Proprietary is an unknown zone (never Ok(None) or host TZ).
    assert_eq!(custom_event.start.to_utc(), Err(Code::UnknownTimezone));

    // Completely unknown zone is rejected everywhere.
    let unknown_event = event_with(json!({
        "start": {"kind": "zoned", "local": "2026-11-02T09:00:00", "tzid": "Mars/Olympus"}
    }));
    let errs = validate_event(&unknown_event).unwrap_err();
    assert!(errs
        .iter()
        .any(|e| e.code == Code::UnknownTimezone && e.path == "/start"));
    assert_eq!(
        unknown_event.resolve_utc(&unknown_event.start),
        Err(Code::UnknownTimezone)
    );
    assert_eq!(unknown_event.start.to_utc(), Err(Code::UnknownTimezone));

    // All-day and floating values do not resolve to a UTC instant without calendar policy,
    // and never fall back to the host OS timezone; malformed values still return InvalidTemporal.
    assert_eq!(date("2026-11-02").to_utc(), Ok(None));
    assert_eq!(floating("2026-11-02T09:00:00").to_utc(), Ok(None));
    assert_eq!(date("2026-02-30").to_utc(), Err(Code::InvalidTemporal));
    assert_eq!(
        floating("2026-11-02T25:00:00").to_utc(),
        Err(Code::InvalidTemporal)
    );
    assert_eq!(
        utc("2026-11-02T09:00:00").to_utc(),
        Err(Code::InvalidTemporal)
    );
    assert_eq!(
        zoned("2026-02-30T09:00:00", "America/Vancouver").to_utc(),
        Err(Code::InvalidTemporal)
    );
}

#[test]
fn ac3_pinned_tzdb_version_and_deterministic_golden_matrix() {
    assert_eq!(TZDB_VERSION, "2025b");

    let golden_cases = [
        (
            "2026-03-08T02:30:00",
            "America/Vancouver",
            "2026-03-08T10:30:00+00:00",
        ),
        (
            "2026-11-01T01:30:00",
            "America/Vancouver",
            "2026-11-01T08:30:00+00:00",
        ),
        (
            "2026-10-04T02:15:00",
            "Australia/Lord_Howe",
            "2026-10-03T15:45:00+00:00",
        ),
        (
            "2026-04-05T01:45:00",
            "Australia/Lord_Howe",
            "2026-04-04T14:45:00+00:00",
        ),
        (
            "2026-09-27T03:15:00",
            "Pacific/Chatham",
            "2026-09-26T14:30:00+00:00",
        ),
        (
            "2026-04-05T03:15:00",
            "Pacific/Chatham",
            "2026-04-04T13:30:00+00:00",
        ),
        (
            "2026-03-08T00:30:00",
            "America/Havana",
            "2026-03-08T05:30:00+00:00",
        ),
        (
            "2026-11-01T00:30:00",
            "America/Havana",
            "2026-11-01T04:30:00+00:00",
        ),
        (
            "2011-12-30T09:00:00",
            "Pacific/Apia",
            "2011-12-30T19:00:00+00:00",
        ),
    ];
    for (local, tzid, expected_utc) in golden_cases {
        assert_zoned_utc(local, tzid, expected_utc);
    }
}
