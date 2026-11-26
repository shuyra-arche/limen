//! GICv3 description and controller-specific native capability.

use crate::metal::platform::description::devicetree::{Error, Node, NodeId, cell};
use limen_treaty::model::PhysicalRange;

#[cfg(all(limen_target, target_arch = "aarch64"))]
mod native;

/// The windows of the GICv3 interrupt domain named by the timer's source.
#[derive(Clone, Copy, Debug)]
pub struct GicV3<'a> {
    pub source: NodeId<'a>,
    pub distributor: PhysicalRange,
    pub redistributor: PhysicalRange,
}

impl<'a> GicV3<'a> {
    pub(crate) fn describe(node: Node<'_, 'a>) -> Result<Self, Error> {
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
