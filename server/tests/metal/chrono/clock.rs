use super::{Clock, Sample, duration, later_count};
use core::{marker::PhantomData, num::NonZeroU64, time::Duration};

#[test]
fn conversion_floors_subnanosecond_samples_and_preserves_long_intervals() {
    let hz = NonZeroU64::new(3).unwrap();
    assert_eq!(duration(2, hz), Duration::new(0, 666_666_666));
    assert_eq!(duration(4, hz), Duration::new(1, 333_333_333));
    assert_eq!(
        duration(u64::MAX, NonZeroU64::new(1).unwrap()),
        Duration::from_secs(u64::MAX)
    );
    let hz = NonZeroU64::new(u64::MAX).unwrap();
    assert_eq!(
        duration(u64::MAX - 1, hz),
        Duration::from_nanos(999_999_999)
    );
}

#[test]
fn deadline_conversion_rounds_up_and_refuses_empty_or_wrapping_targets() {
    let hz = NonZeroU64::new(3).unwrap();
    assert_eq!(later_count(50, hz, Duration::from_nanos(1)), Some(51));
    assert_eq!(
        later_count(50, hz, Duration::from_nanos(333_333_334)),
        Some(52)
    );
    assert_eq!(later_count(50, hz, Duration::ZERO), None);
    assert_eq!(later_count(u64::MAX, hz, Duration::from_secs(1)), None);
    assert_eq!(later_count(0, hz, Duration::from_secs(u64::MAX)), None);
}

#[cfg(not(limen_target))]
#[test]
fn samples_and_deadlines_keep_their_exact_clock_domain() {
    let hertz = NonZeroU64::new(1_000).unwrap();
    let clock = Clock {
        hertz,
        local: PhantomData,
    };
    let other = Clock {
        hertz,
        local: PhantomData,
    };
    let earlier = Sample {
        clock: &clock,
        count: 10,
    };
    let later = Sample {
        clock: &clock,
        count: 12,
    };
    let other_sample = Sample {
        clock: &other,
        count: 9,
    };
    assert_eq!(later.elapsed_since(earlier), Some(Duration::from_millis(2)));
    assert_eq!(earlier.elapsed_since(later), None);
    assert_eq!(later.elapsed_since(other_sample), None);
    let deadline = earlier.after(Duration::from_millis(2)).unwrap();
    assert_eq!(deadline.count_for(&clock), Some(12));
    assert_eq!(deadline.count_for(&other), None);
}
