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
    r#"
.section .text.execution,"ax"
.balign 4
.globl limen_switch_context
.type limen_switch_context, @function
limen_switch_context:
    sd ra, {resume}(a0)
    sd sp, {stack}(a0)
    sd s0, {preserved}+0(a0)
    sd s1, {preserved}+8(a0)
    sd s2, {preserved}+16(a0)
    sd s3, {preserved}+24(a0)
    sd s4, {preserved}+32(a0)
    sd s5, {preserved}+40(a0)
    sd s6, {preserved}+48(a0)
    sd s7, {preserved}+56(a0)
    sd s8, {preserved}+64(a0)
    sd s9, {preserved}+72(a0)
    sd s10, {preserved}+80(a0)
    sd s11, {preserved}+88(a0)

    ld ra, {resume}(a1)
    ld sp, {stack}(a1)
    ld s0, {preserved}+0(a1)
    ld s1, {preserved}+8(a1)
    ld s2, {preserved}+16(a1)
    ld s3, {preserved}+24(a1)
    ld s4, {preserved}+32(a1)
    ld s5, {preserved}+40(a1)
    ld s6, {preserved}+48(a1)
    ld s7, {preserved}+56(a1)
    ld s8, {preserved}+64(a1)
    ld s9, {preserved}+72(a1)
    ld s10, {preserved}+80(a1)
    ld s11, {preserved}+88(a1)
    mv a0, a2
    ret
.size limen_switch_context, .-limen_switch_context
"#,
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
