//! The same consumer of a confirmed native timer on both hardware profiles.

use core::{fmt::Debug, time::Duration};
use kernel::metal::{
    chrono::treaty::Clock,
    interrupt::treaty::{AbsoluteTimer, TimerEffect},
};

pub fn arm<T: AbsoluteTimer>(clock: &Clock, timer: &mut T, delay: Duration)
where
    T::Failure: Debug,
{
    let target = clock.sample().unwrap().after(delay).unwrap();
    timer.arm_at(target).unwrap();
    assert!(timer.pending_effect().unwrap().is_none());
    assert!(timer.confirm_effect().is_err());
    assert!(!timer.due().unwrap());
}

pub fn confirm_due<T: AbsoluteTimer>(timer: &T)
where
    T::Failure: Debug,
{
    assert!(timer.due().unwrap());
}

pub fn advance<T: AbsoluteTimer>(clock: &Clock, timer: &mut T, early: Duration)
where
    T::Failure: Debug,
{
    let sample = clock.sample().unwrap();
    let later = sample.after(Duration::from_secs(30)).unwrap();
    assert!(timer.advance_deadline(later).is_err());
    assert!(!timer.due().unwrap());
    timer
        .advance_deadline(sample.after(early).unwrap())
        .unwrap();
    assert!(timer.pending_effect().unwrap().is_none());
}

/// # Safety
/// The target scenario has taken responsibility for the original notification.
pub unsafe fn stop<T: AbsoluteTimer>(timer: &mut T) -> Result<(), T::Failure>
where
    T::Failure: Debug,
{
    let result = if timer.pending_effect()? == Some(TimerEffect::Clearing) {
        timer.confirm_effect()
    } else {
        unsafe { timer.stop_due() }
    };
    if result.is_ok() {
        assert!(timer.pending_effect()?.is_none());
        assert!(timer.confirm_effect().is_err());
    }
    result
}
