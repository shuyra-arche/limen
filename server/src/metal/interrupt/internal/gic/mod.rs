//! GICv3 local reception facts and private interrupt bindings.

use crate::metal::platform::description::devicetree::{DeviceTree, Error, Node, NodeId, cell};
use limen_treaty::model::PhysicalRange;

#[cfg(all(limen_target, target_arch = "aarch64"))]
mod native;

/// Checked windows of one physical GICv3 domain, including its distributor.
/// This description confers no register access or external routing authority.
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
            || distributor.length().get() < 0x1_0000
            || redistributor.length().get() < 0x2_0000
        {
            return Err(Error::Range);
        }
        Ok(Self {
            source: node.id(),
            distributor,
            redistributor,
        })
    }
}

/// The trigger declared for a PPI in the GICv3 interrupt domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Trigger {
    EdgeRising,
    LevelHigh,
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
    pub fn read(tree: &DeviceTree<'a>, source: NodeId<'a>) -> Result<Self, Error> {
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
        let controller = GicV3::describe(domain)?;
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
            if entries[..index].iter().any(|earlier| earlier.intid == intid) {
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
