use super::{Counter, ProcessorId, Sample, duration, later_count};
use core::{num::NonZeroU64, time::Duration};

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

#[test]
fn samples_and_deadlines_keep_their_exact_clock_domain() {
    let processor = ProcessorId::new(1);
    let hertz = NonZeroU64::new(1_000).unwrap();
    use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

    static COUNT: AtomicU64 = AtomicU64::new(10);
    let clock = Counter::new(processor, hertz, || COUNT.load(Relaxed));
    let other = Counter::new(processor, hertz, || 12);
    let foreign = Counter::new(processor, hertz, || {
        panic!("foreign processor must not be sampled")
    });
    assert_eq!(clock.processor(), processor);
    assert_eq!(clock.frequency(), hertz);
    assert!(Sample::reported(&foreign, ProcessorId::new(2)).is_none());

    let earlier = Sample::reported(&clock, processor).unwrap();
    COUNT.store(12, Relaxed);
    let later = Sample::reported(&clock, processor).unwrap();
    let other_sample = Sample::reported(&other, processor).unwrap();
    assert_eq!(later.elapsed_since(earlier), Some(Duration::from_millis(2)));
    assert_eq!(earlier.elapsed_since(later), None);
    assert_eq!(later.elapsed_since(other_sample), None);
    COUNT.store(0, Relaxed);
    let wrapped = Sample::reported(&clock, processor).unwrap();
    assert_eq!(wrapped.elapsed_since(later), None);

    let deadline = earlier.after(Duration::from_millis(2)).unwrap();
    assert_eq!(deadline.count_for(&clock), Some(12));
    assert_eq!(deadline.count_for(&other), None);
}
