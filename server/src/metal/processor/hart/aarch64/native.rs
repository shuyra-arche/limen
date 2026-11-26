use core::arch::asm;

use super::description::{AFFINITY_MASK, Aarch64Cpus, CpuCandidate};
use crate::metal::platform::description::devicetree::Error;

impl<'tree, 'a> Aarch64Cpus<'tree, 'a> {
    /// Find the enabled declaration belonging to the executing PE.
    ///
    /// # Safety
    /// Run in a privileged AArch64 context that can read MPIDR_EL1.
    pub unsafe fn current(&self) -> Result<CpuCandidate<'a>, Error> {
        let current = boot_identity();
        self.candidates()
            .find(|candidate| candidate.affinity == current && candidate.enabled)
            .ok_or(Error::Missing)
    }
}

pub(crate) fn boot_identity() -> usize {
    let id: usize;
    unsafe { asm!("mrs {}, mpidr_el1", out(reg) id, options(nomem,nostack)) };
    id & AFFINITY_MASK as usize
}

pub(in crate::metal::processor::hart) fn local_address() -> usize {
    let address;
    unsafe { asm!("mrs {}, tpidr_el1", out(reg) address, options(nostack)) };
    address
}

pub(in crate::metal::processor::hart) unsafe fn install_local(address: usize) {
    unsafe { asm!("msr tpidr_el1, {}", "isb", in(reg) address, options(nostack)) };
}

pub(in crate::metal::processor::hart) unsafe fn mask_interrupts() -> usize {
    let previous;
    unsafe { asm!("mrs {}, daif", "msr daifset, #3", out(reg) previous, options(nostack)) };
    previous
}

pub(in crate::metal::processor::hart) unsafe fn restore_interrupts(previous: usize) {
    unsafe { asm!("msr daif, {}", in(reg) previous, options(nostack)) };
}

pub(crate) fn stack_pointer() -> usize {
    let sp: usize;
    unsafe { asm!("mov {}, sp", out(reg) sp, options(nomem, nostack)) };
    sp
}
