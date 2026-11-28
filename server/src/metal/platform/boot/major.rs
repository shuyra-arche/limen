//! Admit the major CPU after the selected backend installs its stack and exception entry.
use crate::metal::{
    platform::boot::{
        self,
        handoff::{Descriptions, Facts, identity_extent},
        treaty::{self, MajorMaterial},
    },
    processor::{
        execution::terminal::trap,
        executor::entry,
        treaty::{ProcessorId, Terminal, can_switch},
    },
};
use core::ops::Range;
use limen_treaty::model::PhysicalRange;

#[cfg(target_arch = "riscv64")]
use crate::metal::interrupt::treaty::Supervisor;

/// Original firmware facts alongside the hardware use admitted by Metal.
pub(crate) struct Admission<'a> {
    pub(crate) processor: ProcessorId,
    pub(crate) terminal: Terminal,
    pub(crate) stack: Range<usize>,
    pub(crate) facts: Facts<'a>,
    pub(crate) descriptions: &'a Descriptions<'a>,
    physical_stacks: [PhysicalRange; 2],
    #[cfg(limen_cooperative)]
    pub(crate) major: crate::metal::processor::treaty::Major,
    #[cfg(target_arch = "riscv64")]
    pub(crate) reception: Supervisor,
}

impl Admission<'_> {
    pub(crate) fn physical_stacks(&self) -> [PhysicalRange; 2] {
        self.physical_stacks
    }
}

/// # Safety
/// Run once on the major CPU after establishing its stack and exception entry.
/// The selected firmware ABI must retain its readable inputs and preserve the
/// trusted identity map for the resident image, entry stacks and buffers.
pub(super) unsafe extern "C" fn start(
    processor: usize,
    input: *const (),
    stack_top: usize,
    material: &'static MajorMaterial,
) -> ! {
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

    let Some((facts, descriptions)) = material
        .descriptions
        .retain(|descriptions| unsafe { boot::admit(processor, input, descriptions) })
    else {
        terminal.reject(format_args!(
            "FAIL Limen boot: invalid or repeated firmware input\n"
        ));
    };
    let physical_stacks = [stack.clone(), trap::stack_range()].map(|range| {
        unsafe { identity_extent(range) }
            .filter(|physical| facts.image.covers(*physical))
            .unwrap_or_else(|| {
                terminal.reject(format_args!(
                    "FAIL Limen boot: stacks outside physical image\n"
                ))
            })
    });
    if !unsafe { identity_extent(material.descriptions.range()) }.is_some_and(|physical| {
        facts.image.covers(physical)
            && physical_stacks
                .iter()
                .all(|stack| !stack.overlaps(physical))
    }) {
        terminal.reject(format_args!(
            "FAIL Limen boot: descriptions outside resident storage\n"
        ));
    }
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
    let reception = Supervisor::take()
        .unwrap_or_else(|_| terminal.reject(format_args!("FAIL Limen boot: local reception\n")));
    treaty::deliver(Admission {
        processor: ProcessorId::new(processor),
        terminal,
        stack,
        facts,
        descriptions,
        physical_stacks,
        #[cfg(limen_cooperative)]
        major: unsafe { local.major() },
        #[cfg(target_arch = "riscv64")]
        reception,
    })
}
