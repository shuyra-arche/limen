//! The actual RV64 supervisor status fields used by the integer profile.

pub(in crate::metal) const SIE: usize = 1 << 1;
const FS_SHIFT: u32 = 13;
const VS_SHIFT: u32 = 9;
const XS_SHIFT: u32 = 15;
const FIELD: usize = 3;
const XS_MASK: usize = FIELD << XS_SHIFT;

/// FS and VS may be cleared; XS is a read-only summary of other extensions.
pub(in crate::metal) const WRITABLE_EXTENSIONS: usize = (FIELD << FS_SHIFT) | (FIELD << VS_SHIFT);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Extension {
    Off,
    Initial,
    Clean,
    Dirty,
}

impl Extension {
    const fn from_bits(bits: usize) -> Self {
        match bits & FIELD {
            0 => Self::Off,
            1 => Self::Initial,
            2 => Self::Clean,
            _ => Self::Dirty,
        }
    }
}

/// Interprets the SIE, FS, VS and XS fields of a supplied sstatus value.
/// Neither the value's provenance nor any live Hart authority is established.
#[derive(Clone, Copy)]
pub struct Status(usize);

impl Status {
    pub const fn from_bits(bits: usize) -> Self {
        Self(bits)
    }

    pub const fn masked(self) -> bool {
        self.0 & SIE == 0
    }

    /// The supplied value's floating-point state, not an ISA capability.
    pub fn floating(self) -> Extension {
        Extension::from_bits(self.0 >> FS_SHIFT)
    }

    /// The supplied value's vector state, not an ISA capability.
    pub fn vector(self) -> Extension {
        Extension::from_bits(self.0 >> VS_SHIFT)
    }

    /// Whether the read-only summary reports all additional state Off.
    pub fn additional_off(self) -> bool {
        self.0 & XS_MASK == 0
    }

    pub fn extensions_off(self) -> bool {
        // All writable extension fields must be Off, and the independent
        // read-only summary must also be Off. No write is authorized here.
        self.0 & WRITABLE_EXTENSIONS == 0 && self.additional_off()
    }
}

#[cfg(test)]
#[path = "../../../../../tests/metal/processor/hart/riscv64/status.rs"]
mod tests;
