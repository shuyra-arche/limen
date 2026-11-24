//! Identity, continuation lifetime and the single terminal decision.

pub(crate) const CAPACITY: usize = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnitId {
    slot: usize,
    generation: u64,
}

impl UnitId {
    pub(crate) fn slot(self) -> usize {
        self.slot
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lifecycle {
    Live,
    Ending,
    Ended { value: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Busy,
    Capacity,
    Gone,
    State,
    Unsupported,
    Firmware(isize),
}

enum State<Binding> {
    Vacant,
    Live(Binding),
    Ending { value: u64, binding: Binding },
    Ended(u64),
}

struct Slot<Binding> {
    generation: u64,
    state: State<Binding>,
}

pub(crate) struct Units<Binding>([Slot<Binding>; CAPACITY]);

impl<Binding> Units<Binding> {
    pub(crate) const fn new() -> Self {
        Self(
            [const {
                Slot {
                    generation: 0,
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
                let generation = record.generation.checked_add(1)?;
                Some(UnitId { slot, generation })
            })
            .ok_or(Error::Capacity)
    }

    /// Commit the slot reserved by `vacant` in the same protected scope.
    pub(crate) fn publish(&mut self, id: UnitId, binding: Binding) {
        self.0[id.slot] = Slot {
            generation: id.generation,
            state: State::Live(binding),
        };
    }

    fn record(&mut self, id: UnitId) -> Result<&mut State<Binding>, Error> {
        let slot = &mut self.0[id.slot];
        if slot.generation != id.generation || matches!(slot.state, State::Vacant) {
            return Err(Error::Gone);
        }
        Ok(&mut slot.state)
    }

    pub(crate) fn lifecycle(&mut self, id: UnitId) -> Result<Lifecycle, Error> {
        Ok(match self.record(id)? {
            State::Live(_) => Lifecycle::Live,
            State::Ending { .. } => Lifecycle::Ending,
            State::Ended(value) => Lifecycle::Ended { value: *value },
            State::Vacant => unreachable!(),
        })
    }

    pub(crate) fn live(&mut self, id: UnitId) -> Result<&Binding, Error> {
        match self.record(id)? {
            State::Live(binding) => Ok(binding),
            _ => Err(Error::State),
        }
    }

    pub(crate) fn begin_end(&mut self, id: UnitId, value: u64) -> Result<(), Error> {
        let state = self.record(id)?;
        if !matches!(state, State::Live(_)) {
            return Err(Error::State);
        }
        let State::Live(binding) = core::mem::replace(state, State::Vacant) else {
            unreachable!()
        };
        *state = State::Ending { value, binding };
        Ok(())
    }

    /// The caller holds the protection that proved the Room entry released.
    pub(crate) fn finish_end(&mut self, id: UnitId) -> Result<Option<Binding>, Error> {
        let state = self.record(id)?;
        let value = match state {
            State::Ending { value, .. } => *value,
            State::Ended(_) => return Ok(None),
            _ => return Err(Error::State),
        };
        let State::Ending { binding, .. } = core::mem::replace(state, State::Ended(value)) else {
            unreachable!()
        };
        Ok(Some(binding))
    }

    pub(crate) fn reap(&mut self, id: UnitId) -> Result<u64, Error> {
        let state = self.record(id)?;
        let State::Ended(value) = *state else {
            return Err(Error::State);
        };
        *state = State::Vacant;
        Ok(value)
    }
}
