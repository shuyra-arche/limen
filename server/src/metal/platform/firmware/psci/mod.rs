//! PSCI protocol declarations and the advertised conduit.

use crate::metal::platform::description::treaty::devicetree::NodeId;

#[cfg(all(limen_target, target_arch = "aarch64"))]
pub(in crate::metal::platform::firmware) mod native;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conduit {
    Hvc,
    Smc,
}

/// The original device-tree PSCI node and the conduit it describes.
#[derive(Clone, Copy, Debug)]
pub struct PsciDescription<'a> {
    pub(in crate::metal::platform::firmware) source: NodeId<'a>,
    pub(in crate::metal::platform::firmware) conduit: Conduit,
    pub(in crate::metal::platform::firmware) minimum: PsciVersion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PsciVersion {
    pub major: u16,
    pub minor: u16,
}

/// An admitted PSCI call endpoint. Actual target ownership remains separate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Psci {
    pub(in crate::metal::platform::firmware) conduit: Conduit,
    pub(in crate::metal::platform::firmware) version: PsciVersion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PsciError {
    Firmware(i32),
    Version,
    Affinity,
}
