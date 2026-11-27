//! RV64 cooperative continuation transfer within the supervisor address space.

use core::{arch::global_asm, mem::offset_of};

use crate::metal::{
    interrupt::riscv64::internal::supervisor, memory::mmu::riscv64::internal::context::Context,
    processor::executor::riscv64::native,
};

/// The native integer-only switch is valid in the verified Bare environment.
pub(in crate::metal::processor) fn ready() -> bool {
    let status = native::status();
    status.masked()
        && status.extensions_off()
        && supervisor::all_masked()
        && Context::current() == Some(Context::BARE)
}

#[repr(C)]
pub(in crate::metal::processor) struct Saved {
    resume: usize,
    stack: usize,
    preserved: [usize; 12],
}

impl Saved {
    pub(in crate::metal::processor) const EMPTY: Self = Self {
        resume: 0,
        stack: 0,
        preserved: [0; 12],
    };

    pub(in crate::metal::processor) fn initial(
        stack: usize,
        entry: extern "C" fn(*const ()) -> !,
    ) -> Self {
        Self {
            resume: (entry as *const ()).addr(),
            stack,
            ..Self::EMPTY
        }
    }
}

global_asm!(
    include_str!("switch.S"),
    resume = const offset_of!(Saved, resume),
    stack = const offset_of!(Saved, stack),
    preserved = const offset_of!(Saved, preserved),
);

unsafe extern "C" {
    fn limen_switch_context(outgoing: *mut Saved, incoming: *const Saved, argument: *const ());
}

/// The real psABI call boundary; gp and tp remain Hart-local.
pub(in crate::metal::processor) unsafe fn switch(
    outgoing: *mut Saved,
    incoming: *const Saved,
    argument: *const (),
) {
    unsafe { limen_switch_context(outgoing, incoming, argument) };
}

/// Enter the resident dispatcher with this target's existing kernel ABI.
#[unsafe(naked)]
pub(in crate::metal::processor) unsafe extern "C" fn start_stack(
    stack: usize,
    entry: extern "C" fn(*const ()) -> !,
    argument: *const (),
) -> ! {
    core::arch::naked_asm!("mv sp, a0", "mv a0, a2", "li ra, 0", "jr a1");
}
