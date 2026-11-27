use super::*;

#[test]
fn divider_uses_only_the_legacy_timer_fields() {
    let dividers = [
        Divider::One,
        Divider::Two,
        Divider::Four,
        Divider::Eight,
        Divider::Sixteen,
        Divider::ThirtyTwo,
        Divider::SixtyFour,
        Divider::OneHundredTwentyEight,
    ];
    assert_eq!(dividers.map(Divider::bits), [11, 0, 1, 2, 3, 8, 9, 10]);
    assert!(
        dividers
            .iter()
            .all(|divider| divider.bits() & !DIVIDE_FIELDS == 0)
    );
}

#[test]
fn unknown_programming_keeps_the_original_countdown_through_stop() {
    let original = Countdown {
        ticks: NonZeroU32::new(123).unwrap(),
        divider: Divider::Eight,
    };
    let replacement = Countdown {
        ticks: NonZeroU32::new(456).unwrap(),
        divider: Divider::One,
    };
    let arming = TimerState::Reserved.begin_arm(original).unwrap();
    assert_eq!(arming, TimerState::Arming(original));
    assert_eq!(arming.begin_arm(replacement), Err(Failure::Busy));
    let stopping = arming.begin_stop();
    assert_eq!(stopping, TimerState::Stopping(original));
    assert_eq!(stopping.begin_arm(replacement), Err(Failure::Busy));
    assert_eq!(stopping.begin_stop(), stopping);
    assert_eq!(TimerState::Armed(original).begin_stop(), stopping);
}

#[test]
fn inherited_stop_keeps_original_mode_and_vector_and_refuses_deadline_mode() {
    let original = Inherited::observe(PERIODIC | DELIVERY_PENDING | 0xf1).unwrap();
    assert_eq!(original.vector(), 0xf1);
    assert_eq!(original.masked(), MASKED | PERIODIC | 0xf1);
    assert!(matches!(
        Inherited::observe((1 << 18) | 0xf2),
        Err(Failure::Scope)
    ));
}
