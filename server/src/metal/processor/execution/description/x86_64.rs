//! The basic x87/SSE save shape and the controls allowing its use.

/// Where to check the enabled state components after validating the base controls.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateControl {
    Legacy,
    Xcr0,
}

impl StateControl {
    pub const fn from_registers(edx: u32, ecx: u32, cr0: usize, cr4: usize) -> Option<Self> {
        const FXSR: u32 = 1 << 24;
        const SSE: u32 = 1 << 25;
        const SSE2: u32 = 1 << 26;
        const XSAVE: u32 = 1 << 26;
        const OSXSAVE: u32 = 1 << 27;
        const EM: usize = 1 << 2;
        const TS: usize = 1 << 3;
        const OS_FXSR: usize = 1 << 9;
        const OS_XSAVE: usize = 1 << 18;
        let features = FXSR | SSE | SSE2;
        if edx & features != features || cr0 & (EM | TS) != 0 || cr4 & OS_FXSR == 0 {
            return None;
        }
        match (cr4 & OS_XSAVE != 0, ecx & OSXSAVE != 0) {
            (false, false) => Some(Self::Legacy),
            (true, true) if ecx & XSAVE != 0 => Some(Self::Xcr0),
            _ => None,
        }
    }
}

pub const fn base_xcr0(bits: u64) -> bool {
    const X87: u64 = 1;
    const SSE: u64 = 1 << 1;
    bits == (X87 | SSE)
}
