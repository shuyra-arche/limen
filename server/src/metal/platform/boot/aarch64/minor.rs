//! Non-secure EL1 minor startup for an identity-mapped image.
use crate::metal::platform::boot::minor::{self, Minor, StartError};
use core::{
    arch::{asm, naked_asm},
    cell::UnsafeCell,
    mem::offset_of,
};

use crate::metal::platform::firmware::psci::Conduit;
#[derive(Clone, Copy)]
pub enum Startup {
    Psci(Conduit),
}

#[repr(C)]
#[derive(Default)]
pub(crate) struct Registers {
    mair: usize,
    tcr: usize,
    ttbr0: usize,
    ttbr1: usize,
    cpacr: usize,
    sctlr: usize,
}
pub(crate) type MinorContext = UnsafeCell<Registers>;
pub(crate) const fn minor_context() -> MinorContext {
    UnsafeCell::new(Registers {
        mair: 0,
        tcr: 0,
        ttbr0: 0,
        ttbr1: 0,
        cpacr: 0,
        sctlr: 0,
    })
}

pub(crate) unsafe fn wake_minor(
    slot: &Minor,
    hart: usize,
    Startup::Psci(conduit): Startup,
) -> Result<(), StartError> {
    let mut regs = Registers::default();
    unsafe {
        asm!("mrs {}, mair_el1","mrs {}, tcr_el1","mrs {}, ttbr0_el1",
            "mrs {}, ttbr1_el1","mrs {}, cpacr_el1","mrs {}, sctlr_el1",
            out(reg) regs.mair,out(reg) regs.tcr,out(reg) regs.ttbr0,out(reg) regs.ttbr1,
            out(reg) regs.cpacr,out(reg) regs.sctlr,options(nomem,nostack));
        slot.context.get().write(regs);
    }
    let ptr = core::ptr::from_ref(slot).expose_provenance();
    // CPU_ON enters with translation off. Publish the descriptor and saved
    // controls to the point of coherency before the target reads either.
    let ctr: usize;
    unsafe { asm!("mrs {}, ctr_el0",out(reg) ctr,options(nomem,nostack)) };
    let line = 4 << ((ctr >> 16) & 15);
    for address in ((ptr & !(line - 1))..ptr + size_of::<Minor>()).step_by(line) {
        unsafe { asm!("dc cvac, {}", in(reg) address,options(nostack)) }
    }
    unsafe { conduit.cpu_on(hart, minor as *const (), ptr) }.map_err(StartError::Rejected)
}

#[unsafe(naked)]
unsafe extern "C" fn minor(slot: *const Minor) -> ! {
    naked_asm!(
        "msr daifset, #15",
        "mrs x2, CurrentEL",
        "cmp x2, #4",
        "b.ne 3f",
        "msr spsel, #1",
        "msr tpidr_el1, xzr",
        "mov x1, x0",
        "mrs x0, mpidr_el1",
        "mov x2, #0xffffff",
        "movk x2, #0xff, lsl #32",
        "and x0, x0, x2",
        // Acquire the publication before using the identity or saved EL1 state.
        "add x2, x1, #{state}",
        "ldar w3, [x2]",
        "cmp w3, #{published}",
        "b.ne 3f",
        "ldr x2, [x1, #{hart}]",
        "cmp x0, x2",
        "b.ne 3f",
        "mov x2, #{ctx}",
        "add x2, x1, x2",
        "ldp x3, x4, [x2]",
        "msr mair_el1, x3",
        "msr tcr_el1, x4",
        "ldp x3, x4, [x2,#16]",
        "msr ttbr0_el1, x3",
        "msr ttbr1_el1, x4",
        "ldp x3, x4, [x2,#32]",
        "msr cpacr_el1, x3",
        "isb",
        "tlbi vmalle1",
        "dsb sy",
        "isb",
        "msr sctlr_el1, x4",
        "isb",
        "add x2, x1, #{state}",
        "2: ldaxr w3, [x2]",
        "cmp w3, #{published}",
        "b.ne 3f",
        "mov w4, #{entered}",
        "stxr w3, w4, [x2]",
        "cbnz w3, 2b",
        "ldr x2,[x1,#{stack}]",
        "mov sp,x2",
        "adrp x2,{vectors}",
        "add x2,x2,:lo12:{vectors}",
        "msr vbar_el1,x2",
        "isb",
        "b {arrive}",
        "3: wfe",
        "b 3b",
        ctx = const offset_of!(Minor,context),
        hart = const minor::HART_OFFSET,
        stack = const minor::STACK_OFFSET,
        state = const minor::STATE_OFFSET,
        published = const minor::PUBLISHED,
        entered = const minor::ENTERED,
        vectors = sym super::vectors,
        arrive = sym minor::arrive,
    )
}

impl Minor {
    pub(in crate::metal) fn established(&self, hart: usize) -> bool {
        hart == crate::metal::processor::hart::aarch64::native::boot_identity()
            && super::established()
    }
}
