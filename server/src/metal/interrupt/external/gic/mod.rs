//! GICv3 Distributor routes for ordinary shared peripheral interrupts.

use crate::metal::{
    interrupt::internal::gic::{GicV3, Trigger, interrupt_parent},
    platform::description::devicetree::{DeviceTree, Error, NodeId},
};

mod distributor;
pub use distributor::{Barrier, Failure, Spi, State};

/// One device's firmware binding to a specific Distributor SPI.
#[derive(Clone, Copy, Debug)]
pub struct Route<'a> {
    device: NodeId<'a>,
    controller: GicV3<'a>,
    intid: usize,
    trigger: Trigger,
}

impl<'a> Route<'a> {
    /// Select an interrupt by index. The ordinary GICv3 binding uses three
    /// cells. An extended list with mixed interrupt domains requires its
    /// individual parents' widths and is outside this profile.
    pub fn read(
        tree: &DeviceTree<'a>,
        device: NodeId<'a>,
        controller: GicV3<'a>,
        index: usize,
    ) -> Result<Self, Error> {
        let node = tree.node(device).ok_or(Error::Missing)?;
        if tree.node(controller.source()).is_none() || !node.enabled()? {
            return Err(Error::Conflict);
        }
        let extended = node.property(b"interrupts-extended")?;
        let bytes = if let Some(list) = extended {
            let (entries, remainder) = list.as_chunks::<16>();
            if !remainder.is_empty() {
                return Err(Error::Format);
            }
            for entry in entries {
                let handle = u32::from_be_bytes(*entry.first_chunk().unwrap());
                if tree.phandle(handle)?.ok_or(Error::Missing)?.id() != controller.source() {
                    return Err(Error::Conflict);
                }
            }
            &entries.get(index).ok_or(Error::Range)?[4..]
        } else {
            if interrupt_parent(tree, node)?.id() != controller.source() {
                return Err(Error::Conflict);
            }
            let (entries, remainder) = node
                .property(b"interrupts")?
                .ok_or(Error::Missing)?
                .as_chunks::<12>();
            if !remainder.is_empty() {
                return Err(Error::Format);
            }
            entries.get(index).ok_or(Error::Range)?.as_slice()
        };
        let [kind, number, flags] = bytes
            .as_chunks::<4>()
            .0
            .first_chunk::<3>()
            .unwrap()
            .map(u32::from_be_bytes);
        if kind != 0 {
            return Err(Error::Unsupported);
        }
        if number >= 988 {
            return Err(Error::Range);
        }
        let trigger = match flags {
            1 => Trigger::EdgeRising,
            4 => Trigger::LevelHigh,
            _ => return Err(Error::Unsupported),
        };
        Ok(Self {
            device,
            controller,
            intid: number as usize + 32,
            trigger,
        })
    }

    pub fn device(self) -> NodeId<'a> {
        self.device
    }
    pub fn intid(self) -> usize {
        self.intid
    }
    pub fn trigger(self) -> Trigger {
        self.trigger
    }
}
