use crate::metal::platform::boot::treaty::MajorMaterial;
use crate::metal::platform::firmware::uefi::Loader;
use crate::metal::processor::{
    description::aarch64::status, execution::terminal::aarch64::vectors,
};
use core::arch::naked_asm;

/// Establish the major CPU stack and exception vectors before delivering Boot.
///
/// # Safety
/// Call once on the boot PE, in non-secure EL1 with the image addressable.
/// Other PEs must stay parked. The Loader and its immutable buffers must remain valid.
#[unsafe(naked)]
pub unsafe extern "C" fn major(
    processor: usize,
    input: &Loader<'_>,
    stack_top: usize,
    material: &'static MajorMaterial,
) -> ! {
    naked_asm!(
        "mrs x4, CurrentEL",
        "cmp x4, #{level}",
        "b.ne 2f",
        "msr daifset, #{exceptions}",
        "msr spsel, #{stack_select}",
        "msr tpidr_el1, xzr",
        "mov sp, x2",
        "adrp x4, {vectors}",
        "add x4, x4, :lo12:{vectors}",
        "msr vbar_el1, x4",
        "isb",
        "b {loaded}",
        "2: wfe",
        "b 2b",
        level = const status::EL1,
        exceptions = const status::ALL_EXCEPTIONS,
        stack_select = const status::SP_EL1,
        vectors = sym vectors,
        loaded = sym super::super::major::start,
    );
}
