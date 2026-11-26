//! Device-tree bindings for the GICv3 domain and Arm generic timer.

use super::devicetree::{DeviceTree, Error, Node, NodeId};
use crate::metal::address::PhysicalRange;
use core::{arch::asm, num::NonZeroU32};

/// The windows of the GICv3 interrupt domain named by the timer's source.
#[derive(Clone, Copy, Debug)]
pub struct GicV3<'a> {
    pub source: NodeId<'a>,
    pub distributor: PhysicalRange,
    pub redistributor: PhysicalRange,
}

impl<'a> GicV3<'a> {
    fn describe(node: Node<'_, 'a>) -> Result<Self, Error> {
        if !node.compatible(b"arm,gic-v3")?
            || !node.enabled()?
            || node.property(b"interrupt-controller")? != Some(b"")
            || node.property(b"#interrupt-cells")? != Some(&3u32.to_be_bytes())
        {
            return Err(Error::Unsupported);
        }
        if cell(node.property(b"#redistributor-regions")?)?.is_some_and(|count| count != 1) {
            return Err(Error::Unsupported);
        }
        let distributor = node.register(0)?;
        let redistributor = node.register(1)?;
        if distributor.overlaps(redistributor)
            || !distributor.start().get().is_multiple_of(0x1_0000)
            || !redistributor.start().get().is_multiple_of(0x1_0000)
            || distributor.end().get() - distributor.start().get() < 0x1_0000
            || redistributor.end().get() - redistributor.start().get() < 0x2_0000
        {
            return Err(Error::Range);
        }
        Ok(Self {
            source: node.id(),
            distributor,
            redistributor,
        })
    }

    /// Inspect the executing PE's architectural GIC CPU interface feature.
    ///
    /// # Safety
    /// Run at an exception level allowed to read ID_AA64PFR0_EL1.
    pub unsafe fn cpu_interface_available() -> bool {
        let features: u64;
        unsafe { asm!("mrs {}, ID_AA64PFR0_EL1", out(reg) features, options(nomem, nostack)) };
        matches!((features >> 24) & 0xf, 1 | 3)
    }
}

/// The trigger declared for a PPI in the GICv3 interrupt domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Trigger {
    EdgeRising,
    LevelHigh,
}

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

    /// Read the frequency from the current PE and compare an explicit DT value.
    ///
    /// # Safety
    /// Run at an exception level allowed to read CNTFRQ_EL0.
    pub unsafe fn counter_frequency(self) -> Result<NonZeroU32, Error> {
        let frequency: u64;
        unsafe { asm!("mrs {}, CNTFRQ_EL0", out(reg) frequency, options(nomem, nostack)) };
        let frequency = u32::try_from(frequency)
            .ok()
            .and_then(NonZeroU32::new)
            .ok_or(Error::Range)?;
        if self
            .frequency
            .is_some_and(|described| described != frequency)
        {
            return Err(Error::Conflict);
        }
        Ok(frequency)
    }
}

fn cell(value: Option<&[u8]>) -> Result<Option<u32>, Error> {
    value
        .map(|bytes| {
            bytes
                .try_into()
                .map(u32::from_be_bytes)
                .map_err(|_| Error::Format)
        })
        .transpose()
}

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
