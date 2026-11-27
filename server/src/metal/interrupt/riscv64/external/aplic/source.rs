//! Source records indexed by the hardware source number within one domain.

use super::{Failure, Mode};

/// A source's locally owned programming state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Source {
    Vacant,
    Configuring(Mode),
    Masked(Mode),
    Enabling(Mode),
    Enabled(Mode),
    Masking(Mode),
    Retiring(Mode),
}

pub(super) struct Sources<'a>(&'a mut [Source]);

impl<'a> Sources<'a> {
    pub(super) fn admit(storage: &'a mut [Source], count: u32) -> Result<Self, Failure> {
        if storage.len() != count as usize {
            return Err(Failure::Range);
        }
        if storage.iter().any(|source| *source != Source::Vacant) {
            return Err(Failure::Busy);
        }
        Ok(Self(storage))
    }

    pub(super) fn get(&self, source: u32) -> Result<Source, Failure> {
        let index = source.checked_sub(1).ok_or(Failure::Range)? as usize;
        self.0.get(index).copied().ok_or(Failure::Range)
    }

    pub(super) fn set(&mut self, source: u32, state: Source) {
        self.0[source as usize - 1] = state;
    }
}
