use super::TrapStack;
use crate::metal::generic::boot::minor::{self, Minor, StartError};
use core::arch::naked_asm;

pub enum Startup {
    Sbi,
}
// The exception stack is private to this CPU and lives inside its resident record.
pub(crate) type MinorContext = TrapStack;
pub(crate) const fn minor_context() -> MinorContext {
    TrapStack::new()
}

pub(crate) unsafe fn wake_minor(
    slot: &Minor,
    hart: usize,
    Startup::Sbi: Startup,
) -> Result<(), StartError> {
    crate::metal::generic::firmware::sbi::start_hart(
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
        "csrw sscratch, zero",
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
        "li t0, {trap_top}",
        "add t0, a1, t0",
        "csrw sscratch, t0",
        "tail {loaded}",
        "3: wfi",
        "j 3b",
        vector = sym super::trap,
        state = const minor::STATE_OFFSET,
        published = const minor::PUBLISHED,
        entered = const minor::ENTERED,
        hart = const minor::HART_OFFSET,
        stack = const minor::STACK_OFFSET,
        trap_top = const core::mem::offset_of!(Minor, context) + size_of::<TrapStack>(),
        loaded = sym minor::arrive,
    );
}

impl Minor {
    pub(crate) fn trap_stack(&self) -> usize {
        self.context.top()
    }
    pub(in crate::metal) fn established(&self) -> bool {
        super::established_on(self.trap_stack())
    }
}
