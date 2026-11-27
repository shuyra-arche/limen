//! Actual CPACR_EL1 fields required by the base FP/SIMD save shape.

const FPEN_SHIFT: u32 = 20;
const ZEN_SHIFT: u32 = 16;
const SMEN_SHIFT: u32 = 24;
const FIELD: usize = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Access {
    Trapped,
    Privileged,
    Open,
}

impl Access {
    fn from_bits(bits: usize) -> Self {
        match bits & FIELD {
            1 => Self::Privileged,
            3 => Self::Open,
            _ => Self::Trapped,
        }
    }
}

/// Observed permission required by the current save shape, not proof that
/// the PE implements extensions or can preserve an arbitrary vector state.
pub(in crate::metal) fn base_simd(bits: usize) -> bool {
    matches!(
        Access::from_bits(bits >> FPEN_SHIFT),
        Access::Privileged | Access::Open
    ) && (bits >> ZEN_SHIFT) & FIELD == 0
        && (bits >> SMEN_SHIFT) & FIELD == 0
}

#[cfg(test)]
#[path = "../../../../../tests/metal/processor/execution/aarch64.rs"]
mod tests;
