use super::Continuation;
use crate::metal::stack::{STACK, Stack};
use core::arch::naked_asm;

#[unsafe(naked)]
#[rustc_align(2048)]
#[unsafe(export_name = "limen_vectors")]
pub(super) unsafe extern "C" fn vectors() -> ! {
    naked_asm!(
        ".rept 16",
        "b 2f",
        ".balign 128",
        ".endr",
        "2: msr daifset, #15",
        "3: wfe",
        "b 3b",
    );
}

/// Enter the diagnostic callback on the boot stack with masked exceptions
/// and a fail-stop vector table. Preserve the loader's input word.
///
/// # Safety
/// Call once on the boot PE, in non-secure EL1 with the image addressable.
/// Other PEs must stay parked. The callback and its input must remain valid.
#[unsafe(naked)]
pub unsafe extern "C" fn enter(input: usize, next: Continuation) -> ! {
    naked_asm!(
        "mrs x2, CurrentEL",
        "cmp x2, #4",
        "b.ne 2f",
        "msr daifset, #15",
        "msr spsel, #1",
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
        vectors = sym vectors,
        loaded = sym crate::entry::boot::loaded,
        stack = sym STACK,
        size = const size_of::<Stack>(),
    );
}
