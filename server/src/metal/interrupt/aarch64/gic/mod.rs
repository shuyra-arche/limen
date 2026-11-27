//! One physical GICv3 domain with its shared firmware identity.

use crate::metal::platform::description::devicetree::{DeviceTree, Error, Node, NodeId, cell};
use limen_treaty::model::PhysicalRange;

#[cfg(all(limen_target, target_arch = "aarch64"))]
mod native;

/// Interpret ID_AA64PFR0_EL1.GIC for supported system register interfaces.
pub const fn system_register_interface(features: u64) -> bool {
    const SHIFT: u32 = 24;
    const GIC_V3_V4: u64 = 1;
    const GIC_V4_1: u64 = 3;
    matches!((features >> SHIFT) & 0xf, GIC_V3_V4 | GIC_V4_1)
}

/// Checked windows of one physical GICv3 domain, including its distributor.
/// This description confers no register access or external routing authority.
#[derive(Clone, Copy, Debug)]
pub struct GicV3<'a> {
    source: NodeId<'a>,
    distributor: PhysicalRange,
    redistributor: PhysicalRange,
}

impl<'a> GicV3<'a> {
    /// Select the unique enabled GICv3 of this description profile.
    pub fn read(tree: &DeviceTree<'a>) -> Result<Option<Self>, Error> {
        let mut found = None;
        for node in tree.nodes() {
            if !node.compatible(b"arm,gic-v3")? || !node.enabled()? {
                continue;
            }
            if found.is_some() {
                return Err(Error::Conflict);
            }
            found = Some(Self::describe(node)?);
        }
        Ok(found)
    }

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

    pub fn source(self) -> NodeId<'a> {
        self.source
    }

    pub fn distributor(self) -> PhysicalRange {
        self.distributor
    }

    pub fn redistributor(self) -> PhysicalRange {
        self.redistributor
    }
}

pub mod external;
pub mod internal;

/// The trigger declared for a peripheral interrupt in the GICv3 interrupt domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Trigger {
    EdgeRising,
    LevelHigh,
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
