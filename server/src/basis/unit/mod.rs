//! Identity, continuation lifetime and the single terminal decision.

use core::num::NonZeroU64;

mod stack;
use crate::metal::execution::Context;
pub(super) use stack::STACK_CAPACITY;
pub use stack::Stacks;
use stack::{StackLease, StackPool};

use crate::metal::identity::Slot;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnitId {
    slot: Slot,
    generation: NonZeroU64,
}

impl UnitId {
    pub(super) fn slot(self) -> usize {
        self.slot.index()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum End {
    Returned { value: u64 },
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lifecycle {
    Live,
    Ending,
    Ended(End),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Error {
    Capacity,
    IdentityExhausted,
    Gone,
    NotLive,
    NotEnding,
    NotEnded,
}

enum State<Binding> {
    Vacant,
    Live(Binding),
    Ending { end: End, binding: Binding },
    Ended(End),
}

struct Record<Binding> {
    generation: Option<NonZeroU64>,
    state: State<Binding>,
}

struct Bound<Binding> {
    binding: Binding,
    stack: StackLease,
}

pub(super) struct Units<Binding> {
    records: [Record<Bound<Binding>>; STACK_CAPACITY],
    stacks: StackPool,
}

impl<Binding> Units<Binding> {
    pub(super) fn new(stacks: &'static mut Stacks) -> Self {
        Self {
            records: [const {
                Record {
                    generation: None,
                    state: State::Vacant,
                }
            }; STACK_CAPACITY],
            stacks: StackPool::new(stacks),
        }
    }

    pub(crate) fn vacant(&self) -> Result<UnitId, Error> {
        let mut reusable = false;
        for (index, record) in self.records.iter().enumerate() {
            let Some(slot) = Slot::new(index) else { break };
            let generation = match record.generation {
                Some(previous) => previous.checked_add(1),
                None => Some(NonZeroU64::MIN),
            };
            if let Some(generation) = generation {
                if matches!(record.state, State::Vacant) {
                    return Ok(UnitId { slot, generation });
                }
                reusable = true;
            }
        }
        Err(if reusable {
            Error::Capacity
        } else {
            Error::IdentityExhausted
        })
    }

    /// Commit the slot reserved by `vacant` in the same protected scope.
    pub(super) fn publish(
        &mut self,
        id: UnitId,
        binding: Binding,
        start: extern "C" fn(*const ()) -> !,
    ) -> Result<(), Error> {
        let stack = self.stacks.claim(start).ok_or(Error::Capacity)?;
        self.records[id.slot()] = Record {
            generation: Some(id.generation),
            state: State::Live(Bound { binding, stack }),
        };
        Ok(())
    }

    fn record(&mut self, id: UnitId) -> Result<&mut State<Bound<Binding>>, Error> {
        let slot = self.records.get_mut(id.slot()).ok_or(Error::Gone)?;
        if slot.generation != Some(id.generation) || matches!(slot.state, State::Vacant) {
            return Err(Error::Gone);
        }
        Ok(&mut slot.state)
    }

    pub(crate) fn lifecycle(&mut self, id: UnitId) -> Result<Lifecycle, Error> {
        Ok(match self.record(id)? {
            State::Live(_) => Lifecycle::Live,
            State::Ending { .. } => Lifecycle::Ending,
            State::Ended(end) => Lifecycle::Ended(*end),
            State::Vacant => unreachable!(),
        })
    }

    pub(super) fn live(&mut self, id: UnitId) -> Result<(&Binding, &'static Context), Error> {
        match self.record(id)? {
            State::Live(bound) => Ok((&bound.binding, bound.stack.context())),
            _ => Err(Error::NotLive),
        }
    }

    pub(crate) fn begin_end(&mut self, id: UnitId, end: End) -> Result<(), Error> {
        let state = self.record(id)?;
        match state {
            State::Live(_) => {}
            State::Ending { end: accepted, .. } | State::Ended(accepted) if *accepted == end => {
                return Ok(());
            }
            _ => return Err(Error::NotLive),
        }
        let State::Live(binding) = core::mem::replace(state, State::Vacant) else {
            unreachable!()
        };
        *state = State::Ending { end, binding };
        Ok(())
    }

    /// # Safety
    ///
    /// The caller holds the protection that proves Room has released the
    /// admission and Metal no longer holds any use of the continuation.
    pub(super) unsafe fn finish_end(&mut self, id: UnitId) -> Result<(), Error> {
        let state = self.record(id)?;
        let end = match state {
            State::Ending { end, .. } => *end,
            State::Ended(_) => return Ok(()),
            _ => return Err(Error::NotEnding),
        };
        let State::Ending { binding, .. } = core::mem::replace(state, State::Ended(end)) else {
            unreachable!()
        };
        // Clear and return the continuation before the protected End can be read.
        unsafe { self.stacks.release(binding.stack) };
        Ok(())
    }

    pub(crate) fn reap(&mut self, id: UnitId) -> Result<End, Error> {
        let state = self.record(id)?;
        let State::Ended(end) = *state else {
            return Err(Error::NotEnded);
        };
        *state = State::Vacant;
        Ok(end)
    }
}
