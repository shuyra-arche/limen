use core::arch::asm;

pub(super) fn local_address() -> usize {
    let local;
    // A resumed continuation may run on another hart, so keep this read
    // ordered with calls that transfer its context.
    unsafe { asm!("mv {}, tp", out(reg) local, options(nostack)) };
    local
}

pub(super) unsafe fn install_local(address: usize) {
    unsafe { asm!("mv tp, {}", in(reg) address, options(nostack)) };
}

pub(super) unsafe fn mask_interrupts() -> usize {
    let previous: usize;
    unsafe { asm!("csrrci {}, sstatus, 2", out(reg) previous, options(nostack)) };
    previous & 2
}

pub(super) unsafe fn restore_interrupts(previous: usize) {
    if previous != 0 {
        unsafe { asm!("csrsi sstatus, 2", options(nostack)) };
    }
}

pub(crate) fn stack_pointer() -> usize {
    let sp: usize;
    unsafe { asm!("mv {}, sp", out(reg) sp, options(nomem, nostack)) };
    sp
}
