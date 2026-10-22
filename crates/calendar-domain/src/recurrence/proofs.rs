//! Kani proofs of production helpers; bounds and trusted dependencies: docs/formal-verification.md.
use super::*;

#[kani::proof]
#[kani::unwind(2)]
fn budget_step() {
    let maximum: usize = kani::any();
    let remaining: usize = kani::any();
    kani::assume(remaining <= maximum);
    let mut budget = IterationBudget {
        remaining,
        max_iterations: maximum,
    };
    let outcome = budget.consume();
    if remaining == 0 {
        assert!(
            matches!(outcome, Err(ExpansionError::IterationLimitExceeded { max_iterations }) if max_iterations == maximum),
            "exhausted budget must reject"
        );
        assert!(budget.remaining == 0, "exhausted budget must stay zero");
    } else {
        assert!(outcome.is_ok(), "available budget must succeed");
        assert!(
            budget.remaining == remaining - 1,
            "budget must decrease by one"
        );
    }
    assert!(
        budget.max_iterations == maximum,
        "maximum must remain unchanged"
    );
}

#[kani::proof]
#[kani::unwind(6)]
fn budget_exhaustion() {
    let maximum: u8 = kani::any();
    kani::assume(maximum <= 4);
    let mut budget = IterationBudget {
        remaining: usize::from(maximum),
        max_iterations: usize::from(maximum),
    };
    for _ in 0..maximum {
        assert!(budget.consume().is_ok(), "initial budget must be usable");
    }
    for _ in 0..2 {
        assert!(budget.consume().is_err(), "exhaustion must persist");
        assert!(budget.remaining == 0, "exhaustion must not wrap");
    }
}

#[kani::proof]
#[kani::unwind(2)]
fn window_membership() {
    // Whole-second UTC instants in [-128, 127] seconds from the Unix epoch.
    let start: i8 = kani::any();
    let end: i8 = kani::any();
    let left: i8 = kani::any();
    let right: i8 = kani::any();
    kani::assume(start <= end && left < right);
    let instant = |seconds: i8| DateTime::<Utc>::from_timestamp(i64::from(seconds), 0).unwrap();
    let actual = overlaps_window(instant(start), instant(end), instant(left), instant(right));
    // Independent complement specification: intervals are disjoint iff one ends before
    // the other starts. A point is a member iff it is at/after left and strictly before right.
    let disjoint = if start == end {
        start < left || start >= right
    } else {
        end <= left || start >= right
    };
    assert!(
        actual != disjoint,
        "half-open window membership must match specification"
    );
}
