//! Firmware bindings for GICv3 private peripheral inputs.

use crate::metal::interrupt::aarch64::gic::{GicV3, Trigger, interrupt_parent};
use crate::metal::platform::description::devicetree::{DeviceTree, Error, NodeId};

fn ppi(bytes: &[u8; 12]) -> Result<(u32, Trigger), Error> {
    let [kind, number, flags] = *bytes.as_chunks::<4>().0.first_chunk::<3>().unwrap();
    let [kind, number, flags] = [kind, number, flags].map(u32::from_be_bytes);
    if kind != 1 || number >= 16 {
        return Err(Error::Range);
    }
    let trigger = match flags {
        1 => Trigger::EdgeRising,
        4 => Trigger::LevelHigh,
        _ => return Err(Error::Unsupported),
    };
    Ok((16 + number, trigger))
}

/// One decoded private peripheral interrupt in the checked GIC domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PrivateInterrupt {
    pub intid: u32,
    pub trigger: Trigger,
}

/// The original source's private-interrupt list, in firmware order.
/// A PPI is identified within the actual domain; no interrupt is enabled here.
#[derive(Clone, Copy, Debug)]
pub struct PrivateInterrupts<'a> {
    source: NodeId<'a>,
    controller: GicV3<'a>,
    entries: [PrivateInterrupt; 16],
    count: usize,
}

impl<'a> PrivateInterrupts<'a> {
    pub fn read(
        tree: &DeviceTree<'a>,
        source: NodeId<'a>,
        controller: GicV3<'a>,
    ) -> Result<Self, Error> {
        let node = tree.node(source).ok_or(Error::Missing)?;
        let extended = node.property(b"interrupts-extended")?;
        let interrupts = node.property(b"interrupts")?;
        let (bytes, handle, width) = match (extended, interrupts) {
            (Some(bytes), _) => {
                let handle = bytes.first_chunk::<4>().ok_or(Error::Format)?;
                (bytes, Some(u32::from_be_bytes(*handle)), 16)
            }
            (None, Some(bytes)) => (bytes, None, 12),
            (None, None) => return Err(Error::Missing),
        };
        let interrupts = bytes.chunks_exact(width);
        if !interrupts.remainder().is_empty() {
            return Err(Error::Format);
        }
        let count = interrupts.len();
        if !(1..=16).contains(&count) {
            return Err(Error::Unsupported);
        }
        let domain = match handle {
            Some(handle) => tree.phandle(handle)?.ok_or(Error::Missing)?,
            None => interrupt_parent(tree, node)?,
        };
        if domain.id() != controller.source() {
            return Err(Error::Conflict);
        }
        let mut entries = [PrivateInterrupt {
            intid: 0,
            trigger: Trigger::LevelHigh,
        }; 16];
        for (index, specifier) in interrupts.enumerate() {
            let ppi_bytes = if let Some(handle) = handle {
                let (actual, specifier) = specifier.split_first_chunk::<4>().unwrap();
                if u32::from_be_bytes(*actual) != handle {
                    return Err(Error::Unsupported);
                }
                specifier
            } else {
                specifier
            };
            let (intid, trigger) = ppi(ppi_bytes.try_into().unwrap())?;
            if entries[..index]
                .iter()
                .any(|earlier| earlier.intid == intid)
            {
                return Err(Error::Conflict);
            }
            entries[index] = PrivateInterrupt { intid, trigger };
        }
        Ok(Self {
            source,
            controller,
            entries,
            count,
        })
    }

    pub fn source(&self) -> NodeId<'a> {
        self.source
    }

    pub fn controller(&self) -> GicV3<'a> {
        self.controller
    }

    pub fn entries(&self) -> &[PrivateInterrupt] {
        &self.entries[..self.count]
    }
}
