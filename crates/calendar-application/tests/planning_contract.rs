use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use calendar_application::{
    plan_events, Clock, EventV1, PlanningError, PlanningEventInput, PlanningLimits,
    PlanningRequest, RulePolicy,
};
use serde_json::{json, Value};

const NOW: i64 = 1_793_577_600_000; // 2026-11-02T00:00:00Z
const DAY: i64 = 86_400_000;

struct FakeClock {
    utc: AtomicI64,
    monotonic: AtomicU64,
    step: u64,
}

impl FakeClock {
    fn fixed(utc_ms: i64) -> Self {
        Self {
            utc: AtomicI64::new(utc_ms),
            monotonic: AtomicU64::new(0),
            step: 0,
        }
    }
}

impl Clock for FakeClock {
    fn utc_now_ms(&self) -> i64 {
        self.utc.load(Ordering::SeqCst)
    }

    fn monotonic_now_ms(&self) -> u64 {
        self.monotonic.fetch_add(self.step, Ordering::SeqCst)
    }
}

fn default_request() -> PlanningRequest {
    PlanningRequest {
        source_id: "s".into(),
        base_revision: 1,
        rule_id: "r".into(),
        rule_generation: 1,
        output_id: "o".into(),
        output_generation: 1,
        horizon_ms: 30 * DAY,
        max_lead_ms: 7 * DAY,
        extension_interval_ms: 300_000,
        tzdata_version: "2025b".into(),
        limits: PlanningLimits {
            max_occurrences: 100,
            max_iterations: 1_000,
            max_elapsed_ms: 1_000,
        },
    }
}

fn default_policy() -> RulePolicy {
    RulePolicy {
        calendar_ids: vec!["c".into()],
        lead_ms: 900_000,
        all_day_time: "09:00".into(),
    }
}

fn make_event(id: &str, tz: Option<&str>, extra: Value) -> PlanningEventInput {
    let mut doc = json!({
        "schema_version": "1.0",
        "external_uid": id,
        "title": "Review",
        "status": "confirmed",
        "duration": "PT1H",
        "start": {"kind": "utc", "value": "2026-11-02T17:00:00Z"}
    });
    for (k, v) in extra.as_object().expect("json object") {
        doc[k] = v.clone();
    }
    PlanningEventInput {
        event_id: id.into(),
        event_revision: 1,
        calendar_tzid: tz.map(str::to_owned),
        event: serde_json::from_value::<EventV1>(doc).expect("valid EventV1"),
    }
}

#[test]
fn window_and_lead_bounds_reject_invalid_configurations_without_panicking() {
    let clock = FakeClock::fixed(NOW);
    let policy = default_policy();

    let mut short = default_request();
    short.horizon_ms = short.max_lead_ms + short.extension_interval_ms - 1;
    assert_eq!(
        plan_events(&short, &policy, &[], &clock),
        Err(PlanningError::InvalidWindow)
    );

    let mut exact = default_request();
    exact.horizon_ms = exact.max_lead_ms + exact.extension_interval_ms;
    assert!(plan_events(&exact, &policy, &[], &clock).is_ok());

    let mut excessive_lead = default_policy();
    excessive_lead.lead_ms = 7 * DAY + 1;
    assert_eq!(
        plan_events(&default_request(), &excessive_lead, &[], &clock),
        Err(PlanningError::InvalidWindow)
    );

    let mut negative_lead = default_policy();
    negative_lead.lead_ms = -1;
    assert_eq!(
        plan_events(&default_request(), &negative_lead, &[], &clock),
        Err(PlanningError::InvalidWindow)
    );

    let overflow_clock = FakeClock::fixed(i64::MAX - 10);
    assert_eq!(
        plan_events(&default_request(), &policy, &[], &overflow_clock),
        Err(PlanningError::InvalidWindow)
    );
}

#[test]
fn cumulative_occurrence_and_iteration_budgets_apply_across_multiple_events() {
    let clock = FakeClock::fixed(NOW);
    let policy = default_policy();
    let events = vec![
        make_event(
            "e1",
            None,
            json!({"recurrence": {"rrule": "FREQ=DAILY;COUNT=2"}}),
        ),
        make_event(
            "e2",
            None,
            json!({"recurrence": {"rrule": "FREQ=DAILY;COUNT=2"}}),
        ),
    ];

    let mut iter_limited = default_request();
    iter_limited.limits.max_iterations = 3;
    assert_eq!(
        plan_events(&iter_limited, &policy, &events, &clock),
        Err(PlanningError::IterationLimitExceeded)
    );

    let mut iter_exact = default_request();
    iter_exact.limits.max_iterations = 4;
    assert_eq!(
        plan_events(&iter_exact, &policy, &events, &clock)
            .unwrap()
            .occurrences
            .len(),
        4
    );

    let mut occ_limited = default_request();
    occ_limited.limits.max_occurrences = 3;
    assert_eq!(
        plan_events(&occ_limited, &policy, &events, &clock),
        Err(PlanningError::OccurrenceLimitExceeded)
    );
}

#[test]
fn utc_clock_jumps_do_not_affect_monotonic_budget_and_window_extension_is_deterministic() {
    let policy = default_policy();
    let events = vec![make_event("e1", None, json!({}))];

    let jumped_utc = FakeClock {
        utc: AtomicI64::new(NOW + 300_000),
        monotonic: AtomicU64::new(0),
        step: 100,
    };
    let batch = plan_events(&default_request(), &policy, &events, &jumped_utc).unwrap();
    assert_eq!(batch.window_start, NOW + 300_000);
    assert_eq!(batch.planned_until, NOW + 300_000 + 30 * DAY);
    assert_eq!(batch.occurrences.len(), 1);
}
