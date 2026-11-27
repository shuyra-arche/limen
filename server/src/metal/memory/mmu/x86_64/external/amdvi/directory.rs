//! Resident device directory geometry and device-table entry encoding.

use super::{Error, binding::Context};

const ADDRESS: u64 = 0x000f_ffff_ffff_f000;
// V and TV are set with a nonzero paging mode, but the DTE grants
// neither read nor write, so no ordinary device access may proceed.
pub(super) const DENY: [u64; 4] = [3 | (1 << 9), 0, 0, 0];

/// Resident, naturally aligned device table covering every 16-bit DeviceID.
#[repr(C, align(4096))]
pub struct Table(pub(super) [[u64; 4]; 65536]);

impl Table {
    pub const DENIED: Self = Self([DENY; 65536]);
}

impl Context {
    /// Qualify the supplied root and encode the requester’s DTE.
    pub(super) fn device_entry(self) -> Result<[u64; 4], Error> {
        if self.domain == 0
            || !(1..=3).contains(&self.levels)
            || self.root.get() == 0
            || self.root.get() & !ADDRESS != 0
        {
            return Err(Error::Address);
        }
        let root = self.root.get() | (u64::from(self.levels) << 9) | 3 | (3 << 61);
        Ok([root, u64::from(self.domain), 0, 0])
    }
}
