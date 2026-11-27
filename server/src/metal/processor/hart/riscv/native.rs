use core::arch::asm;

pub(in crate::metal::processor::hart) fn local_address() -> usize {
    let local;
    // A resumed continuation may run on another hart, so keep this read
    // ordered with calls that transfer its context.
    unsafe { asm!("mv {}, tp", out(reg) local, options(nostack)) };
    local
}

pub(in crate::metal::processor::hart) unsafe fn install_local(address: usize) {
    unsafe { asm!("mv tp, {}", in(reg) address, options(nostack)) };
}

pub(in crate::metal::processor::hart) unsafe fn mask_interrupts() -> usize {
    let previous: usize;
    unsafe { asm!("csrrci {}, sstatus, 2", out(reg) previous, options(nostack)) };
    previous & 2
}

pub(in crate::metal::processor::hart) unsafe fn restore_interrupts(previous: usize) {
    if previous != 0 {
        unsafe { asm!("csrsi sstatus, 2", options(nostack)) };
    }
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
