//! Admit the major CPU after the selected backend installs its stack and exception entry.
use crate::metal::{
    platform::boot::{
        self,
        handoff::{Descriptions, Facts},
        treaty::MajorMaterial,
    },
    processor::{
        executor::entry,
        treaty::{Terminal, can_switch},
    },
};
use core::ops::Range;

#[cfg(target_arch = "riscv64")]
use crate::metal::interrupt::riscv64::internal::supervisor::Supervisor;

/// Original firmware facts alongside the hardware use admitted by Metal.
pub(crate) struct Admission<'a> {
    pub(crate) processor: usize,
    pub(crate) terminal: Terminal,
    pub(crate) stack: Range<usize>,
    pub(crate) facts: Facts<'a>,
    #[cfg(limen_cooperative)]
    pub(crate) major: crate::metal::processor::treaty::Major,
    #[cfg(target_arch = "riscv64")]
    pub(crate) reception: Supervisor,
}

/// # Safety
/// Run once on the major CPU after establishing its stack and exception entry.
/// The input must satisfy the selected firmware ABI and its referenced storage
/// must remain readable and reserved for the lifetime of the delivered Boot.
pub(crate) unsafe fn admit<'a>(
    processor: usize,
    input: *const (),
    stack_top: usize,
    material: &'static MajorMaterial,
    descriptions: &mut Descriptions<'a>,
) -> Admission<'a> {
    let terminal = boot::firmware::terminal_on_entry();
    let stack = material.stack.range();
    if stack.end != stack_top || entry::admit(stack.clone()).is_none() {
        terminal.reject(format_args!(
            "FAIL Limen boot: invalid CPU or firmware handoff\n"
        ));
    }

    #[cfg(target_arch = "riscv64")]
    if boot::linked_stack() != stack {
        terminal.reject(format_args!(
            "FAIL Limen boot: invalid linked stack bounds\n"
        ));
    }

    let Some(facts) = (unsafe { boot::admit(processor, input, descriptions) }) else {
        terminal.reject(format_args!("FAIL Limen boot: invalid firmware input\n"));
    };
    let Some(entry) = entry::admit(stack.clone()) else {
        terminal.reject(format_args!(
            "FAIL Limen boot: entry changed before processor admission\n"
        ));
    };
    let local = unsafe { material.local.bind() };
    unsafe { local.install(processor, entry) };
    if !can_switch() {
        terminal.reject(format_args!(
            "FAIL Limen boot: local protection is not idle\n"
        ));
    }
    #[cfg(target_arch = "riscv64")]
    // The actual hart and its ordinary stack are installed before using CSRs.
    let reception = unsafe { Supervisor::admit(processor as u64) }
        .unwrap_or_else(|_| terminal.reject(format_args!("FAIL Limen boot: local reception\n")));
    Admission {
        processor,
        terminal,
        stack,
        facts,
        #[cfg(limen_cooperative)]
        major: unsafe { local.major() },
        #[cfg(target_arch = "riscv64")]
        reception,
    }
}
