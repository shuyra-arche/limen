//! Original comparator requests and matching native completion.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Scope,
    Busy,
    Range,
    Unconfirmed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Idle,
    Programming {
        previous: Option<u64>,
        requested: u64,
    },
    Armed(u64),
    Masking(u64),
}

impl State {
    pub fn begin_program(
        &mut self,
        previous: Option<u64>,
        now: u64,
        requested: u64,
    ) -> Result<(), Failure> {
        if *self != previous.map_or(Self::Idle, Self::Armed) {
            return Err(Failure::Busy);
        }
        if requested <= now {
            return Err(Failure::Range);
        }
        // Preserve the earlier live stop. An expired comparison must
        // separately settle its original notification before rearming.
        if previous.is_some_and(|prior| now >= prior || requested > prior) {
            return Err(Failure::Busy);
        }
        *self = Self::Programming {
            previous,
            requested,
        };
        Ok(())
    }

    pub fn finish_program(&mut self, compare: u64, enabled: bool) -> Result<(), Failure> {
        let Self::Programming { requested, .. } = *self else {
            return Err(Failure::Busy);
        };
        if compare != requested || !enabled {
            return Err(Failure::Unconfirmed);
        }
        *self = Self::Armed(requested);
        Ok(())
    }

    pub fn begin_stop(&mut self, now: u64) -> Result<(), Failure> {
        let target = match *self {
            Self::Armed(target) if now >= target => target,
            Self::Masking(target) => target,
            _ => return Err(Failure::Busy),
        };
        *self = Self::Masking(target);
        Ok(())
    }

    pub fn finish_stop(&mut self, masked: bool) -> Result<(), Failure> {
        if !matches!(*self, Self::Masking(_)) {
            return Err(Failure::Busy);
        }
        if !masked {
            return Err(Failure::Unconfirmed);
        }
        *self = Self::Idle;
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../../../../../tests/metal/interrupt/aarch64/timer/programming.rs"]
mod tests;
