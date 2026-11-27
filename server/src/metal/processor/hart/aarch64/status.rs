//! Actual EL1 stack and exception-mask fields.

pub(in crate::metal) const EL1: usize = 1 << 2;
pub(in crate::metal) const SP_EL1: usize = 1;
pub(super) const IRQ_FIQ: usize = 0b0011;
pub(in crate::metal) const ALL_EXCEPTIONS: usize = IRQ_FIQ | 0b1100;
const DAIF_MASK: usize = ALL_EXCEPTIONS << 6;

/// Interpret the supplied CurrentEL value, without reading a register.
pub const fn el1(bits: usize) -> bool {
    bits == EL1
}

/// Interpret supplied DAIF and SPSel values, not a live entry qualification.
pub const fn masked_stack(mask: usize, select: usize) -> bool {
    mask & DAIF_MASK == DAIF_MASK && select == SP_EL1
}

#[cfg(test)]
#[path = "../../../../../tests/metal/processor/hart/aarch64/status.rs"]
mod tests;
