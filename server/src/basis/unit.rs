//! Identity, continuation lifetime and the single terminal decision.

use core::num::NonZeroU64;

use crate::metal::identity::Slot;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnitId {
    slot: Slot,
    generation: NonZeroU64,
}

impl UnitId {
    fn slot(self) -> usize {
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
    Gone,
    State,
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

pub(crate) struct Units<Binding, const CAPACITY: usize>([Record<Binding>; CAPACITY]);

impl<Binding, const CAPACITY: usize> Units<Binding, CAPACITY> {
    pub(crate) const fn new() -> Self {
        Self(
            [const {
                Record {
                    generation: None,
                    state: State::Vacant,
                }
            }; CAPACITY],
        )
    }

    pub(crate) fn vacant(&self) -> Result<UnitId, Error> {
        self.0
            .iter()
            .enumerate()
            .find_map(|(slot, record)| {
                if !matches!(record.state, State::Vacant) {
                    return None;
                }
                let slot = Slot::new(slot)?;
                let generation = match record.generation {
                    Some(previous) => previous.checked_add(1)?,
                    None => NonZeroU64::MIN,
                };
                Some(UnitId { slot, generation })
            })
            .ok_or(Error::Capacity)
    }

    /// Commit the slot reserved by `vacant` in the same protected scope.
    pub(crate) fn publish(&mut self, id: UnitId, binding: Binding) {
        self.0[id.slot()] = Record {
            generation: Some(id.generation),
            state: State::Live(binding),
        };
    }

    fn record(&mut self, id: UnitId) -> Result<&mut State<Binding>, Error> {
        let slot = self.0.get_mut(id.slot()).ok_or(Error::Gone)?;
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

    pub(crate) fn live(&mut self, id: UnitId) -> Result<&Binding, Error> {
        match self.record(id)? {
            State::Live(binding) => Ok(binding),
            _ => Err(Error::State),
        }
    }

    pub(crate) fn begin_end(&mut self, id: UnitId, end: End) -> Result<(), Error> {
        let state = self.record(id)?;
        match state {
            State::Live(_) => {}
            State::Ending { end: accepted, .. } | State::Ended(accepted) if *accepted == end => {
                return Ok(());
            }
            _ => return Err(Error::State),
        }
        let State::Live(binding) = core::mem::replace(state, State::Vacant) else {
            unreachable!()
        };
        *state = State::Ending { end, binding };
        Ok(())
    }

    /// The caller holds the protection that proved the Room entry released.
    pub(crate) fn finish_end(&mut self, id: UnitId) -> Result<Option<Binding>, Error> {
        let state = self.record(id)?;
        let end = match state {
            State::Ending { end, .. } => *end,
            State::Ended(_) => return Ok(None),
            _ => return Err(Error::State),
        };
        let State::Ending { binding, .. } = core::mem::replace(state, State::Ended(end)) else {
            unreachable!()
        };
        Ok(Some(binding))
    }

    pub(crate) fn reap(&mut self, id: UnitId) -> Result<End, Error> {
        let state = self.record(id)?;
        let State::Ended(end) = *state else {
            return Err(Error::State);
        };
        *state = State::Vacant;
        Ok(end)
    }
}
