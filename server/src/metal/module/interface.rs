//! The function declaration in an immutable module image.

use super::{image::ImageAddress, treaty::Contract};
use crate::metal::platform::description::devicetree::NodeId;

/// An entry declaration tied to the original immutable note.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Interface<'a> {
    contract: Contract<'a>,
    entry: ImageAddress,
    source: NodeId<'a>,
}

impl<'a> Interface<'a> {
    pub(super) const fn new(
        contract: Contract<'a>,
        entry: ImageAddress,
        source: NodeId<'a>,
    ) -> Self {
        Self {
            contract,
            entry,
            source,
        }
    }

    pub const fn contract(self) -> Contract<'a> {
        self.contract
    }

    pub const fn version(self) -> u32 {
        self.contract.version()
    }

    /// Link-time virtual address in the file-backed executable segment.
    pub const fn entry(self) -> ImageAddress {
        self.entry
    }

    pub const fn source(self) -> NodeId<'a> {
        self.source
    }
}
