use crate::metal::sync::treaty::TurnMode;
use limen_treaty::model::PhysicalAddress;

use super::{Allocation, Allocator, Error, Retired};

impl Allocator {
    /// Convert one retired physical disposition into two independent ones.
    /// The original and both receiving positions stay intact on rejection.
    ///
    /// # Safety
    ///
    /// The receivers must be the actual durable positions of the new owners.
    /// The calling domain must have completed its original access and close
    /// handoff, with no additional shared role requiring this commit.
    pub unsafe fn split(
        &'static self,
        input: Retired,
        boundary: PhysicalAddress,
        receivers: &mut [Option<Allocation>; 2],
        mode: TurnMode,
    ) -> Result<(), (Retired, Error)> {
        if !boundary.is_aligned(self.config.granule_alignment()) {
            return Err((input, Error::Layout));
        }
        let Some((left, right)) = input.physical().split(boundary) else {
            return Err((input, Error::Layout));
        };
        let result = self.convert([&input], [left, right], receivers, mode);
        result.map_err(|error| (input, error))
    }

    /// Merge consecutive input dispositions without rewriting Buddy or
    /// changing their original funding. The output has its own prepaid return.
    ///
    /// # Safety
    ///
    /// The receiver must be the actual durable position of the new owner.
    /// Both calling domains must have completed the original close handoff,
    /// with no additional shared role requiring this commit.
    pub unsafe fn merge(
        &'static self,
        left: Retired,
        right: Retired,
        receiver: &mut Option<Allocation>,
        mode: TurnMode,
    ) -> Result<(), (Retired, Retired, Error)> {
        let fits = left.physical().end() == right.physical().start()
            && left.0.region() == right.0.region();
        if !fits {
            return Err((left, right, Error::Range));
        }
        let physical = left
            .physical()
            .merge(right.physical())
            .expect("consecutive allocation ranges");
        let result = self.convert(
            [&left, &right],
            [physical],
            core::array::from_mut(receiver),
            mode,
        );
        result.map_err(|error| (left, right, error))
    }
}
