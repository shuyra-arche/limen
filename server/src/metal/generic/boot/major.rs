//! Admit the major CPU after the selected backend installs its stack and exception entry.
use crate::{entry::boot::Boot, metal::boot};
use core::ops::Range;

/// Image-owned stack reservation for the major CPU.
pub fn stack_range() -> Range<usize> {
    let start = (&raw const boot::STACK).addr();
    start..start + boot::STACK_BYTES
}

/// # Safety
/// Run once on the major CPU after establishing its stack and exception entry.
/// The input must satisfy the selected firmware ABI and its referenced storage
/// must remain readable and reserved for the lifetime of the delivered Boot.
pub(crate) unsafe fn admit<'a>(hart: usize, input: *const ()) -> Boot<'a> {
    let services = boot::firmware::services_on_entry();
    if !stack_range().contains(&boot::stack_pointer()) || !boot::established() {
        services.reject(format_args!(
            "FAIL Limen boot: invalid CPU or firmware handoff\n"
        ));
    }

    if hart != boot::current_hart() {
        services.reject(format_args!(
            "FAIL Limen boot: major CPU identity mismatch\n"
        ));
    }
    let Some(boot) = (unsafe { boot::firmware::admit(hart, input) }) else {
        services.reject(format_args!("FAIL Limen boot: invalid firmware input\n"));
    };
    boot
}
