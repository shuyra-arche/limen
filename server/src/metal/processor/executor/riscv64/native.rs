use core::arch::asm;

use crate::metal::processor::description::riscv64::status::{SIE, Status};

pub(in crate::metal) fn status() -> Status {
    let bits;
    unsafe { asm!("csrr {}, sstatus", out(reg) bits, options(nomem, nostack)) };
    Status::from_bits(bits)
}

pub(in crate::metal::processor::executor) fn local_address() -> usize {
    let local;
    // A resumed continuation may run on another hart, so keep this read
    // ordered with calls that transfer its context.
    unsafe { asm!("mv {}, tp", out(reg) local, options(nostack)) };
    local
}

pub(in crate::metal::processor::executor) unsafe fn install_local(address: usize) {
    unsafe { asm!("mv tp, {}", in(reg) address, options(nostack)) };
}

pub(in crate::metal::processor::executor) unsafe fn mask_interrupts() -> usize {
    let previous: usize;
    unsafe {
        asm!("csrrci {}, sstatus, {mask}", out(reg) previous, mask = const SIE, options(nostack))
    };
    previous & SIE
}

pub(in crate::metal::processor::executor) unsafe fn restore_interrupts(previous: usize) {
    if previous != 0 {
        unsafe { asm!("csrsi sstatus, {mask}", mask = const SIE, options(nostack)) };
    }
}

/// Wait once in the admitted supervisor profile. The caller retains the
/// individually enabled wake sources and tests the original work afterward.
///
/// # Safety
/// WFI is permitted here and the processor's global IRQ dispatch is masked.
#[cfg(limen_firmware = "sbi")]
pub(in crate::metal) unsafe fn wait_for_interrupt() {
    unsafe { asm!("wfi", options(nostack)) };
}

pub(crate) fn stack_pointer() -> usize {
    let sp: usize;
    unsafe { asm!("mv {}, sp", out(reg) sp, options(nomem, nostack)) };
    sp
}

/// Native entry controls, not a software lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::metal) struct Entry {
    pub(in crate::metal) vector: usize,
    pub(in crate::metal) scratch: usize,
}

pub(in crate::metal) fn entry() -> Option<Entry> {
    let (vector, scratch): (usize, usize);
    unsafe {
        asm!(
            "csrr {vector}, stvec",
            "csrr {scratch}, sscratch",
            vector = out(reg) vector,
            scratch = out(reg) scratch,
            options(nomem, nostack),
        );
    }
    Some(Entry { vector, scratch })
}
