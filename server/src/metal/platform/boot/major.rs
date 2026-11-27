//! Admit the major CPU after the selected backend installs its stack and exception entry.
use super::STACK_BYTES;
use crate::metal::{
    platform::boot,
    platform::boot::handoff::{Descriptions, Facts},
    processor::hart::local::{self, Storage, stack::Stack},
    sync::TURN_CAPACITY,
};
use core::ops::Range;

#[cfg(target_arch = "riscv64")]
use crate::metal::interrupt::riscv64::internal::supervisor::Supervisor;

pub(in crate::metal) static mut STACK: Stack<STACK_BYTES> = Stack::new();
static LOCAL: Storage<TURN_CAPACITY> = Storage::new();

/// Original firmware facts alongside the hardware use admitted by Metal.
pub(crate) struct Admission<'a> {
    pub(crate) hart: usize,
    pub(crate) facts: Facts<'a>,
    #[cfg(target_arch = "riscv64")]
    pub(crate) reception: Supervisor,
}

/// Image-owned stack reservation for the major CPU.
pub fn stack_range() -> Range<usize> {
    let start = (&raw const STACK).addr();
    start..start + STACK_BYTES
}

/// # Safety
/// Run once on the major CPU after establishing its stack and exception entry.
/// The input must satisfy the selected firmware ABI and its referenced storage
/// must remain readable and reserved for the lifetime of the delivered Boot.
pub(crate) unsafe fn admit<'a>(
    hart: usize,
    input: *const (),
    descriptions: &mut Descriptions<'a>,
) -> Admission<'a> {
    let services = boot::firmware::services_on_entry();
    if !stack_range().contains(&local::stack_pointer()) || !boot::established() {
        services.reject(format_args!(
            "FAIL Limen boot: invalid CPU or firmware handoff\n"
        ));
    }

    let Some(facts) = (unsafe { boot::admit(hart, input, descriptions) }) else {
        services.reject(format_args!("FAIL Limen boot: invalid firmware input\n"));
    };
    let Some(entry) = boot::entry() else {
        services.reject(format_args!(
            "FAIL Limen boot: entry changed before Hart admission\n"
        ));
    };
    unsafe { LOCAL.bind().install(hart, entry) };
    if !local::can_switch() {
        services.reject(format_args!(
            "FAIL Limen boot: local protection is not idle\n"
        ));
    }
    #[cfg(target_arch = "riscv64")]
    // The actual Hart and its ordinary stack are installed before using CSRs.
    let reception = unsafe { Supervisor::admit(hart as u64) }
        .unwrap_or_else(|_| services.reject(format_args!("FAIL Limen boot: local reception\n")));
    Admission {
        hart,
        facts,
        #[cfg(target_arch = "riscv64")]
        reception,
    }
}
