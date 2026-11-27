use crate::metal::platform::boot::minor::{self, Minor, StartError};
use crate::metal::platform::boot::trap as diagnostic;
use crate::metal::platform::firmware::sbi::HartService;
use core::arch::naked_asm;

pub enum Startup {
    Sbi(HartService),
}
pub(crate) struct MinorContext;
pub(crate) const fn minor_context() -> MinorContext {
    MinorContext
}

pub(crate) unsafe fn wake_minor(
    slot: &Minor,
    hart: usize,
    Startup::Sbi(service): Startup,
) -> Result<(), StartError> {
    slot.publish();
    service
        .start(
            hart,
            minor as *const () as usize,
            core::ptr::from_ref(slot).expose_provenance(),
        )
        .map_err(StartError::Rejected)
}

#[unsafe(naked)]
unsafe extern "C" fn minor(hart: usize, descriptor: usize) -> ! {
    naked_asm!(
        establish_supervisor!(),
        "la t0, {top}",
        "csrw sscratch, t0",
        "la t0, {vector}",
        "csrw stvec, t0",
        // Acquire the published descriptor before reading its stack addresses.
        "addi t0, a1, {state}",
        "li t1, {published}",
        "li t2, {entered}",
        "lw t3, (t0)",
        "fence r, rw",
        "bne t3, t1, 3f",
        "ld t3, {hart}(a1)",
        "bne a0, t3, 3f",
        "2: lr.w.aq t3, (t0)",
        "bne t3, t1, 3f",
        "sc.w t3, t2, (t0)",
        "bnez t3, 2b",
        "ld sp, {stack}(a1)",
        "tail {loaded}",
        "3: wfi",
        "j 3b",
        vector = sym super::trap,
        state = const minor::STATE_OFFSET,
        published = const minor::PUBLISHED,
        entered = const minor::ENTERED,
        hart = const minor::HART_OFFSET,
        stack = const minor::STACK_OFFSET,
        top = sym diagnostic::TOP,
        loaded = sym minor::arrive,
    );
}

impl Minor {
    pub(in crate::metal) fn established(&self, _hart: usize) -> bool {
        // The entry ABI and trampoline already checked the incoming Hart.
        super::established()
    }
}
