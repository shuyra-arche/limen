use super::{status, vectors};
use crate::metal::platform::boot::{STACK_BYTES, major::STACK};
use crate::metal::platform::firmware::uefi::Loader;
use core::arch::naked_asm;

/// Establish the major CPU stack and exception vectors before delivering Boot.
///
/// # Safety
/// Call once on the boot PE, in non-secure EL1 with the image addressable.
/// Other PEs must stay parked. The Loader and its immutable buffers must remain valid.
#[unsafe(naked)]
pub unsafe extern "C" fn major(hart: usize, input: &Loader<'_>) -> ! {
    naked_asm!(
        "mrs x2, CurrentEL",
        "cmp x2, #{level}",
        "b.ne 2f",
        "msr daifset, #{exceptions}",
        "msr spsel, #{stack_select}",
        "msr tpidr_el1, xzr",
        "adrp x2, {stack}",
        "add x2, x2, :lo12:{stack}",
        "add sp, x2, {size}",
        "adrp x2, {vectors}",
        "add x2, x2, :lo12:{vectors}",
        "msr vbar_el1, x2",
        "isb",
        "b {loaded}",
        "2: wfe",
        "b 2b",
        level = const status::EL1,
        exceptions = const status::ALL_EXCEPTIONS,
        stack_select = const status::SP_EL1,
        vectors = sym vectors,
        loaded = sym crate::entry::boot::start,
        stack = sym STACK,
        size = const STACK_BYTES,
    );
}
