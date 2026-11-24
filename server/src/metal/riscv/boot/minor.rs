use super::major::establish_supervisor;
use crate::metal::generic::minor;
use core::arch::naked_asm;

pub enum Startup {
    Sbi,
}
// The exception stack is private to this CPU and lives inside its resident record.
const TRAP_BYTES: usize = 4096;
#[repr(C, align(16))]
pub(crate) struct MinorContext(core::cell::UnsafeCell<[core::mem::MaybeUninit<u8>; TRAP_BYTES]>);
pub(crate) const fn minor_context() -> MinorContext {
    MinorContext(core::cell::UnsafeCell::new(
        [core::mem::MaybeUninit::uninit(); TRAP_BYTES],
    ))
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
        "li t0, {trap_top}",
        "add t0, a1, t0",
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
        trap_top = const core::mem::offset_of!(minor::Minor, context) + TRAP_BYTES,
        loaded = sym minor::arrive,
    );
}

impl minor::Minor {
    pub(crate) fn trap_stack(&self) -> usize {
        self.context
            .0
            .get()
            .cast::<u8>()
            .wrapping_add(TRAP_BYTES)
            .addr()
    }
    pub(in crate::metal) fn established(&self) -> bool {
        super::established_on(self.trap_stack())
    }
}
