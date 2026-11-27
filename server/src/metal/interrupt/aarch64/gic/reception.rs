//! The original CPU Interface claim through its exact priority and deactivation effects.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Scope,
    Busy,
    Special(u32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Combined,
    Split,
}

/// Each effect has a reservation before the hardware instruction. The owner
/// retains an interrupted operation in its original slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Empty,
    Reading,
    Claimed(u32),
    DroppingPriority(u32),
    DroppedPriority(u32),
    Deactivating(u32),
}

impl State {
    pub fn begin_read(&mut self) -> Result<(), Failure> {
        if *self != Self::Empty {
            return Err(Failure::Busy);
        }
        *self = Self::Reading;
        Ok(())
    }

    /// Settle the one reserved IAR operation using its native result.
    pub fn finish_read(&mut self, value: u64) -> Result<Option<u32>, Failure> {
        if *self != Self::Reading {
            return Err(Failure::Busy);
        }
        let id = (value & 0xff_ffff) as u32;
        if (1020..=1023).contains(&id) {
            *self = Self::Empty;
            return if id == 1023 {
                Ok(None)
            } else {
                Err(Failure::Special(id))
            };
        }
        *self = Self::Claimed(id);
        Ok(Some(id))
    }

    pub fn claim(self) -> Option<u32> {
        match self {
            Self::Claimed(id) | Self::DroppedPriority(id) => Some(id),
            _ => None,
        }
    }

    pub fn begin_priority_drop(&mut self, id: u32) -> Result<(), Failure> {
        if *self != Self::Claimed(id) {
            return Err(Failure::Busy);
        }
        *self = Self::DroppingPriority(id);
        Ok(())
    }

    /// A native EOIR completed. Ordinary interrupts in split mode still
    /// require their original DIR. LPIs do not acquire an active state.
    pub fn finish_priority_drop(&mut self, id: u32, mode: Mode) -> Result<(), Failure> {
        if *self != Self::DroppingPriority(id) {
            return Err(Failure::Busy);
        }
        *self = if mode == Mode::Split && id < 8192 {
            Self::DroppedPriority(id)
        } else {
            Self::Empty
        };
        Ok(())
    }

    pub fn begin_deactivation(&mut self, id: u32) -> Result<(), Failure> {
        if *self != Self::DroppedPriority(id) {
            return Err(Failure::Busy);
        }
        *self = Self::Deactivating(id);
        Ok(())
    }

    pub fn finish_deactivation(&mut self, id: u32) -> Result<(), Failure> {
        if *self != Self::Deactivating(id) {
            return Err(Failure::Busy);
        }
        *self = Self::Empty;
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../../../../tests/metal/interrupt/aarch64/gic/reception.rs"]
mod tests;
