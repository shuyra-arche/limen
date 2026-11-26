//! Arm architected counter description and native frequency facts.

use core::num::NonZeroU32;

use crate::metal::platform::description::devicetree::{DeviceTree, Error, NodeId, cell};

#[cfg(all(limen_target, target_arch = "aarch64"))]
mod native;

/// Counter facts from the architected timer node, independent of IRQ delivery.
#[derive(Clone, Copy, Debug)]
pub struct GenericCounter<'a> {
    source: NodeId<'a>,
    frequency: Option<NonZeroU32>,
}

impl<'a> GenericCounter<'a> {
    pub fn read(tree: &DeviceTree<'a>) -> Result<Option<Self>, Error> {
        let mut counter = None;
        for node in tree.nodes() {
            if !node.compatible(b"arm,armv8-timer")? || !node.enabled()? {
                continue;
            }
            if counter.is_some() {
                return Err(Error::Conflict);
            }
            let frequency = cell(node.property(b"clock-frequency")?)?
                .map(|frequency| NonZeroU32::new(frequency).ok_or(Error::Range))
                .transpose()?;
            counter = Some(Self {
                source: node.id(),
                frequency,
            });
        }
        Ok(counter)
    }

    pub fn source(self) -> NodeId<'a> {
        self.source
    }

    pub fn described_frequency(self) -> Option<NonZeroU32> {
        self.frequency
    }
}
