//! Sign's timed waits on one admitted clock.

mod deadlines;

#[cfg(limen_cooperative)]
pub(super) use deadlines::TockId;

#[cfg(limen_cooperative)]
use super::room::Occupancy;
#[cfg(limen_cooperative)]
use crate::metal::{chrono::treaty::Clock, sync::treaty::TurnLock};

#[cfg(limen_cooperative)]
pub(super) type Deadlines<const CAPACITY: usize> = deadlines::Deadlines<Occupancy, CAPACITY>;

/// The preallocated Tocks and expiry order for one exact local clock.
///
/// The clock stays borrowed for the lifetime of the index. The index stores
/// integer comparisons in this one domain and retains the original occupancy
/// until the corresponding Room timeout has been committed.
#[cfg(limen_cooperative)]
pub struct Sign<'clock, const CAPACITY: usize> {
    pub(super) clock: &'clock Clock,
    pub(super) deadlines: TurnLock<Deadlines<CAPACITY>>,
}

#[cfg(limen_cooperative)]
impl<'clock, const CAPACITY: usize> Sign<'clock, CAPACITY> {
    pub const fn new(clock: &'clock Clock) -> Self {
        Self {
            clock,
            deadlines: TurnLock::new(Deadlines::new()),
        }
    }
}
