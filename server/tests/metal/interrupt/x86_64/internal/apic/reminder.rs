use super::{Observation, Rate};

#[test]
fn conversion_uses_the_actual_bracket_and_rounds_toward_an_early_reminder() {
    let first = Observation {
        before: 100,
        current: 1000,
        after: 110,
    };
    let last = Observation {
        before: 210,
        current: 900,
        after: 220,
    };
    let rate = Rate::observe(first, last).unwrap();
    assert_eq!(rate.decrement, 99);
    assert_eq!(rate.reference, 121);
    assert_eq!(rate.ticks(121).unwrap().get(), 99);
    assert_eq!(rate.ticks(1).unwrap().get(), 1);
    assert_eq!(rate.ticks(0), None);
    assert_eq!(rate.ticks(u64::MAX).unwrap().get(), u32::MAX);
}

#[test]
fn rate_rejects_overlapping_reversed_and_exhausted_observations() {
    let first = Observation {
        before: 100,
        current: 1000,
        after: 110,
    };
    for last in [
        Observation {
            before: 109,
            current: 900,
            after: 220,
        },
        Observation {
            before: 210,
            current: 1001,
            after: 220,
        },
        Observation {
            before: 210,
            current: 999,
            after: 220,
        },
        Observation {
            before: 210,
            current: 0,
            after: 220,
        },
        Observation {
            before: 220,
            current: 900,
            after: 210,
        },
    ] {
        assert!(Rate::observe(first, last).is_none());
    }
    assert!(
        Rate::observe(
            Observation {
                before: 0,
                current: 1000,
                after: 1
            },
            Observation {
                before: 2,
                current: 900,
                after: u64::MAX
            },
        )
        .is_none()
    );
}
