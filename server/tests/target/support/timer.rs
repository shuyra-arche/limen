//! The same consumer of a confirmed native timer on both hardware profiles.

use core::{fmt::Debug, time::Duration};
use kernel::metal::{chrono::Clock, interrupt::AbsoluteTimer};

pub fn arm<T: AbsoluteTimer>(clock: &Clock<'_>, timer: &mut T, delay: Duration)
where
    T::Failure: Debug,
{
    let target = clock.sample().unwrap().after(delay).unwrap();
    timer.arm_at(target).unwrap();
    assert!(!timer.due().unwrap());
}

pub fn confirm_due<T: AbsoluteTimer>(timer: &T)
where
    T::Failure: Debug,
{
    assert!(timer.due().unwrap());
}
