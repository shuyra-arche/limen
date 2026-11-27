//! AArch64 cooperative call boundary and base FP/SIMD state.

use core::{
    arch::{asm, naked_asm},
    mem::offset_of,
};

use crate::metal::processor::{execution::profile::aarch64::base_simd, executor::aarch64::native};

#[cfg(target_feature = "sve")]
compile_error!("cooperative AArch64 requires the base FP/SIMD execution profile");

pub(in crate::metal::processor) fn ready() -> bool {
    if !native::masked_stack() {
        return false;
    }
    let access: usize;
    unsafe { asm!("mrs {}, CPACR_EL1", out(reg) access, options(nomem, nostack, preserves_flags)) };
    base_simd(access)
}

#[repr(C, align(16))]
pub(in crate::metal::processor) struct Saved {
    resume: usize,
    stack: usize,
    preserved: [usize; 11],
    fpcr: u64,
    fpsr: u64,
    vectors: [u128; 8],
}

impl Saved {
    pub(in crate::metal::processor) const EMPTY: Self = Self {
        resume: 0,
        stack: 0,
        preserved: [0; 11],
        fpcr: 0,
        fpsr: 0,
        vectors: [0; 8],
    };

    pub(in crate::metal::processor) fn initial(
        stack: usize,
        entry: extern "C" fn(*const ()) -> !,
    ) -> Self {
        let mut saved = Self {
            resume: (first as *const ()).addr(),
            stack,
            ..Self::EMPTY
        };
        saved.preserved[0] = (entry as *const ()).addr();
        saved
    }
}

#[unsafe(naked)]
unsafe extern "C" fn first() -> ! {
    naked_asm!("blr x19", "brk #0")
}

/// The ordinary call ABI preserves X19-X29, SP, LR and the required
/// portions of V8-V15. Save full Q8-Q15 and FP controls. X18 and TPIDR_EL1
/// remain part of the actual platform/Hart environment.
#[unsafe(naked)]
pub(in crate::metal::processor) unsafe extern "C" fn switch(
    outgoing: *mut Saved,
    incoming: *const Saved,
    argument: *const (),
) {
    naked_asm!(
        "str x30, [x0, #{resume}]",
        "mov x9, sp",
        "str x9, [x0, #{stack}]",
        "stp x19, x20, [x0, #{preserved}]",
        "stp x21, x22, [x0, #{preserved} + 16]",
        "stp x23, x24, [x0, #{preserved} + 32]",
        "stp x25, x26, [x0, #{preserved} + 48]",
        "stp x27, x28, [x0, #{preserved} + 64]",
        "str x29, [x0, #{preserved} + 80]",
        "mrs x9, FPCR",
        "mrs x10, FPSR",
        "str x9, [x0, #{fpcr}]",
        "str x10, [x0, #{fpsr}]",
        "stp q8, q9, [x0, #{vectors}]",
        "stp q10, q11, [x0, #{vectors} + 32]",
        "stp q12, q13, [x0, #{vectors} + 64]",
        "stp q14, q15, [x0, #{vectors} + 96]",
        "ldr x30, [x1, #{resume}]",
        "ldr x9, [x1, #{stack}]",
        "mov sp, x9",
        "ldp x19, x20, [x1, #{preserved}]",
        "ldp x21, x22, [x1, #{preserved} + 16]",
        "ldp x23, x24, [x1, #{preserved} + 32]",
        "ldp x25, x26, [x1, #{preserved} + 48]",
        "ldp x27, x28, [x1, #{preserved} + 64]",
        "ldr x29, [x1, #{preserved} + 80]",
        "ldr x9, [x1, #{fpcr}]",
        "ldr x10, [x1, #{fpsr}]",
        "msr FPCR, x9",
        "msr FPSR, x10",
        "ldp q8, q9, [x1, #{vectors}]",
        "ldp q10, q11, [x1, #{vectors} + 32]",
        "ldp q12, q13, [x1, #{vectors} + 64]",
        "ldp q14, q15, [x1, #{vectors} + 96]",
        "mov x0, x2",
        "ret",
        resume = const offset_of!(Saved, resume),
        stack = const offset_of!(Saved, stack),
        preserved = const offset_of!(Saved, preserved),
        fpcr = const offset_of!(Saved, fpcr),
        fpsr = const offset_of!(Saved, fpsr),
        vectors = const offset_of!(Saved, vectors),
    )
}

/// Enter the resident dispatcher with this target's existing kernel ABI.
#[unsafe(naked)]
pub(in crate::metal::processor) unsafe extern "C" fn start_stack(
    stack: usize,
    entry: extern "C" fn(*const ()) -> !,
    argument: *const (),
) -> ! {
    core::arch::naked_asm!("mov sp, x0", "mov x0, x2", "mov x30, xzr", "br x1");
}
