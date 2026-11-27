//! Arm Generic Timer interrupt binding. No comparator is enabled by discovery.

use crate::metal::{
    interrupt::aarch64::gic::internal::{PrivateInterrupt, PrivateInterrupts},
    platform::description::devicetree::{DeviceTree, Error, NodeId},
};

/// The virtual timer's checked role within one GIC domain.
/// This description is neither a live route nor a comparator programming permit.
#[derive(Clone, Copy, Debug)]
pub struct GenericTimer<'a> {
    source: NodeId<'a>,
    domain: NodeId<'a>,
    interrupt: PrivateInterrupt,
    always_on: bool,
}

impl<'a> GenericTimer<'a> {
    /// Interpret the timer role in the original, already checked GIC list.
    /// The caller supplies the actual list; this does not discover a controller.
    pub fn describe(
        tree: &DeviceTree<'a>,
        interrupts: &PrivateInterrupts<'a>,
    ) -> Result<Self, Error> {
        let source = interrupts.source();
        let node = tree.node(source).ok_or(Error::Missing)?;
        if !node.compatible(b"arm,armv8-timer")? || !node.enabled()? {
            return Err(Error::Unsupported);
        }
        let count = interrupts.entries().len();
        if !(2..=5).contains(&count) {
            return Err(Error::Unsupported);
        }
        let index = virtual_interrupt_index(node.property(b"interrupt-names")?, count)?;
        let always_on = match node.property(b"always-on")? {
            None => false,
            Some(b"") => true,
            Some(_) => return Err(Error::Format),
        };
        Ok(Self {
            source,
            domain: interrupts.controller().source(),
            interrupt: interrupts.entries()[index],
            always_on,
        })
    }

    pub fn source(self) -> NodeId<'a> {
        self.source
    }

    pub fn domain(self) -> NodeId<'a> {
        self.domain
    }

    pub fn virtual_interrupt(self) -> PrivateInterrupt {
        self.interrupt
    }

    /// The original timer power-domain declaration, not proof of continuity.
    pub fn always_on(self) -> bool {
        self.always_on
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
