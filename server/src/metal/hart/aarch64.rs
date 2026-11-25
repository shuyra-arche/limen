use core::arch::asm;

pub(crate) fn boot_identity() -> usize {
    let id: usize;
    unsafe { asm!("mrs {}, mpidr_el1", out(reg) id, options(nomem,nostack)) };
    id & 0xff00ffffff
}

pub(super) fn local_address() -> usize {
    let address;
    unsafe { asm!("mrs {}, tpidr_el1", out(reg) address, options(nostack)) };
    address
}

pub(super) unsafe fn install_local(address: usize) {
    unsafe { asm!("msr tpidr_el1, {}", "isb", in(reg) address, options(nostack)) };
}

pub(super) unsafe fn mask_interrupts() -> usize {
    let previous;
    unsafe { asm!("mrs {}, daif", "msr daifset, #3", out(reg) previous, options(nostack)) };
    previous
}

pub(super) unsafe fn restore_interrupts(previous: usize) {
    unsafe { asm!("msr daif, {}", in(reg) previous, options(nostack)) };
}

pub(crate) fn stack_pointer() -> usize {
    let sp: usize;
    unsafe { asm!("mov {}, sp", out(reg) sp, options(nomem, nostack)) };
    sp
}
