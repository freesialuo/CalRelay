//! Bounded integration enumeration: verify that recurrence paths actually charge the budget.
use calendar_domain::{expand_occurrences, EventV1, ExpansionError, ExpansionOptions};
use chrono::{DateTime, Utc};
use serde_json::json;

fn instant(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}

#[test]
fn bounded_expansions_charge_shared_budget_before_deduplication_and_exclusion() {
    for count in 1..=4 {
        for extra in 0..=4 {
            for excluded in [false, true] {
                let start = json!({"kind": "utc", "value": "2026-01-01T09:00:00Z"});
                let event: EventV1 = serde_json::from_value(json!({
                    "schema_version": "1.0", "external_uid": "bounded", "title": "Bounded",
                    "status": "confirmed", "start": start, "duration": "PT1H",
                    "recurrence": {
                        "rrule": format!("FREQ=DAILY;COUNT={count}"),
                        "rdates": vec![start.clone(); extra],
                        "exdates": if excluded { vec![start] } else { vec![] }
                    }
                }))
                .unwrap();
                for budget in 1..=9 {
                    let options = ExpansionOptions {
                        window_start: instant("2026-01-01T00:00:00Z"),
                        window_end: instant("2026-02-01T00:00:00Z"),
                        calendar_tzid: None,
                        max_occurrences: 4,
                        max_iterations: budget,
                    };
                    let actual = expand_occurrences(&event, &options);
                    if budget < count + extra {
                        assert_eq!(
                            actual,
                            Err(ExpansionError::IterationLimitExceeded {
                                max_iterations: budget
                            })
                        );
                    } else {
                        let occurrences = actual.unwrap();
                        assert_eq!(occurrences.len(), count - usize::from(excluded));
                        for (index, occurrence) in occurrences.iter().enumerate() {
                            let day = index + 1 + usize::from(excluded);
                            assert_eq!(
                                occurrence.start_utc,
                                instant(&format!("2026-01-{day:02}T09:00:00Z"))
                            );
                            assert_eq!(
                                occurrence.original_key,
                                format!("utc:2026-01-{day:02}T09:00:00Z")
                            );
                        }
                    }
                }
            }
        }
    }
}
