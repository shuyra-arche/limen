use core::arch::asm;

use crate::metal::{
    platform::description::devicetree::Error,
    processor::description::aarch64::{
        cpu::{AFFINITY_MASK, Aarch64Cpus, CpuCandidate},
        status,
    },
};

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

pub(in crate::metal::processor::executor) fn local_address() -> usize {
    let address;
    unsafe { asm!("mrs {}, tpidr_el1", out(reg) address, options(nostack)) };
    address
}

pub(in crate::metal::processor::executor) unsafe fn install_local(address: usize) {
    unsafe { asm!("msr tpidr_el1, {}", "isb", in(reg) address, options(nostack)) };
}

pub(in crate::metal::processor::executor) unsafe fn mask_interrupts() -> usize {
    let previous;
    unsafe {
        asm!(
            "mrs {}, daif",
            "msr daifset, #{mask}",
            out(reg) previous,
            mask = const status::IRQ_FIQ,
            options(nostack),
        )
    };
    previous
}

pub(in crate::metal::processor::executor) unsafe fn restore_interrupts(previous: usize) {
    unsafe { asm!("msr daif, {}", in(reg) previous, options(nostack)) };
}

/// Wait once in the admitted EL1 profile with local IRQ/FIQ masks set.
///
/// # Safety
/// WFI is permitted, and the original wake source remains enabled at the
/// interrupt controller until the ordinary owner rechecks it.
pub(in crate::metal) unsafe fn wait_for_interrupt() {
    unsafe { asm!("dsb sy", "wfi", options(nostack, preserves_flags)) };
}

pub(crate) fn stack_pointer() -> usize {
    let sp: usize;
    unsafe { asm!("mov {}, sp", out(reg) sp, options(nomem, nostack)) };
    sp
}

/// Actual EL1 vector base. Observe only after confirming the exception level.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::metal) struct Entry {
    pub(in crate::metal) vector: usize,
}

fn at_el1() -> bool {
    let level: usize;
    unsafe { asm!("mrs {}, CurrentEL", out(reg) level, options(nostack, nomem, preserves_flags)) };
    status::el1(level)
}

/// Actual ordinary kernel stack selection and all four exception masks.
/// This is an observation, not a local-protection or execution receipt.
pub(in crate::metal) fn masked_stack() -> bool {
    if !at_el1() {
        return false;
    }
    let (mask, select): (usize, usize);
    unsafe {
        asm!("mrs {}, DAIF", "mrs {}, SPSel", out(reg) mask, out(reg) select,
             options(nostack, nomem, preserves_flags))
    };
    status::masked_stack(mask, select)
}

pub(in crate::metal) fn entry() -> Option<Entry> {
    if !at_el1() {
        return None;
    }
    let vector: usize;
    unsafe { asm!("mrs {}, VBAR_EL1", out(reg) vector, options(nostack, nomem, preserves_flags)) };
    Some(Entry { vector })
}
