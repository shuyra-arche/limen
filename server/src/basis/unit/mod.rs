//! Identity, continuation lifetime and the single terminal decision.

use core::num::NonZeroU64;

mod stack;
use crate::metal::processor::execution::cooperative::Context;
pub(super) use stack::STACK_CAPACITY;
pub use stack::Stacks;
use stack::{StackLease, StackPool};

use crate::metal::identity::Slot;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Scope(usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnitId {
    scope: Scope,
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
    Vacant(Option<Slot>),
    Retired,
    Live(Binding),
    Ending { end: End, binding: Binding },
    Ended(End),
}

/// Actual resident Unit record storage, supplied independently of stacks.
pub struct UnitRecord<Binding> {
    generation: Option<NonZeroU64>,
    state: State<Bound<Binding>>,
}

impl<Binding> UnitRecord<Binding> {
    pub const EMPTY: Self = Self {
        generation: None,
        state: State::Vacant(None),
    };
}

struct Bound<Binding> {
    binding: Binding,
    stack: StackLease,
}

pub(super) struct Units<Binding: 'static> {
    scope: Scope,
    records: &'static mut [UnitRecord<Binding>],
    free: Option<Slot>,
    retired: usize,
    stacks: StackPool,
}

impl<Binding: 'static> Units<Binding> {
    pub(super) fn accepts(records: &[UnitRecord<Binding>]) -> bool {
        !records.is_empty()
            && Slot::new(records.len() - 1).is_some()
            && records.iter().all(|record| {
                record.generation.is_none() && matches!(record.state, State::Vacant(None))
            })
    }

    pub(super) fn new(
        stacks: &'static mut Stacks,
        records: &'static mut [UnitRecord<Binding>],
    ) -> Self {
        assert!(Self::accepts(records));
        let scope = Scope(records.as_ptr().addr());
        let mut free = None;
        for (index, record) in records.iter_mut().enumerate().rev() {
            record.state = State::Vacant(free);
            free = Slot::new(index);
        }
        Self {
            scope,
            records,
            free,
            retired: 0,
            stacks: StackPool::new(stacks),
        }
    }

    pub(crate) fn vacant(&self) -> Result<UnitId, Error> {
        let slot = self.free.ok_or_else(|| {
            if self.retired == self.records.len() {
                Error::IdentityExhausted
            } else {
                Error::Capacity
            }
        })?;
        let record = &self.records[slot.index()];
        assert!(matches!(record.state, State::Vacant(_)));
        let generation = match record.generation {
            Some(previous) => previous.checked_add(1).expect("free record can issue"),
            None => NonZeroU64::MIN,
        };
        Ok(UnitId {
            scope: self.scope,
            slot,
            generation,
        })
    }

    /// Commit the observed free position in the same protected scope.
    pub(super) fn publish(
        &mut self,
        id: UnitId,
        binding: Binding,
        start: extern "C" fn(*const ()) -> !,
    ) -> Result<(), Error> {
        assert_eq!(self.vacant()?, id);
        let State::Vacant(next) = self.records[id.slot()].state else {
            unreachable!("validated free record");
        };
        let stack = self.stacks.claim(start).ok_or(Error::Capacity)?;
        self.free = next;
        self.records[id.slot()] = UnitRecord {
            generation: Some(id.generation),
            state: State::Live(Bound { binding, stack }),
        };
        Ok(())
    }

    fn record(&mut self, id: UnitId) -> Result<&mut State<Bound<Binding>>, Error> {
        if self.scope != id.scope {
            return Err(Error::Gone);
        }
        let slot = self.records.get_mut(id.slot()).ok_or(Error::Gone)?;
        if slot.generation != Some(id.generation)
            || matches!(slot.state, State::Vacant(_) | State::Retired)
        {
            return Err(Error::Gone);
        }
        Ok(&mut slot.state)
    }

    pub(crate) fn lifecycle(&mut self, id: UnitId) -> Result<Lifecycle, Error> {
        Ok(match self.record(id)? {
            State::Live(_) => Lifecycle::Live,
            State::Ending { .. } => Lifecycle::Ending,
            State::Ended(end) => Lifecycle::Ended(*end),
            State::Vacant(_) | State::Retired => unreachable!(),
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
        let State::Live(binding) = core::mem::replace(state, State::Vacant(None)) else {
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
        let free = self.free;
        let state = self.record(id)?;
        let State::Ended(end) = *state else {
            return Err(Error::NotEnded);
        };
        if id.generation.get() == u64::MAX {
            *state = State::Retired;
            self.retired += 1;
        } else {
            *state = State::Vacant(free);
            self.free = Some(id.slot);
        }
        Ok(end)
    }
}
