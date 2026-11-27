use super::{self as launch, Minor, StartError};
use crate::metal::platform::firmware::treaty::sbi::HartService;
use crate::metal::processor::{
    execution::terminal::{riscv64::trap, trap as diagnostic},
    executor::entry::riscv64::establish_supervisor,
};
use core::arch::naked_asm;

pub enum Startup {
    Sbi(HartService),
}
pub type DeliveryError = core::convert::Infallible;

pub(super) struct MinorContext;
pub(super) const fn minor_context() -> MinorContext {
    MinorContext
}

pub(super) unsafe fn wake_minor(
    slot: &Minor,
    hart: usize,
    Startup::Sbi(service): &mut Startup,
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
        interrupt_enable = const crate::metal::processor::description::riscv64::status::SIE,
        extensions = const crate::metal::processor::description::riscv64::status::WRITABLE_EXTENSIONS,
        vector = sym trap,
        state = const launch::STATE_OFFSET,
        published = const launch::PUBLISHED,
        entered = const launch::ENTERED,
        hart = const launch::PROCESSOR_OFFSET,
        stack = const launch::STACK_OFFSET,
        top = sym diagnostic::TOP,
        loaded = sym launch::arrive,
    );
}
