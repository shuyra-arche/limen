//! GIC private peripheral interrupt descriptions consumed by the kernel.

use crate::metal::platform::description::devicetree::Error;

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
