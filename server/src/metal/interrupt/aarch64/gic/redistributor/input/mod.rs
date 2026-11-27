//! Original configuration and progress for the private inputs of one Frame.

pub mod description;
#[cfg(all(limen_target, target_arch = "aarch64"))]
mod native;
#[cfg(all(limen_target, target_arch = "aarch64"))]
pub use native::{Ppi, StoppedPpi};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Setup {
    pub(super) priority: u8,
    pub(super) inherited_priority: u8,
    pub(super) group: bool,
}

#[cfg_attr(
    not(any(test, all(limen_target, target_arch = "aarch64"))),
    expect(
        dead_code,
        reason = "private-input setup and progress are observed at native admission"
    )
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum State {
    Vacant,
    // Only hardware writes can establish any of the following states.
    Configuring(Setup),
    Masked(Setup),
    Enabling(Setup),
    Enabled(Setup),
    Masking(Setup),
    Restoring(Setup),
}

impl State {
    #[cfg_attr(
        not(any(test, all(limen_target, target_arch = "aarch64"))),
        expect(dead_code, reason = "only the actual Frame can retire a private input")
    )]
    fn retirement(self) -> Option<Setup> {
        match self {
            Self::Masked(setup)
            | Self::Enabling(setup)
            | Self::Enabled(setup)
            | Self::Masking(setup)
            | Self::Restoring(setup) => Some(setup),
            Self::Vacant | Self::Configuring(_) => None,
        }
    }
}

#[cfg(test)]
#[path = "../../../../../../../tests/metal/interrupt/aarch64/gic/private_input.rs"]
mod tests;
