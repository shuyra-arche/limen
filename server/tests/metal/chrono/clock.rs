use super::{duration, later_count};
use core::{num::NonZeroU32, time::Duration};

#[test]
fn conversion_floors_subnanosecond_samples_and_preserves_long_intervals() {
    let hz = NonZeroU32::new(3).unwrap();
    assert_eq!(duration(2, hz), Duration::new(0, 666_666_666));
    assert_eq!(duration(4, hz), Duration::new(1, 333_333_333));
    assert_eq!(
        duration(u64::MAX, NonZeroU32::new(1).unwrap()),
        Duration::from_secs(u64::MAX)
    );
    let hz = NonZeroU32::new(u32::MAX).unwrap();
    assert_eq!(
        duration(u64::from(u32::MAX) + 1, hz),
        Duration::from_secs(1)
    );
}

#[test]
fn deadline_conversion_rounds_up_and_refuses_empty_or_wrapping_targets() {
    let hz = NonZeroU32::new(3).unwrap();
    assert_eq!(later_count(50, hz, Duration::from_nanos(1)), Some(51));
    assert_eq!(
        later_count(50, hz, Duration::from_nanos(333_333_334)),
        Some(52)
    );
    assert_eq!(later_count(50, hz, Duration::ZERO), None);
    assert_eq!(later_count(u64::MAX, hz, Duration::from_secs(1)), None);
    assert_eq!(later_count(0, hz, Duration::from_secs(u64::MAX)), None);
}
