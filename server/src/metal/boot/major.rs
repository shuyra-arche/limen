//! Admit the major CPU after the selected backend installs its stack and exception entry.
use super::STACK_BYTES;
use crate::{
    entry::boot::Boot,
    metal::{
        boot,
        hart::{self, stack::Stack},
    },
};
use core::ops::Range;

pub(in crate::metal) static mut STACK: Stack<STACK_BYTES> = Stack::new();

/// Image-owned stack reservation for the major CPU.
pub fn stack_range() -> Range<usize> {
    let start = (&raw const STACK).addr();
    start..start + STACK_BYTES
}

/// # Safety
/// Run once on the major CPU after establishing its stack and exception entry.
/// The input must satisfy the selected firmware ABI and its referenced storage
/// must remain readable and reserved for the lifetime of the delivered Boot.
pub(crate) unsafe fn admit<'a>(hart: usize, input: *const ()) -> Boot<'a> {
    let services = boot::firmware::services_on_entry();
    if !stack_range().contains(&hart::stack_pointer()) || !boot::established() {
        services.reject(format_args!(
            "FAIL Limen boot: invalid CPU or firmware handoff\n"
        ));
    }

    if hart != hart::current_hart() {
        services.reject(format_args!(
            "FAIL Limen boot: major CPU identity mismatch\n"
        ));
    }
    let Some(boot) = (unsafe { boot::firmware::admit(hart, input) }) else {
        services.reject(format_args!("FAIL Limen boot: invalid firmware input\n"));
    };
    boot
}
