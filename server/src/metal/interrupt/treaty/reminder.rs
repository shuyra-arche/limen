//! The ordinary wake/recheck boundary for an admitted actual timer.
//! An absolute request is not necessarily a native absolute setting.

use super::AbsoluteTimer;
use crate::metal::chrono::treaty::{Clock, Deadline};

pub trait Sealed {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReminderProgress {
    Idle,
    Armed,
    AwaitInterrupt,
    Recheck,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReminderError<E> {
    Timing,
    Timer(E),
}

/// Keep the actual native wake obligation while serving an original absolute
/// request in the supplied admitted Clock. The caller retains the business
/// deadline and alone determines whether it is due. This interface does not
/// provide the confirmed absolute stop coverage required by Fund.
pub trait Reminder: Sealed {
    type Failure;

    fn recheck(
        &mut self,
        clock: &Clock,
        next: Option<Deadline<'_>>,
    ) -> Result<ReminderProgress, ReminderError<Self::Failure>>;
}

impl<T: AbsoluteTimer> Sealed for T {}

impl<T: AbsoluteTimer> Reminder for T {
    type Failure = T::Failure;

    fn recheck(
        &mut self,
        clock: &Clock,
        next: Option<Deadline<'_>>,
    ) -> Result<ReminderProgress, ReminderError<Self::Failure>> {
        let error = ReminderError::Timer;
        if self.pending_effect().map_err(error)?.is_some() {
            self.confirm_effect().map_err(error)?;
            return Ok(ReminderProgress::Recheck);
        }
        let armed = self
            .confirmed_deadline()
            .map_err(error)?
            .map(|deadline| clock.count_for(deadline).ok_or(ReminderError::Timing))
            .transpose()?;
        let next = next
            .map(|deadline| clock.count_for(deadline).ok_or(ReminderError::Timing))
            .transpose()?;
        let now = clock.sample().ok_or(ReminderError::Timing)?.count();
        if armed.is_some_and(|count| count <= now) {
            // The original receiver must obtain the actual interrupt.
            return Ok(ReminderProgress::AwaitInterrupt);
        }
        if next.is_some_and(|count| count <= now) {
            return Ok(ReminderProgress::Recheck);
        }
        let (result, count) = match (armed, next) {
            (None, Some(count)) => (self.arm_at(clock.deadline(count)), count),
            (Some(old), Some(count)) if count < old => {
                (self.advance_deadline(clock.deadline(count)), count)
            }
            (None, None) => return Ok(ReminderProgress::Idle),
            _ => return Ok(ReminderProgress::Armed),
        };
        if let Err(reason) = result {
            let reached = clock.sample().is_some_and(|sample| sample.count() >= count);
            if reached && matches!(self.pending_effect(), Ok(None)) {
                return Ok(ReminderProgress::Recheck);
            }
            return Err(error(reason));
        }
        Ok(ReminderProgress::Armed)
    }
}
