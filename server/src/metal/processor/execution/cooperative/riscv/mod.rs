//! RV64 cooperative continuation transfer within the supervisor address space.

use core::{
    arch::{asm, global_asm},
    mem::offset_of,
};

/// The native integer-only switch is valid in the verified Bare environment.
pub(super) fn ready() -> bool {
    let status: usize;
    unsafe { asm!("csrr {}, sstatus", out(reg) status, options(nomem, nostack)) };
    const EXTENSIONS: usize = (3 << 13) | (3 << 9);
    status & EXTENSIONS == 0
}

#[repr(C)]
pub(super) struct Saved {
    resume: usize,
    stack: usize,
    preserved: [usize; 12],
}

impl Saved {
    pub(super) const EMPTY: Self = Self {
        resume: 0,
        stack: 0,
        preserved: [0; 12],
    };

    pub(super) fn initial(stack: usize, entry: extern "C" fn(*const ()) -> !) -> Self {
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
pub(super) unsafe fn switch(outgoing: *mut Saved, incoming: *const Saved, argument: *const ()) {
    unsafe { limen_switch_context(outgoing, incoming, argument) };
}
