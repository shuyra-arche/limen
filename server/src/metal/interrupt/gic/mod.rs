//! GICv3 description and controller-specific native capability.

use crate::metal::firmware::devicetree::{Error, Node, NodeId, cell};
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

/// The trigger declared for a PPI in the GICv3 interrupt domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Trigger {
    EdgeRising,
    LevelHigh,
}

pub(crate) fn ppi(bytes: &[u8; 12]) -> Result<(u32, Trigger), Error> {
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
