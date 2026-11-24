use super::major::establish_supervisor;
use crate::metal::generic::minor;
use core::arch::naked_asm;

pub enum Startup {
    Sbi,
}
pub(crate) struct MinorContext;
pub(crate) const fn minor_context() -> MinorContext {
    MinorContext
}

pub(crate) unsafe fn wake_minor(
    slot: &crate::metal::generic::minor::Minor,
    hart: usize,
    Startup::Sbi: Startup,
) -> Result<(), crate::metal::generic::minor::StartError> {
    crate::metal::generic::firmware::sbi::start_hart(
        hart,
        minor as *const () as usize,
        core::ptr::from_ref(slot).expose_provenance(),
    )
    .map_err(crate::metal::generic::minor::StartError::Rejected)
}

#[unsafe(naked)]
#[unsafe(export_name = "_limen_secondary_start")]
unsafe extern "C" fn minor(hart: usize, descriptor: usize) -> ! {
    naked_asm!(
        establish_supervisor!(),
        "csrw sscratch, zero",
        "la t0, {vector}",
        "csrw stvec, t0",
        // Acquire the published descriptor before reading its stack addresses.
        "addi t0, a1, {state}",
        "li t1, {published}",
        "li t2, {entered}",
        "2: lr.w.aq t3, (t0)",
        "bne t3, t1, 3f",
        "sc.w t3, t2, (t0)",
        "bnez t3, 2b",
        "ld t0, {hart}(a1)",
        "bne a0, t0, 3f",
        "ld sp, {stack}(a1)",
        "ld t0, {trap_stack}(a1)",
        "csrw sscratch, t0",
        "tail {loaded}",
        "3: wfi",
        "j 3b",
        vector = sym super::major::trap,
        state = const minor::STATE_OFFSET,
        published = const minor::PUBLISHED,
        entered = const minor::ENTERED,
        hart = const minor::HART_OFFSET,
        stack = const minor::STACK_OFFSET,
        trap_stack = const minor::TRAP_STACK_OFFSET,
        loaded = sym minor::arrive,
    );
}
