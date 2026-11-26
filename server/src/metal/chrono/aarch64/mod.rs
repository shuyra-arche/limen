//! Arm generic timer description and native counter facts.

use core::num::NonZeroU32;

use crate::metal::platform::description::devicetree::{DeviceTree, Error, Node, NodeId, cell};

#[cfg(all(limen_target, target_arch = "aarch64"))]
mod native;

/// The generic timer's own description, independent of its interrupt controller.
#[derive(Clone, Copy, Debug)]
pub struct GenericTimer<'a> {
    pub source: NodeId<'a>,
    pub always_on: bool,
    frequency: Option<NonZeroU32>,
}

impl<'a> GenericTimer<'a> {
    pub fn read(tree: &DeviceTree<'a>) -> Result<Option<Self>, Error> {
        let mut timer = None;
        for node in tree.nodes() {
            if !node.compatible(b"arm,armv8-timer")? || !node.enabled()? {
                continue;
            }
            if timer.is_some() {
                return Err(Error::Conflict);
            }
            timer = Some(Self::describe(node)?);
        }
        Ok(timer)
    }

    fn describe(node: Node<'_, 'a>) -> Result<Self, Error> {
        let always_on = match node.property(b"always-on")? {
            None => false,
            Some(b"") => true,
            Some(_) => return Err(Error::Format),
        };
        let frequency = cell(node.property(b"clock-frequency")?)?
            .map(|frequency| NonZeroU32::new(frequency).ok_or(Error::Range))
            .transpose()?;
        Ok(Self {
            source: node.id(),
            always_on,
            frequency,
        })
    }

    /// Identify the virtual timer's index in the checked interrupt list of
    /// this exact source. The actual interrupt domain interprets the cells.
    pub fn virtual_interrupt_index(
        self,
        tree: &DeviceTree<'a>,
        count: usize,
    ) -> Result<usize, Error> {
        let node = tree.node(self.source).ok_or(Error::Missing)?;
        if !(2..=5).contains(&count) {
            return Err(Error::Unsupported);
        }
        virtual_interrupt_index(node.property(b"interrupt-names")?, count)
    }

    pub fn described_frequency(self) -> Option<NonZeroU32> {
        self.frequency
    }
}

fn virtual_interrupt_index(names: Option<&[u8]>, count: usize) -> Result<usize, Error> {
    // The architected timer binding orders these sources by role. A list
    // without names uses the original order, including the secure physical PPI.
    const NAMES: [&[u8]; 5] = [b"sec-phys", b"phys", b"virt", b"hyp-phys", b"hyp-virt"];
    match names {
        None if count >= 3 => Ok(2),
        None => Err(Error::Missing),
        Some(names) => {
            let names = names.strip_suffix(b"\0").ok_or(Error::Format)?;
            let names = names.split(|byte| *byte == 0);
            let start = usize::from(names.clone().next() != Some(NAMES[0]));
            let end = start + count;
            if end > NAMES.len() || !names.eq(NAMES[start..end].iter().copied()) {
                return Err(Error::Format);
            }
            let index = 2 - start;
            (index < count).then_some(index).ok_or(Error::Missing)
        }
    }
}
