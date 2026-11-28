use super::{DurationBounds, StopBounds, TimingBounds, duration_ceil, ticks};
use core::{num::NonZeroU64, time::Duration};

fn hz(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).unwrap()
}

fn timing() -> TimingBounds {
    TimingBounds {
        minimum_hertz: hz(1_000),
        maximum_hertz: hz(2_000),
        count_error: 1,
        valid: 0..=10_000,
    }
}

fn stop() -> StopBounds {
    StopBounds {
        elapsed: Duration::from_millis(10),
        charged: Duration::from_millis(25),
        start: Duration::from_millis(10),
    }
}

#[test]
fn earliest_budget_or_window_comparison_preserves_both_stop_margins() {
    let timing = timing();
    assert!(timing.valid(hz(1_500)));
    assert!(!timing.valid(hz(999)));
    assert!(!timing.valid(hz(2_001)));
    assert_eq!(
        timing.trigger(1_000, 2_000, Duration::from_millis(150), stop()),
        Some(1_118),
    );
    assert_eq!(
        timing.trigger(1_000, 1_080, Duration::from_millis(150), stop()),
        Some(1_055),
    );
    assert_eq!(
        timing.trigger(1_000, 1_047, Duration::from_millis(150), stop()),
        None,
    );
    assert_eq!(
        timing.trigger(1_000, 2_000, Duration::from_millis(25), stop()),
        None,
    );
    assert_eq!(
        timing.trigger(1_000, 20_000, Duration::from_millis(150), stop()),
        None,
    );
}

#[test]
fn window_and_start_recheck_use_full_read_interval_and_strict_latest_entry() {
    let timing = timing();
    assert!(timing.window_valid(100, 200));
    assert!(!timing.window_valid(200, 200));
    assert!(!timing.window_valid(100, 20_000));
    assert!(!timing.in_window(101, 100, 200));
    assert!(timing.in_window(102, 100, 200));
    assert!(timing.in_window(197, 100, 200));
    assert!(!timing.in_window(198, 100, 200));
    assert!(timing.can_start(1_000, 1_021, stop().start));
    assert!(!timing.can_start(1_000, 1_020, stop().start));
}

#[test]
fn earlier_confirmed_comparison_must_cover_the_stop_and_leave_the_full_start_tail() {
    let timing = timing();
    let start = stop().start;
    assert!(timing.covers_start(1_000, 900, 1_118, 1_118, start));
    assert!(timing.covers_start(1_000, 900, 1_118, 1_023, start));
    assert!(!timing.covers_start(1_000, 900, 1_118, 1_022, start));
    assert!(!timing.covers_start(1_000, 900, 1_118, 1_119, start));
    assert!(!timing.covers_start(1_000, 900, 1_118, 999, start));
    assert!(!timing.covers_start(1_000, 900, 20_000, 1_100, start));
}

#[test]
fn measured_charge_rounds_outward_and_rejects_expired_or_reversed_reads() {
    let timing = timing();
    assert_eq!(
        timing.measure(1_000, 1_009),
        Some(DurationBounds {
            minimum: Duration::from_micros(2_500),
            maximum: Duration::from_millis(13),
        }),
    );
    assert_eq!(
        timing.measure(1_000, 1_000),
        Some(DurationBounds {
            minimum: Duration::ZERO,
            maximum: Duration::from_millis(4),
        }),
    );
    assert_eq!(timing.measure(1_000, 999), None);
    assert_eq!(timing.measure(1_000, 9_999), None);
    assert_eq!(timing.measure(1_000, u64::MAX), None);
}

#[test]
fn count_and_duration_rounding_handle_fractional_ticks_and_overflow() {
    assert_eq!(ticks(Duration::from_nanos(10), hz(1_000), false), Some(0));
    assert_eq!(ticks(Duration::from_nanos(10), hz(1_000), true), Some(1));
    assert_eq!(ticks(Duration::MAX, hz(u64::MAX), true), None);
    assert_eq!(
        duration_ceil(1, hz(3)),
        Some(Duration::from_nanos(333_333_334))
    );
    assert_eq!(
        duration_ceil(u64::MAX, hz(1)),
        Some(Duration::from_secs(u64::MAX))
    );
}
