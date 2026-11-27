//! The actual RV64 supervisor status fields used by the integer profile.

pub(in crate::metal) const SIE: usize = 1 << 1;
const FS_SHIFT: u32 = 13;
const VS_SHIFT: u32 = 9;
const XS_SHIFT: u32 = 15;
const FIELD: usize = 3;
const XS_MASK: usize = FIELD << XS_SHIFT;

/// FS and VS may be cleared; XS is a read-only summary of other extensions.
pub(in crate::metal) const WRITABLE_EXTENSIONS: usize =
    (FIELD << FS_SHIFT) | (FIELD << VS_SHIFT);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Extension {
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

/// One actual sstatus observation. Not an execution admission or saved context.
#[derive(Clone, Copy)]
pub(in crate::metal) struct Status(usize);

impl Status {
    pub(in crate::metal) const fn from_bits(bits: usize) -> Self {
        Self(bits)
    }

    pub(in crate::metal) const fn masked(self) -> bool {
        self.0 & SIE == 0
    }

    fn floating(self) -> Extension {
        Extension::from_bits(self.0 >> FS_SHIFT)
    }

    fn vector(self) -> Extension {
        Extension::from_bits(self.0 >> VS_SHIFT)
    }

    fn additional_off(self) -> bool {
        self.0 & XS_MASK == 0
    }

    pub(in crate::metal) fn extensions_off(self) -> bool {
        self.floating() == Extension::Off
            && self.vector() == Extension::Off
            && self.additional_off()
    }
}

#[cfg(test)]
#[path = "../../../../../tests/metal/processor/hart/riscv/status.rs"]
mod tests;
