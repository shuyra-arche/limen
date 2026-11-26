//! RV64 cooperative continuation transfer within the supervisor address space.

use core::{
    arch::{asm, global_asm},
    cell::UnsafeCell,
    mem::offset_of,
};

/// The native integer-only switch is valid in the verified Bare environment.
pub(crate) fn cooperative_ready() -> bool {
    if !crate::metal::boot::established() {
        return false;
    }
    let status: usize;
    unsafe { asm!("csrr {}, sstatus", out(reg) status, options(nomem, nostack)) };
    const EXTENSIONS: usize = (3 << 13) | (3 << 9);
    status & EXTENSIONS == 0
}

#[repr(C)]
struct Registers {
    resume: usize,
    stack: usize,
    preserved: [usize; 12],
}

impl Registers {
    const EMPTY: Self = Self {
        resume: 0,
        stack: 0,
        preserved: [0; 12],
    };
}

global_asm!(
    include_str!("switch.S"),
    resume = const offset_of!(Registers, resume),
    stack = const offset_of!(Registers, stack),
    preserved = const offset_of!(Registers, preserved),
);

unsafe extern "C" {
    fn limen_switch_context(
        outgoing: *mut Registers,
        incoming: *const Registers,
        argument: *const (),
    );
}

pub(crate) struct Context(UnsafeCell<Registers>);

impl Context {
    pub(crate) const fn new() -> Self {
        Self(UnsafeCell::new(Registers::EMPTY))
    }

    /// Requires exclusive use of an inactive context and a live stack extent.
    pub(crate) unsafe fn prepare(&self, stack: usize, entry: extern "C" fn(*const ()) -> !) {
        unsafe {
            self.0.get().write(Registers {
                resume: (entry as *const ()).addr(),
                stack,
                ..Registers::EMPTY
            });
        }
    }

    /// Requires an inactive context with no permitted future entry.
    pub(crate) unsafe fn retire(&self) {
        unsafe { self.0.get().write(Registers::EMPTY) };
    }

    // These are normal call boundaries. Integer caller-saved registers can be
    // discarded, gp and tp remain Hart-local, and FS/VS stay disabled.
    pub(in crate::metal::execution) unsafe fn switch(
        &self,
        incoming: &Context,
        argument: *const (),
    ) {
        unsafe { limen_switch_context(self.0.get(), incoming.0.get(), argument) };
    }
}
