//! Arm generic timer description and native counter facts.

use core::num::NonZeroU32;

use crate::metal::{
    platform::firmware::devicetree::{DeviceTree, Error, Node, NodeId, cell},
    interrupt::gic::{GicV3, Trigger, ppi},
};

#[cfg(all(limen_target, target_arch = "aarch64"))]
mod native;

/// The original generic timer binding, including the domain of its virtual PPI.
#[derive(Clone, Copy, Debug)]
pub struct GenericTimer<'a> {
    pub source: NodeId<'a>,
    pub controller: GicV3<'a>,
    pub virtual_intid: u32,
    pub virtual_trigger: Trigger,
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
            timer = Some(Self::describe(tree, node)?);
        }
        Ok(timer)
    }

    fn describe(tree: &DeviceTree<'a>, node: Node<'_, 'a>) -> Result<Self, Error> {
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
        if !(2..=5).contains(&count) {
            return Err(Error::Unsupported);
        }
        let virtual_index = virtual_interrupt_index(node.property(b"interrupt-names")?, count)?;
        let domain = match handle {
            Some(handle) => tree.phandle(handle)?.ok_or(Error::Missing)?,
            None => interrupt_parent(tree, node)?,
        };
        let controller = GicV3::describe(domain)?;
        let mut decoded = [(0, Trigger::LevelHigh); 5];
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
            let (intid, kind) = ppi(ppi_bytes.try_into().unwrap())?;
            if decoded[..index]
                .iter()
                .any(|(earlier, _)| *earlier == intid)
            {
                return Err(Error::Conflict);
            }
            decoded[index] = (intid, kind);
        }
        let (virtual_intid, virtual_trigger) = decoded[virtual_index];
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
            controller,
            virtual_intid,
            virtual_trigger,
            always_on,
            frequency,
        })
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

fn interrupt_parent<'tree, 'a>(
    tree: &'tree DeviceTree<'a>,
    mut node: Node<'tree, 'a>,
) -> Result<Node<'tree, 'a>, Error> {
    loop {
        // An interrupt nexus changes the meaning of child cells and
        // requires a binding reader for that translation.
        if node.property(b"interrupt-map")?.is_some() {
            return Err(Error::Unsupported);
        }
        if let Some(handle) = cell(node.property(b"interrupt-parent")?)? {
            return tree.phandle(handle)?.ok_or(Error::Missing);
        }
        let parent = node.parent_id().ok_or(Error::Missing)?;
        node = tree.node(parent).ok_or(Error::Format)?;
        if node.property(b"#interrupt-cells")?.is_some() {
            return Ok(node);
        }
    }
}
